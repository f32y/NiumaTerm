use std::collections::HashMap;
use std::fs;
use std::io::{BufReader, BufWriter, Write as _};
use std::path::{Path, PathBuf};

use chrono::{SecondsFormat, Utc};
use serde_json::Value;
use tracing::warn;
use uuid::Uuid;

use super::index::TranscriptIndex;
use super::paths::session_path;
use super::replay::parse_replay;
use super::titles::{title_line, user_prompt_text};
use crate::chat::Item;

/// A conversation rewind either starts a fresh process before the first
/// prompt or resumes an immutable prefix copied into a new Claude session.
#[derive(Clone, Debug, PartialEq)]
pub struct ClaudeFork {
    pub session_id: Option<String>,
    pub replay: Vec<Item>,
}

/// Fork the active conversation immediately before a human prompt. The source
/// JSONL is never opened for writing, and file-history snapshots are not part
/// of the copied active chain, so the result has no inherited file undo state.
pub fn fork_session_before(
    cwd: Option<&str>,
    source_session_id: &str,
    user_message_id: &str,
) -> Result<ClaudeFork, String> {
    Uuid::parse_str(source_session_id)
        .map_err(|_| format!("invalid Claude session id: {source_session_id}"))?;
    Uuid::parse_str(user_message_id)
        .map_err(|_| format!("invalid Claude user message id: {user_message_id}"))?;

    let source_path = session_path(cwd, source_session_id)
        .ok_or_else(|| format!("Claude session {source_session_id} has no project directory"))?;
    let source = fs::File::open(&source_path)
        .map_err(|error| format!("could not read Claude session {source_session_id}: {error}"))?;
    let transcript = TranscriptIndex::read(BufReader::new(source));

    if let Some(parent) = &transcript.broken_parent {
        warn!(
            session_id = source_session_id,
            missing_parent = parent,
            "Claude transcript fork ended at a missing parent"
        );
    }

    let new_session_id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    let Some(records) = build_fork_records(
        &transcript,
        source_session_id,
        user_message_id,
        &new_session_id,
        &now,
    )?
    else {
        return Ok(ClaudeFork {
            session_id: None,
            replay: Vec::new(),
        });
    };

    let project_dir = source_path
        .parent()
        .ok_or_else(|| format!("Claude session {source_session_id} has no parent directory"))?;
    let target = write_fork_file(project_dir, &new_session_id, &records)?;
    let replay_file = fs::File::open(&target)
        .map_err(|error| format!("could not replay fork {new_session_id}: {error}"))?;

    Ok(ClaudeFork {
        session_id: Some(new_session_id),
        replay: parse_replay(BufReader::new(replay_file)),
    })
}

