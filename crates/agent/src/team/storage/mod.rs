mod validation;

#[cfg(test)]
mod tests;

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use nmt_platform::filesystem::replace_file_durable;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tempfile::NamedTempFile;
use thiserror::Error;

use crate::team::model::RoomId;
use crate::team::room::Room;

const VERSION: u32 = 3;

/// The directory under the data directory that holds one directory per room.
const ROOMS_DIRECTORY: &str = "agent-teams";

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("room storage is unavailable: {0}")]
    Io(#[from] io::Error),
    #[error("room snapshot cannot be decoded: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported Team data version {0}")]
    UnsupportedVersion(u64),
    #[error("room snapshot failed validation: {0}")]
    Invalid(&'static str),
    #[error("legacy Team room format is not supported; saved files were left unchanged")]
    LegacyFormat,
    #[error("reopen this room to reconcile a failed storage operation")]
    ReopenRequired,
}

pub struct RoomStore {
    directory: PathBuf,
    _lock: File,
    room: Room,
    revision: u64,
    failed: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    version: u32,
    revision: u64,
    room: Room,
}

impl RoomStore {
    pub fn create(data_directory: &Path, room: Room) -> Result<Self, StorageError> {
        validation::validate(&room)?;

        let parent = data_directory.join(ROOMS_DIRECTORY);

        fs::create_dir_all(&parent)?;

        let directory = parent.join(room.id().to_string());

        fs::create_dir(&directory)?;

        let lock = lock_room(&directory)?;

        write_snapshot(&directory, &room, 0)?;

        Ok(Self {
            directory,
            _lock: lock,
            room,
            revision: 0,
            failed: false,
        })
    }

    /// The rooms saved under `data_directory`, in id order.
    pub fn saved_rooms(data_directory: &Path) -> Result<Vec<RoomId>, StorageError> {
        let directory = data_directory.join(ROOMS_DIRECTORY);

        if !directory.exists() {
            return Ok(Vec::new());
        }

        let mut rooms = Vec::new();

        for entry in fs::read_dir(directory)? {
            let entry = entry?;

            if entry.file_type()?.is_dir()
                && let Some(id) = entry
                    .file_name()
                    .to_str()
                    .and_then(|name| name.parse().ok())
            {
                rooms.push(id);
            }
        }

        rooms.sort();

        Ok(rooms)
    }

    pub fn open(data_directory: &Path, id: RoomId) -> Result<Self, StorageError> {
        let directory = data_directory.join(ROOMS_DIRECTORY).join(id.to_string());
        let lock = lock_room(&directory)?;

        let bytes = match fs::read(directory.join("room.json")) {
            Err(error)
                if error.kind() == io::ErrorKind::NotFound
                    && (directory.join("checkpoint.json").exists()
                        || directory.join("journal.jsonl").exists()) =>
            {
                return Err(StorageError::LegacyFormat);
            }
            result => result?,
        };

        let raw: Value = serde_json::from_slice(&bytes)?;

        let version = raw
            .get("version")
            .and_then(Value::as_u64)
            .ok_or(StorageError::Invalid("missing room version"))?;

        if version != u64::from(VERSION) {
            return Err(StorageError::UnsupportedVersion(version));
        }

        let snapshot: Snapshot = serde_json::from_value(raw)?;

        if snapshot.room.id() != id {
            return Err(StorageError::Invalid("room identity changed"));
        }

        validation::validate(&snapshot.room)?;

        Ok(Self {
            directory,
            _lock: lock,
            room: snapshot.room,
            revision: snapshot.revision,
            failed: false,
        })
    }

    pub fn room(&self) -> &Room {
        &self.room
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Publish memory only after the complete snapshot reaches durable storage.
    /// A failed replacement can have reached disk, so reopen before another write.
    pub fn commit(&mut self, next: Room) -> Result<(), StorageError> {
        if self.failed {
            return Err(StorageError::ReopenRequired);
        }

        if next == self.room {
            return Ok(());
        }

        validation::validate(&next)?;
        validation::validate_update(&self.room, &next)?;

        let revision = self
            .revision
            .checked_add(1)
            .ok_or(StorageError::Invalid("room revision exhausted"))?;

        if let Err(error) = write_snapshot(&self.directory, &next, revision) {
            self.failed = true;

            return Err(error);
        }

        self.room = next;
        self.revision = revision;

        Ok(())
    }
}

fn write_snapshot(directory: &Path, room: &Room, revision: u64) -> Result<(), StorageError> {
    let snapshot = Snapshot {
        version: VERSION,
        revision,
        room: room.clone(),
    };

    atomic_write(
        &directory.join("room.json"),
        &serde_json::to_vec(&snapshot)?,
    )?;

    Ok(())
}

fn lock_room(directory: &Path) -> Result<File, StorageError> {
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(directory.join("owner.lock"))?;

    lock.try_lock().map_err(io::Error::other)?;

    Ok(lock)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let directory = path
        .parent()
        .ok_or_else(|| io::Error::other("missing parent directory"))?;

    let mut temporary = NamedTempFile::new_in(directory)?;

    temporary.write_all(bytes)?;

    temporary.as_file().sync_all()?;

    replace_file_durable(temporary.path(), path)
}
