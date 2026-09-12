#[cfg(test)]
mod tests;

use std::fs::{self, File};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::AgentWorkspace;
use crate::chat::ThreadSettings;
use crate::session::AgentKind;
use crate::team::identity::{ConversationId, MemberId, OperationId, OwnershipGeneration, RoomId};
use crate::team::member::ProfileReference;
use crate::team::storage::records::{decode, digest, encode};
use crate::team::storage::{StorageError, VERSION, atomic_write, lock_room};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConversationOwner {
    Independent,
    Team { room: RoomId, member: MemberId },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferTicket {
    operation: OperationId,
    from: ConversationOwner,
    to: ConversationOwner,
    generation: OwnershipGeneration,
}

impl TransferTicket {
    pub fn destination(&self) -> ConversationOwner {
        self.to
    }

    pub fn generation(&self) -> OwnershipGeneration {
        self.generation
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnershipRecord {
    version: u32,
    pub conversation: ConversationId,
    pub provider_id: String,
    pub profile: ProfileReference,
    pub roots: AgentWorkspace,
    pub settings: ThreadSettings,
    pub owner: ConversationOwner,
    pub generation: OwnershipGeneration,
    pub pending: Option<TransferTicket>,
}

impl OwnershipRecord {
    pub fn new(
        conversation: ConversationId,
        provider_id: String,
        profile: ProfileReference,
        roots: AgentWorkspace,
        settings: ThreadSettings,
        owner: ConversationOwner,
    ) -> Self {
        Self {
            version: VERSION,
            conversation,
            provider_id,
            profile,
            roots,
            settings,
            owner,
            generation: OwnershipGeneration::default(),
            pending: None,
        }
    }
}

/// One provider identity has one durable authority and one live lock. A view
/// snapshot can lag behind a transfer; reopening consults this record before
/// it may acquire the provider conversation for execution.
pub struct OwnershipStore {
    directory: PathBuf,
    _lock: File,
    record: OwnershipRecord,
    failed: bool,
}

impl OwnershipStore {
    pub fn create(data_directory: &Path, record: OwnershipRecord) -> Result<Self, StorageError> {
        validate(&record)?;

        let directory = directory(data_directory, record.profile.kind, &record.provider_id)?;

        fs::create_dir_all(
            directory
                .parent()
                .ok_or(StorageError::Invalid("ownership directory has no parent"))?,
        )?;

        fs::create_dir(&directory)?;

        let lock = lock_room(&directory)?;

        atomic_write(&directory.join("ownership.json"), &encode(&record)?)?;

        Ok(Self {
            directory,
            _lock: lock,
            record,
            failed: false,
        })
    }

    pub fn open(
        data_directory: &Path,
        kind: AgentKind,
        provider_id: &str,
    ) -> Result<Self, StorageError> {
        let directory = directory(data_directory, kind, provider_id)?;
        let lock = lock_room(&directory)?;
        let record: OwnershipRecord = decode(&fs::read(directory.join("ownership.json"))?)?;

        validate(&record)?;

        if record.profile.kind != kind || record.provider_id != provider_id {
            return Err(StorageError::Invalid("provider ownership identity changed"));
        }

        Ok(Self {
            directory,
            _lock: lock,
            record,
            failed: false,
        })
    }

    pub fn record(&self) -> &OwnershipRecord {
        &self.record
    }

    pub fn authorizes(&self, owner: ConversationOwner, generation: OwnershipGeneration) -> bool {
        !self.failed && self.record.owner == owner && self.record.generation == generation
    }

    /// The source remains authoritative during preparation. The destination
    /// must persist its reference before the caller commits this ticket.
    pub fn prepare_transfer(
        &mut self,
        owner: ConversationOwner,
        generation: OwnershipGeneration,
        destination: ConversationOwner,
        effective_settings: ThreadSettings,
    ) -> Result<TransferTicket, StorageError> {
        if !self.authorizes(owner, generation)
            || owner == destination
            || self.record.pending.is_some()
        {
            return Err(StorageError::Invalid(
                "conversation ownership changed or transfer is already pending",
            ));
        }

        let ticket = TransferTicket {
            operation: OperationId::new(),
            from: owner,
            to: destination,
            generation: generation
                .next()
                .ok_or(StorageError::Invalid("ownership generation is exhausted"))?,
        };

        let mut record = self.record.clone();

        record.settings = effective_settings;
        record.pending = Some(ticket.clone());
        self.commit(record)?;

        Ok(ticket)
    }

    pub fn commit_transfer(&mut self, ticket: &TransferTicket) -> Result<(), StorageError> {
        if self.record.pending.as_ref() != Some(ticket) || self.record.owner != ticket.from {
            return Err(StorageError::Invalid("transfer ticket is stale"));
        }

        let mut record = self.record.clone();

        record.owner = ticket.to;
        record.generation = ticket.generation;
        record.pending = None;

        self.commit(record)
    }

    pub fn cancel_transfer(&mut self, ticket: &TransferTicket) -> Result<(), StorageError> {
        if self.record.pending.as_ref() != Some(ticket) {
            return Err(StorageError::Invalid("transfer ticket is stale"));
        }

        let mut record = self.record.clone();

        record.pending = None;

        self.commit(record)
    }

    fn commit(&mut self, record: OwnershipRecord) -> Result<(), StorageError> {
        if self.failed {
            return Err(StorageError::ReopenRequired);
        }

        validate(&record)?;

        if let Err(error) = atomic_write(&self.directory.join("ownership.json"), &encode(&record)?)
        {
            self.failed = true;

            return Err(error.into());
        }

        self.record = record;

        Ok(())
    }
}

fn directory(root: &Path, kind: AgentKind, provider_id: &str) -> Result<PathBuf, StorageError> {
    Ok(root
        .join("agent-conversation-owners")
        .join(digest(&serde_json::to_vec(&(kind, provider_id))?)))
}

fn validate(record: &OwnershipRecord) -> Result<(), StorageError> {
    if record.version != VERSION
        || record.provider_id.trim().is_empty()
        || record.provider_id.chars().any(char::is_control)
    {
        return Err(StorageError::Invalid("invalid provider ownership record"));
    }

    if let Some(ticket) = &record.pending
        && (ticket.from != record.owner
            || ticket.from == ticket.to
            || record.generation.next() != Some(ticket.generation))
    {
        return Err(StorageError::Invalid("invalid transfer transition"));
    }

    Ok(())
}
