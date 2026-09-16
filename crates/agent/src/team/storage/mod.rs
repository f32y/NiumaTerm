pub use crate::team::session::DispatchError;

pub(super) use crate::team::storage::records::digest;

mod records;
mod validation;

#[cfg(test)]
mod tests;

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead as _, BufReader, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use nmt_platform::filesystem::replace_file_durable;
use tempfile::NamedTempFile;
use thiserror::Error;

use crate::team::model::RoomId;
use crate::team::room::Room;
use crate::team::storage::records::{Checkpoint, JournalRecord, RoomDelta, decode, encode};

const VERSION: u32 = 1;

/// The directory under the data directory that holds one directory per room.
const ROOMS_DIRECTORY: &str = "agent-teams";

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("room storage is unavailable: {0}")]
    Io(#[from] io::Error),
    #[error("room records cannot be decoded: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported Team data version {0}")]
    UnsupportedVersion(u64),
    #[error("room records failed validation: {0}")]
    Invalid(&'static str),
    #[error("reopen this room to reconcile a failed storage operation")]
    ReopenRequired,
}

pub struct RoomStore {
    directory: PathBuf,
    _lock: File,
    journal: File,
    room: Room,
    sequence: u64,
    digest: String,
    failed: bool,
}

impl RoomStore {
    pub(super) fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn create(data_directory: &Path, room: Room) -> Result<Self, StorageError> {
        validation::validate(&room)?;

        let parent = data_directory.join(ROOMS_DIRECTORY);

        fs::create_dir_all(&parent)?;

        let directory = parent.join(room.id().to_string());

        fs::create_dir(&directory)?;

        let lock = lock_room(&directory)?;

        let checkpoint = Checkpoint {
            version: VERSION,
            sequence: 0,
            digest: String::new(),
            room: room.clone(),
        };

        atomic_write(&directory.join("checkpoint.json"), &encode(&checkpoint)?)?;

        let journal = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(directory.join("journal.jsonl"))?;

        journal.sync_all()?;

        Ok(Self {
            directory,
            _lock: lock,
            journal,
            room,
            sequence: 0,
            digest: String::new(),
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

    pub fn open(data_directory: &Path, id: RoomId) -> Result<(Self, bool), StorageError> {
        let directory = data_directory.join(ROOMS_DIRECTORY).join(id.to_string());
        let lock = lock_room(&directory)?;
        let checkpoint: Checkpoint = decode(&fs::read(directory.join("checkpoint.json"))?)?;

        if checkpoint.room.id() != id {
            return Err(StorageError::Invalid("room identity changed"));
        }

        validation::validate(&checkpoint.room)?;

        let mut journal = OpenOptions::new()
            .read(true)
            .write(true)
            .open(directory.join("journal.jsonl"))?;

        let replay = replay(&mut journal, checkpoint)?;

        let truncated = if let Some(valid_bytes) = replay.truncated_at {
            journal.set_len(valid_bytes)?;

            journal.sync_all()?;

            true
        } else {
            false
        };

        journal.seek(SeekFrom::End(0))?;

        Ok((
            Self {
                directory,
                _lock: lock,
                journal,
                room: replay.room,
                sequence: replay.sequence,
                digest: replay.digest,
                failed: false,
            },
            truncated,
        ))
    }

    pub fn room(&self) -> &Room {
        &self.room
    }

    pub fn revision(&self) -> u64 {
        self.sequence
    }

    /// Publish in-memory state only after the ordered record reaches durable
    /// storage. A failed append closes this writer until recovery determines
    /// whether any complete record survived.
    pub fn commit(&mut self, next: Room) -> Result<(), StorageError> {
        if self.failed {
            return Err(StorageError::ReopenRequired);
        }

        if next == self.room {
            return Ok(());
        }

        validation::validate(&next)?;

        let changes = RoomDelta::between(&self.room, &next)?;

        let sequence = self
            .sequence
            .checked_add(1)
            .ok_or(StorageError::Invalid("record sequence exhausted"))?;

        let record = JournalRecord {
            version: VERSION,
            sequence,
            previous: self.digest.clone(),
            changes,
        };

        let mut encoded = encode(&record)?;

        let digest = records::digest(&encoded);

        encoded.push(b'\n');

        if let Err(error) = self.append(&encoded) {
            self.failed = true;

            return Err(error.into());
        }

        self.room = next;
        self.sequence = sequence;
        self.digest = digest;

        Ok(())
    }

    fn append(&mut self, encoded: &[u8]) -> io::Result<()> {
        self.journal.seek(SeekFrom::End(0))?;

        self.journal.write_all(encoded)?;

        self.journal.sync_all()
    }

    pub fn checkpoint(&mut self) -> Result<(), StorageError> {
        if self.failed {
            return Err(StorageError::ReopenRequired);
        }

        let checkpoint = Checkpoint {
            version: VERSION,
            sequence: self.sequence,
            digest: self.digest.clone(),
            room: self.room.clone(),
        };

        atomic_write(
            &self.directory.join("checkpoint.json"),
            &encode(&checkpoint)?,
        )?;

        // The old journal remains valid if the process stops before truncation;
        // replay verifies its saved prefix against the checkpoint digest.
        if let Err(error) = self
            .journal
            .set_len(0)
            .and_then(|()| self.journal.sync_all())
        {
            self.failed = true;

            return Err(error.into());
        }

        Ok(())
    }
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

struct Replay {
    room: Room,
    sequence: u64,
    digest: String,
    truncated_at: Option<u64>,
}

fn replay(journal: &mut File, checkpoint: Checkpoint) -> Result<Replay, StorageError> {
    let mut result = Replay {
        room: checkpoint.room,
        sequence: checkpoint.sequence,
        digest: checkpoint.digest,
        truncated_at: None,
    };

    let mut reader = BufReader::new(journal);
    let mut offset = 0;
    let mut prior: Option<(u64, String)> = None;
    let mut line = Vec::new();

    loop {
        line.clear();

        let count = reader.read_until(b'\n', &mut line)?;

        if count == 0 {
            break;
        }

        if line.last() != Some(&b'\n') {
            result.truncated_at = Some(offset);

            break;
        }

        line.pop();

        let record: JournalRecord = decode(&line)?;
        let record_digest = digest(&line);

        if let Some((sequence, hash)) = &prior {
            if sequence.checked_add(1) != Some(record.sequence) || &record.previous != hash {
                return Err(StorageError::Invalid(
                    "journal sequence or predecessor changed",
                ));
            }
        } else if record.sequence == 0
            || (record.sequence > result.sequence
                && Some(record.sequence) != result.sequence.checked_add(1))
        {
            return Err(StorageError::Invalid(
                "journal starts after missing records",
            ));
        }

        if record.sequence == checkpoint.sequence && record_digest != result.digest {
            return Err(StorageError::Invalid("checkpoint does not match journal"));
        }

        if record.sequence > result.sequence {
            if Some(record.sequence) != result.sequence.checked_add(1)
                || record.previous != result.digest
            {
                return Err(StorageError::Invalid(
                    "journal does not continue checkpoint",
                ));
            }

            record.changes.apply(&mut result.room);

            validation::validate(&result.room)?;

            result.sequence = record.sequence;
            result.digest = record_digest.clone();
        }

        prior = Some((record.sequence, record_digest));
        offset += count as u64;
    }

    if prior.is_some_and(|(sequence, _)| sequence < checkpoint.sequence) {
        return Err(StorageError::Invalid(
            "retained journal ends before checkpoint",
        ));
    }

    Ok(result)
}
