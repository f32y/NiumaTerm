//! Early traffic has a shared byte budget before its owning tab is known.

use std::collections::hash_map::RandomState;
use std::collections::{HashMap, VecDeque};
use std::hash::BuildHasher as _;
use std::mem::size_of;

use serde_json::Value;

use crate::message_memory::retained_bytes;

#[cfg(test)]
mod tests;

const MAX_THREADS: usize = 64;
const MAX_MESSAGES: usize = 32;
const MAX_THREAD_BYTES: usize = 2 * 1024 * 1024;
const MAX_BYTES: usize = 16 * 1024 * 1024;
const LOSS_WORDS: usize = 128;

struct Message {
    value: Value,
    bytes: usize,
}

struct Thread {
    messages: VecDeque<Message>,
    bytes: usize,
    overhead: usize,
}

pub(super) struct Replay {
    pub(super) messages: Vec<Value>,
    pub(super) incomplete: bool,
}

pub(super) struct EarlyMessages {
    threads: HashMap<String, Thread>,
    order: VecDeque<String>,
    bytes: usize,
    // A fixed loss filter survives eviction without retaining arbitrary thread IDs.
    // Collisions may warn about possible loss, but cannot hide an actual drop.
    // Bits remain set for the lifetime of this host, including repeated claims.
    loss: [u64; LOSS_WORDS],
    hashes: RandomState,
}

impl Default for EarlyMessages {
    fn default() -> Self {
        Self {
            threads: HashMap::new(),
            order: VecDeque::new(),
            bytes: 0,
            loss: [0; LOSS_WORDS],
            hashes: RandomState::new(),
        }
    }
}

impl EarlyMessages {
    fn loss_bits(&self, thread_id: &str) -> [usize; 2] {
        let hash = self.hashes.hash_one(thread_id);
        [
            (hash as usize) % (LOSS_WORDS * 64),
            ((hash >> 32) as usize) % (LOSS_WORDS * 64),
        ]
    }

    fn mark_loss(&mut self, thread_id: &str) {
        for bit in self.loss_bits(thread_id) {
            self.loss[bit / 64] |= 1 << (bit % 64);
        }
    }

    fn may_have_lost(&self, thread_id: &str) -> bool {
        self.loss_bits(thread_id)
            .into_iter()
            .all(|bit| self.loss[bit / 64] & (1 << (bit % 64)) != 0)
    }

    fn evict_oldest(&mut self) {
        if let Some(thread_id) = self.order.pop_front() {
            self.mark_loss(&thread_id);
            self.remove(&thread_id);
        }
    }

    pub(super) fn hold(&mut self, thread_id: &str, value: Value) {
        let bytes = retained_bytes(&value);
        // Account for both owned keys and the full per-thread deque allocation.
        let overhead = thread_id
            .len()
            .saturating_mul(2)
            .saturating_add(MAX_MESSAGES * size_of::<Message>())
            .saturating_add(512);
        if bytes.saturating_add(overhead) > MAX_THREAD_BYTES {
            self.mark_loss(thread_id);
            return;
        }
        let mut dropped = false;
        if let Some(thread) = self.threads.get_mut(thread_id) {
            while thread.messages.len() >= MAX_MESSAGES
                || thread.bytes + overhead + bytes > MAX_THREAD_BYTES
            {
                let Some(oldest) = thread.messages.pop_front() else {
                    break;
                };
                thread.bytes -= oldest.bytes;
                self.bytes -= oldest.bytes;
                dropped = true;
            }
        }
        if dropped {
            self.mark_loss(thread_id);
        }
        loop {
            let exists = self.threads.contains_key(thread_id);
            let extra = bytes + if exists { 0 } else { overhead };
            if self.bytes + extra <= MAX_BYTES && (exists || self.threads.len() < MAX_THREADS) {
                break;
            }
            self.evict_oldest();
        }
        let thread = self.threads.entry(thread_id.to_owned()).or_insert_with(|| {
            self.order.push_back(thread_id.to_owned());
            self.bytes += overhead;
            Thread {
                messages: VecDeque::with_capacity(MAX_MESSAGES),
                bytes: 0,
                overhead,
            }
        });
        thread.messages.push_back(Message { value, bytes });
        thread.bytes += bytes;
        self.bytes += bytes;
    }

    pub(super) fn take(&mut self, thread_id: &str) -> Replay {
        let incomplete = self.may_have_lost(thread_id);
        let messages = self
            .remove(thread_id)
            .map(|thread| {
                thread
                    .messages
                    .into_iter()
                    .map(|message| message.value)
                    .collect()
            })
            .unwrap_or_default();
        Replay {
            messages,
            incomplete,
        }
    }

    fn remove(&mut self, thread_id: &str) -> Option<Thread> {
        self.order.retain(|id| id != thread_id);
        let thread = self.threads.remove(thread_id)?;
        self.bytes -= thread.bytes + thread.overhead;
        Some(thread)
    }

    pub(super) fn forget(&mut self, thread_id: &str) {
        self.remove(thread_id);
    }

    pub(super) fn clear(&mut self) {
        self.threads.clear();
        self.order.clear();
        self.bytes = 0;
    }
}
