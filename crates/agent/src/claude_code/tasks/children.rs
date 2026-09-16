//! Child conversations accumulated from the sidechain stream.
//!
//! A child agent has a conversation of its own that the parent transcript
//! never shows. The reducer forwards its content rather than retaining it, so
//! what lives here is only what a later record needs: the items observed since
//! the caller last drained, the tool calls still waiting for their results, and
//! the launch instruction already published as the opening message.

use std::collections::HashMap;
use std::mem::take;

use serde_json::Value;

use crate::background_task::{BackgroundTaskKey, BackgroundTaskTranscriptUpdate};
use crate::chat::Item;
use crate::claude_code::records::child_content_items;

#[derive(Default)]
pub(super) struct ChildTranscripts {
    /// Child conversation content observed since the caller last drained it.
    pending: Vec<(BackgroundTaskKey, BackgroundTaskTranscriptUpdate)>,

    /// Tool calls a child started, so its matching result completes the same
    /// row instead of appearing as a second one.
    open_tools: HashMap<String, Item>,

    /// Launch instructions already published as a child's opening message, by
    /// canonical id. Claude Code 2.1.2x keeps a child's conversation entirely
    /// in its own file and streams only the child's assistant output, so the
    /// launch block is the one place the live stream states what the child was
    /// asked to do. Older versions also replay that text as a sidechain user
    /// record, which `repeats_launch` recognizes as the same instruction rather
    /// than a second one.
    launch_prompts: HashMap<String, String>,
}

impl ChildTranscripts {
    pub(super) fn clear(&mut self) {
        self.pending.clear();

        self.open_tools.clear();

        self.launch_prompts.clear();
    }

    /// Publish a child's launch instruction as the opening message of its
    /// conversation, reporting whether this is the first time. A second launch
    /// block for the same call states nothing new.
    pub(super) fn open(&mut self, tool_use_id: &str, prompt: String) -> bool {
        if self.launch_prompts.contains_key(tool_use_id) {
            return false;
        }

        self.launch_prompts
            .insert(tool_use_id.to_owned(), prompt.clone());

        self.pending.push((
            BackgroundTaskKey::claude_code(tool_use_id),
            BackgroundTaskTranscriptUpdate::appended(vec![Item::UserMessage {
                text: Some(prompt),
            }]),
        ));

        true
    }

    /// Add live content to a child's conversation.
    pub(super) fn push(&mut self, canonical: &str, items: Vec<Item>) {
        if items.is_empty() {
            return;
        }

        self.pending.push((
            BackgroundTaskKey::claude_code(canonical),
            BackgroundTaskTranscriptUpdate::appended(items),
        ));
    }

    /// Offer stored history for a child. History predates whatever the live
    /// stream produced, so it fills a child nothing has been seen for and
    /// never replaces newer live content.
    pub(super) fn push_restored(&mut self, key: BackgroundTaskKey, items: Vec<Item>) {
        self.pending
            .push((key, BackgroundTaskTranscriptUpdate::restored(items)));
    }

    pub(super) fn drain(&mut self) -> Vec<(BackgroundTaskKey, BackgroundTaskTranscriptUpdate)> {
        take(&mut self.pending)
    }

    /// Transcript items for one sidechain record, using the same item shapes
    /// the parent conversation renders so a child reads identically.
    pub(super) fn sidechain_items(&mut self, canonical: &str, message: &Value) -> Vec<Item> {
        let mut items = Vec::new();

        if message["type"].as_str() == Some("user") {
            let text = message["message"]["content"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|block| block["type"].as_str() == Some("text"))
                .filter_map(|block| block["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n");

            let repeats_launch = self.repeats_launch(canonical, &text);

            if !text.trim().is_empty() && !repeats_launch {
                items.push(Item::UserMessage { text: Some(text) });
            }
        }

        items.extend(child_content_items(message, &mut self.open_tools));

        items
    }

    /// Whether this text is the launch instruction already published as the
    /// child's opening message.
    fn repeats_launch(&self, canonical: &str, text: &str) -> bool {
        self.launch_prompts
            .get(canonical)
            .is_some_and(|prompt| prompt.trim() == text.trim())
    }
}
