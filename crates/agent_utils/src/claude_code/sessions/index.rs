use std::collections::{HashMap, HashSet};
use std::io::BufRead;

use serde_json::Value;

use super::titles::{clean_prompt, user_prompt_text};
use super::{ClaudeCheckpoint, FileRestoreAvailability};

const TRANSCRIPT_ENTRY_TYPES: [&str; 5] = ["user", "assistant", "progress", "system", "attachment"];

/// Parsed transcript plus the single active parent chain selected using the
/// same leaf rules as the official Agent SDK session helpers.
pub(super) struct TranscriptIndex {
    pub(super) records: Vec<Value>,
    pub(super) chain: Vec<usize>,
    pub(super) snapshot_message_ids: HashSet<String>,
    pub(super) malformed_snapshot: bool,
    pub(super) broken_parent: Option<String>,
}

impl TranscriptIndex {
    pub(super) fn read(reader: impl BufRead) -> Self {
        let records = reader
            .lines()
            .map_while(Result::ok)
            .filter_map(|line| serde_json::from_str::<Value>(&line).ok())
            .collect::<Vec<_>>();
        let (chain, broken_parent) = active_chain_indices(&records);
        let mut snapshot_message_ids = HashSet::new();
        let mut malformed_snapshot = false;

        for record in &records {
            if record["type"].as_str() != Some("file-history-snapshot") {
                continue;
            }

            match record["messageId"].as_str() {
                Some(message_id) => {
                    snapshot_message_ids.insert(message_id.to_string());
                }
                None => malformed_snapshot = true,
            }
        }

        Self {
            records,
            chain,
            snapshot_message_ids,
            malformed_snapshot,
            broken_parent,
        }
    }

    pub(super) fn active_records(&self) -> impl Iterator<Item = &Value> {
        self.chain
            .iter()
            .filter_map(|index| self.records.get(*index))
    }

    pub(super) fn checkpoints(&self) -> Vec<ClaudeCheckpoint> {
        let mut checkpoints = self
            .active_records()
            .filter_map(|record| {
                let user_message_id = record["uuid"].as_str()?.to_string();
                let prompt = clean_prompt(&user_prompt_text(record)?);

                if prompt.is_empty() {
                    return None;
                }

                let file_restore_availability =
                    if self.snapshot_message_ids.contains(&user_message_id) {
                        FileRestoreAvailability::Available
                    } else if self.malformed_snapshot {
                        FileRestoreAvailability::Unknown
                    } else {
                        FileRestoreAvailability::Unavailable
                    };

                Some(ClaudeCheckpoint {
                    user_message_id,
                    parent_message_id: record["parentUuid"].as_str().map(str::to_owned),
                    prompt,
                    timestamp: record["timestamp"].as_str().map(str::to_owned),
                    file_restore_availability,
                })
            })
            .collect::<Vec<_>>();

        checkpoints.reverse();
        checkpoints
    }
}

pub(super) fn is_transcript_entry(record: &Value) -> bool {
    record["type"]
        .as_str()
        .is_some_and(|kind| TRANSCRIPT_ENTRY_TYPES.contains(&kind))
        && record["uuid"].as_str().is_some()
}

/// Find the latest main-conversation leaf, then walk only `parentUuid` back to
/// the root. `logicalParentUuid` crosses a compaction boundary into content the
/// summary replaced, so following it would duplicate discarded conversation.
pub(super) fn active_chain_indices(records: &[Value]) -> (Vec<usize>, Option<String>) {
    let transcript = records
        .iter()
        .enumerate()
        .filter_map(|(index, record)| is_transcript_entry(record).then_some(index))
        .collect::<Vec<_>>();

    if transcript.is_empty() {
        // Very old or synthetic fixtures can lack UUID links entirely. There is
        // no branch information to recover, so chronological replay is the only
        // lossless compatibility behavior available.
        return ((0..records.len()).collect(), None);
    }

    let mut by_uuid = HashMap::new();
    let mut parent_uuids = HashSet::new();

    for &index in &transcript {
        let record = &records[index];
        let uuid = record["uuid"].as_str().unwrap_or_default().to_string();
        by_uuid.insert(uuid, index);

        if let Some(parent) = record["parentUuid"].as_str() {
            parent_uuids.insert(parent.to_string());
        }
    }

    let terminals = transcript.iter().copied().filter(|index| {
        records[*index]["uuid"]
            .as_str()
            .is_some_and(|uuid| !parent_uuids.contains(uuid))
    });
    let mut leaves = Vec::new();

    for terminal in terminals {
        let mut current = Some(terminal);
        let mut seen = HashSet::new();

        while let Some(index) = current {
            let record = &records[index];
            let Some(uuid) = record["uuid"].as_str() else {
                break;
            };
            if !seen.insert(uuid.to_string()) {
                break;
            }
            if matches!(record["type"].as_str(), Some("user" | "assistant")) {
                leaves.push(index);
                break;
            }

            current = record["parentUuid"]
                .as_str()
                .and_then(|parent| by_uuid.get(parent).copied());
        }
    }

    let main_leaves = leaves.iter().copied().filter(|index| {
        let record = &records[*index];
        record["isSidechain"].as_bool() != Some(true)
            && record["isMeta"].as_bool() != Some(true)
            && record["teamName"].as_str().is_none()
    });
    let leaf = main_leaves.max().or_else(|| leaves.into_iter().max());
    let Some(mut current) = leaf else {
        return (Vec::new(), None);
    };

    let mut reversed = Vec::new();
    let mut seen = HashSet::new();
    let mut broken_parent = None;

    loop {
        let record = &records[current];
        let Some(uuid) = record["uuid"].as_str() else {
            break;
        };
        if !seen.insert(uuid.to_string()) {
            break;
        }
        reversed.push(current);

        let Some(parent) = record["parentUuid"].as_str() else {
            break;
        };
        let Some(parent_index) = by_uuid.get(parent).copied() else {
            broken_parent = Some(parent.to_string());
            break;
        };
        current = parent_index;
    }

    reversed.reverse();
    (reversed, broken_parent)
}
