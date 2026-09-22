#[cfg(test)]
#[path = "history_tests.rs"]
mod tests;

use std::fs;
use std::path::Path;
use std::time::SystemTime;

use crate::team::model::RoomId;
use crate::team::storage::validation::validate;
use crate::team::storage::{ROOMS_DIRECTORY, RoomStore, Snapshot, StorageError, VERSION};

#[derive(Clone, Debug)]
pub struct RoomSummary {
    pub id: RoomId,
    pub title: String,
    pub cwd: Option<String>,
    pub last_active: SystemTime,
}

/// Read summaries without opening live sessions or acquiring their ownership locks.
/// A damaged room cannot prevent other saved conversations from being listed.
pub fn recent_rooms(directory: &Path) -> Result<Vec<RoomSummary>, StorageError> {
    let mut summaries = Vec::new();

    for id in RoomStore::saved_rooms(directory)? {
        let path = directory
            .join(ROOMS_DIRECTORY)
            .join(id.to_string())
            .join("room.json");

        match read_summary(&path, id) {
            Ok(Some(summary)) => summaries.push(summary),
            Ok(None) => {}
            Err(error) => tracing::warn!(%id, %error, "could not read Team history entry"),
        }
    }

    summaries.sort_by(|left, right| {
        right
            .last_active
            .cmp(&left.last_active)
            .then_with(|| left.id.cmp(&right.id))
    });

    Ok(summaries)
}

fn read_summary(path: &Path, id: RoomId) -> Result<Option<RoomSummary>, StorageError> {
    let snapshot: Snapshot = serde_json::from_slice(&fs::read(path)?)?;

    if snapshot.version != VERSION {
        return Err(StorageError::UnsupportedVersion(u64::from(
            snapshot.version,
        )));
    }

    if snapshot.room.id() != id {
        return Err(StorageError::Invalid("room identity changed"));
    }

    validate(&snapshot.room)?;

    let room = snapshot.room;

    if room.members.is_empty() && room.input_history.is_empty() && room.messages.is_empty() {
        return Ok(None);
    }

    let title = room
        .input_history
        .iter()
        .map(|input| input.text.trim())
        .find(|text| !text.is_empty())
        .map(|text| {
            text.lines()
                .next()
                .unwrap_or(text)
                .chars()
                .take(160)
                .collect()
        })
        .unwrap_or_else(|| {
            room.members()
                .iter()
                .map(|member| member.name())
                .collect::<Vec<_>>()
                .join(", ")
        });

    Ok(Some(RoomSummary {
        id,
        title,
        cwd: room.workspace().primary().map(str::to_owned),
        last_active: fs::metadata(path)?.modified()?,
    }))
}