/// Build a prefix fork while preserving fields the current client does not
/// understand. UUID links are rewritten so the copied transcript is an
/// independent graph and can safely be resumed by Claude Code.
pub(super) fn build_fork_records(
    transcript: &TranscriptIndex,
    source_session_id: &str,
    user_message_id: &str,
    new_session_id: &str,
    now: &str,
) -> Result<Option<Vec<Value>>, String> {
    let cutoff = transcript
        .chain
        .iter()
        .position(|index| {
            let record = &transcript.records[*index];
            record["uuid"].as_str() == Some(user_message_id) && user_prompt_text(record).is_some()
        })
        .ok_or_else(|| {
            format!(
                "Claude user message {user_message_id} is not a prompt on the active conversation"
            )
        })?;
    let prefix = transcript.chain[..cutoff]
        .iter()
        .filter_map(|index| transcript.records.get(*index))
        .collect::<Vec<_>>();

    if prefix.is_empty() {
        return Ok(None);
    }

    let uuid_mapping = prefix
        .iter()
        .filter_map(|record| {
            record["uuid"]
                .as_str()
                .map(|uuid| (uuid.to_string(), Uuid::new_v4().to_string()))
        })
        .collect::<HashMap<_, _>>();
    let by_uuid = prefix
        .iter()
        .filter_map(|record| {
            record["uuid"]
                .as_str()
                .map(|uuid| (uuid.to_string(), *record))
        })
        .collect::<HashMap<_, _>>();
    let writable = prefix
        .iter()
        .filter(|record| record["type"].as_str() != Some("progress"))
        .copied()
        .collect::<Vec<_>>();

    if writable.is_empty() {
        return Ok(None);
    }

    let mut records = Vec::with_capacity(writable.len() + 2);
    for (position, original) in writable.iter().enumerate() {
        let original_uuid = original["uuid"]
            .as_str()
            .ok_or_else(|| "Claude fork record is missing its UUID".to_string())?;
        let new_uuid = uuid_mapping
            .get(original_uuid)
            .ok_or_else(|| format!("Claude fork could not remap message {original_uuid}"))?;
        let mut parent = original["parentUuid"].as_str();
        let mut new_parent = None;

        while let Some(parent_uuid) = parent {
            let Some(parent_record) = by_uuid.get(parent_uuid) else {
                break;
            };
            if parent_record["type"].as_str() != Some("progress") {
                new_parent = uuid_mapping.get(parent_uuid).cloned();
                break;
            }
            parent = parent_record["parentUuid"].as_str();
        }

        let mut forked = (*original).clone();
        let object = forked
            .as_object_mut()
            .ok_or_else(|| "Claude fork record is not a JSON object".to_string())?;
        object.insert("uuid".into(), Value::String(new_uuid.clone()));
        object.insert(
            "parentUuid".into(),
            new_parent.map(Value::String).unwrap_or(Value::Null),
        );
        let logical_parent = original["logicalParentUuid"]
            .as_str()
            .and_then(|uuid| uuid_mapping.get(uuid))
            .cloned();
        object.insert(
            "logicalParentUuid".into(),
            logical_parent.map(Value::String).unwrap_or(Value::Null),
        );
        object.insert("sessionId".into(), Value::String(new_session_id.into()));
        object.insert("isSidechain".into(), Value::Bool(false));
        object.insert(
            "timestamp".into(),
            if position + 1 == writable.len() {
                Value::String(now.into())
            } else {
                original
                    .get("timestamp")
                    .cloned()
                    .unwrap_or_else(|| Value::String(now.into()))
            },
        );
        object.insert(
            "forkedFrom".into(),
            serde_json::json!({
                "sessionId": source_session_id,
                "messageUuid": original_uuid,
            }),
        );
        for field in ["teamName", "agentName", "slug", "sourceToolAssistantUUID"] {
            object.remove(field);
        }
        records.push(forked);
    }

    let replacements = transcript
        .records
        .iter()
        .filter(|record| {
            record["type"].as_str() == Some("content-replacement")
                && record["sessionId"].as_str() == Some(source_session_id)
        })
        .filter_map(|record| record["replacements"].as_array())
        .flatten()
        .cloned()
        .collect::<Vec<_>>();
    if !replacements.is_empty() {
        records.push(serde_json::json!({
            "type": "content-replacement",
            "sessionId": new_session_id,
            "replacements": replacements,
            "uuid": Uuid::new_v4().to_string(),
            "timestamp": now,
        }));
    }

    let title = prefix
        .iter()
        .find_map(|record| user_prompt_text(record).as_deref().and_then(title_line))
        .unwrap_or_else(|| "Rewound session".into());
    records.push(serde_json::json!({
        "type": "custom-title",
        "sessionId": new_session_id,
        "customTitle": format!("{title} (rewind)"),
        "uuid": Uuid::new_v4().to_string(),
        "timestamp": now,
    }));

    Ok(Some(records))
}

pub(super) fn write_fork_file(
    project_dir: &Path,
    session_id: &str,
    records: &[Value],
) -> Result<PathBuf, String> {
    let target = project_dir.join(format!("{session_id}.jsonl"));
    let temp = project_dir.join(format!(".{session_id}.{}.tmp", Uuid::new_v4()));
    let write_result = (|| -> Result<(), String> {
        let file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)
            .map_err(|error| format!("could not create Claude fork: {error}"))?;
        let mut writer = BufWriter::new(file);
        for record in records {
            serde_json::to_writer(&mut writer, record)
                .map_err(|error| format!("could not serialize Claude fork: {error}"))?;
            writer
                .write_all(b"\n")
                .map_err(|error| format!("could not write Claude fork: {error}"))?;
        }
        writer
            .flush()
            .map_err(|error| format!("could not flush Claude fork: {error}"))?;
        writer
            .get_ref()
            .sync_all()
            .map_err(|error| format!("could not sync Claude fork: {error}"))?;
        fs::rename(&temp, &target)
            .map_err(|error| format!("could not publish Claude fork: {error}"))
    })();

    if write_result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    write_result.map(|()| target)
}
