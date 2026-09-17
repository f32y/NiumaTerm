//! Moving a tab onto another conversation without holding up its thread.
//!
//! Opening a conversation is a call plus a stream handshake, together up to
//! tens of seconds against a busy host. Both run as one command here, and the
//! opened streams wait in a slot until the session takes them over while
//! processing the frame that announces them.

#[cfg(test)]
#[path = "switch_tests.rs"]
mod switch_tests;

use std::sync::{Arc, Weak};

use parking_lot::Mutex;
use serde_json::{Value, json};

use crate::dsh::api::ApiClient;
use crate::dsh::events::Downlinks;
use crate::dsh::host::Host;
use crate::dsh::session::{OpenedConversation, open_conversation, settled};

/// An opened conversation on its way to the session.
pub(super) struct Switch {
    pub(super) opened: OpenedConversation,
    pub(super) downlinks: Downlinks,
    pub(super) snapshot: Value,
}

pub(super) type SwitchSlot = Arc<Mutex<Option<Switch>>>;

/// Which conversation a change leads to.
pub(super) enum Target {
    Existing(String),
    /// A copy the harness cuts first. Omitting the seq falls back to the last
    /// completed turn.
    BranchOf {
        session_id: String,
        at_seq: Option<u64>,
    },
}

/// What a conversation change works with, all of it owned so the change can
/// outlive the call that asked for it.
pub(super) struct Switching {
    pub(super) client: ApiClient,
    pub(super) host: Weak<Host>,
    pub(super) cwd: Option<String>,

    /// The conversation the tab is on, which a failure is reported against.
    pub(super) current: String,

    pub(super) deliver: Arc<dyn Fn(Value) + Send + Sync>,
    pub(super) slot: SwitchSlot,
}

/// Open `target` and hand it to the session, or report why it stayed put.
///
/// The new streams must open before the old ones are released, so a refused
/// change leaves the current conversation usable.
pub(super) async fn switch_conversation(switching: Switching, target: Target) {
    let Switching {
        client,
        host,
        cwd,
        current,
        deliver,
        slot,
    } = switching;

    // The new streams report as soon as they open, ahead of the announcement
    // that makes the session accept their conversation. Their frames are held
    // until it is out, or the session would drop them as another tab's.
    let held = Arc::new(Mutex::new(Some(Vec::new())));

    let gate: Arc<dyn Fn(Value) + Send + Sync> = {
        let held = Arc::clone(&held);
        let deliver = Arc::clone(&deliver);

        Arc::new(move |frame| {
            let mut held = held.lock();

            match held.as_mut() {
                Some(frames) => frames.push(frame),
                None => {
                    drop(held);

                    deliver(frame);
                }
            }
        })
    };

    let opened = async {
        let thread_id = match target {
            Target::Existing(thread_id) => thread_id,
            Target::BranchOf { session_id, at_seq } => {
                let mut payload = json!({ "sessionId": session_id });

                if let Some(at_seq) = at_seq {
                    payload["atSeq"] = json!(at_seq);
                }

                let forked = client
                    .request("session/fork", payload)
                    .await
                    .map_err(|error| error.message().to_string())?;

                forked["sessionId"]
                    .as_str()
                    .ok_or_else(|| "the harness answered without a conversation id".to_string())?
                    .to_string()
            }
        };

        let opened = open_conversation(&client, cwd.as_deref(), Some(&thread_id), None).await?;

        let (downlinks, snapshot) =
            Downlinks::open(client.clone(), host, opened.session_id.clone(), gate).await?;

        Ok::<_, String>(Switch {
            opened,
            downlinks,
            snapshot,
        })
    }
    .await;

    match opened {
        Ok(switch) => {
            let announced = switch.opened.session_id.clone();

            // Held across the release so a frame arriving meanwhile queues
            // behind the ones already waiting instead of overtaking them.
            let mut held = held.lock();

            *slot.lock() = Some(switch);

            deliver(settled(&announced, json!({ "kind": "switched" })));

            for frame in held.take().into_iter().flatten() {
                deliver(frame);
            }
        }
        Err(error) => {
            tracing::warn!("deepseek could not change conversations: {error}");

            deliver(settled(
                &current,
                json!({ "kind": "switchFailed", "error": error }),
            ));
        }
    }
}
