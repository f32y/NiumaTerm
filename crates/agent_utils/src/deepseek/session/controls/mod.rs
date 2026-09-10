//! Bounded background execution of interactive control calls.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::deepseek::api::ApiClient;
use crate::deepseek::mapping::{ApprovalRequest, QuestionRequest};
use crate::message_memory::retained_bytes;

mod results;

#[cfg(test)]
mod tests;

pub(super) const COMPLETED_FRAME: &str = "nmt/control-completed";
const MAX_PENDING: usize = 32;
const MAX_CALL_BYTES: usize = 32 * 1024;
const CALL_DEADLINE: Duration = Duration::from_secs(15);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Operation {
    Approval(ApprovalRequest),
    Questions {
        request: QuestionRequest,
        skipped: bool,
    },
    Interrupt,
    InterruptChild(String),
}

struct Pending {
    operation: Operation,
    cancelled: Arc<AtomicBool>,
}

impl Drop for Pending {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

struct Call {
    id: u64,
    method: &'static str,
    args: Value,
    cancel_after: Option<String>,
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
}

pub(super) struct Controls {
    sender: mpsc::SyncSender<Call>,
    pending: HashMap<u64, Pending>,
    next_id: u64,
}

impl Controls {
    pub(super) fn new(
        client: ApiClient,
        deliver: Arc<dyn Fn(Value) + Send + Sync>,
    ) -> Result<Self, String> {
        let (sender, receiver) = mpsc::sync_channel::<Call>(MAX_PENDING);
        thread::Builder::new()
            .name("deepseek-controls".to_string())
            .spawn(move || {
                for call in receiver {
                    if call.cancelled.load(Ordering::Acquire) {
                        continue;
                    }
                    let result = match call.deadline.checked_duration_since(Instant::now()) {
                        Some(timeout) => client
                            .call_with_timeout(call.method, call.args, timeout)
                            .map(|_| ())
                            .map_err(|error| format!(
                                "DeepSeek control failed; a transport failure may have an unknown outcome: {}",
                                error.message()
                            )),
                        None => Err("DeepSeek control expired before sending; please retry.".to_string()),
                    };
                    if call.cancelled.load(Ordering::Acquire) {
                        continue;
                    }
                    // Refusal and stopping are separate outcomes: a failed stop
                    // cannot make an accepted refusal retryable again.
                    let stop_error = if result.is_ok() && let Some(session_id) = call.cancel_after {
                        match call.deadline.checked_duration_since(Instant::now()) {
                            Some(timeout) => client.call_with_timeout(
                                "session/cancel", json!({"request": {"sessionId": session_id}}), timeout,
                            ).err().map(|error| error.message().to_string()),
                            None => Some("The approval was refused, but the stop expired before sending.".to_string()),
                        }
                    } else { None };
                    if !call.cancelled.load(Ordering::Acquire) {
                        (deliver)(json!({"payload": {
                            "type": COMPLETED_FRAME, "id": call.id,
                            "error": result.as_ref().err(), "stopError": stop_error,
                        }}));
                    }
                }
            })
            .map_err(|error| format!("could not start DeepSeek control worker: {error}"))?;
        Ok(Self {
            sender,
            pending: HashMap::new(),
            next_id: 0,
        })
    }

    pub(super) fn submit(
        &mut self,
        operation: Operation,
        method: &'static str,
        args: Value,
        cancel_after: Option<String>,
    ) -> bool {
        // Capacity includes calls awaiting UI consumption, not just queued HTTP work.
        if self.pending.len() >= MAX_PENDING
            || retained_bytes(&args)
                .saturating_add(cancel_after.as_ref().map_or(0, String::capacity))
                > MAX_CALL_BYTES
            || self
                .pending
                .values()
                .any(|pending| match (&pending.operation, &operation) {
                    (
                        Operation::Questions { request: left, .. },
                        Operation::Questions { request: right, .. },
                    ) => left == right,
                    _ => pending.operation == operation,
                })
        {
            return false;
        }
        let Some(id) = self.next_id.checked_add(1) else {
            return false;
        };
        self.next_id = id;
        let cancelled = Arc::new(AtomicBool::new(false));
        let call = Call {
            id,
            method,
            args,
            cancel_after,
            deadline: Instant::now() + CALL_DEADLINE,
            cancelled: Arc::clone(&cancelled),
        };
        if self.sender.try_send(call).is_err() {
            return false;
        }
        self.pending.insert(
            id,
            Pending {
                operation,
                cancelled,
            },
        );
        true
    }

    pub(super) fn complete(&mut self, id: u64) -> Option<Operation> {
        self.pending
            .remove(&id)
            .map(|pending| pending.operation.clone())
    }

    pub(super) fn clear(&mut self) {
        self.pending.clear();
    }

    pub(super) fn retire_approval(&mut self) {
        self.pending
            .retain(|_, pending| !matches!(pending.operation, Operation::Approval(_)));
    }

    pub(super) fn retire_questions(&mut self) {
        self.pending
            .retain(|_, pending| !matches!(pending.operation, Operation::Questions { .. }));
    }

    pub(super) fn retire_interrupt(&mut self) {
        self.pending
            .retain(|_, pending| !matches!(pending.operation, Operation::Interrupt));
    }
}

pub(super) fn question_id(request: &QuestionRequest) -> String {
    json!([request.client_id, request.event_id]).to_string()
}
