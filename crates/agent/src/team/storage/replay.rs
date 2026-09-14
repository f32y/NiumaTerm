use std::fs::File;
use std::io::{BufRead as _, BufReader};

use crate::team::room::Room;
use crate::team::storage::records::{Checkpoint, JournalRecord, decode, digest};
use crate::team::storage::{StorageError, validation};

pub(super) struct Replay {
    pub room: Room,
    pub sequence: u64,
    pub digest: String,
    pub truncated_at: Option<u64>,
}

pub(super) fn replay(journal: &mut File, checkpoint: Checkpoint) -> Result<Replay, StorageError> {
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
