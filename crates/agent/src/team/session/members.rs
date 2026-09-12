use std::fs;
use std::path::Path;

use crate::chat::ThreadSettings;
use crate::team::content::AttachmentReference;
use crate::team::discussion::PauseReason;
use crate::team::identity::{MemberId, OwnershipGeneration, RoomId};
use crate::team::member::{HistoryScope, MemberConfig};
use crate::team::session::{TeamError, TeamSession};
use crate::team::storage::StorageError;

impl TeamSession {
    pub fn saved_rooms(data_directory: &Path) -> Result<Vec<RoomId>, TeamError> {
        let directory = data_directory.join("agent-teams");

        if !directory.exists() {
            return Ok(Vec::new());
        }

        let mut rooms = Vec::new();

        for entry in fs::read_dir(directory).map_err(StorageError::from)? {
            let entry = entry.map_err(StorageError::from)?;

            if entry.file_type().map_err(StorageError::from)?.is_dir()
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

    pub fn add_member(&mut self, config: MemberConfig) -> Result<MemberId, TeamError> {
        let mut room = self.room().clone();
        let id = room.add_member(config)?;

        self.store.commit(room)?;

        Ok(id)
    }

    pub fn rename_member(&mut self, member: MemberId, name: &str) -> Result<(), TeamError> {
        let mut room = self.room().clone();

        room.rename_member(member, name)?;
        self.store.commit(room)?;

        Ok(())
    }

    pub fn set_member_context(
        &mut self,
        id: MemberId,
        role: String,
        history: HistoryScope,
    ) -> Result<(), TeamError> {
        let mut room = self.room().clone();

        let member = room
            .members
            .iter_mut()
            .find(|member| member.id == id)
            .ok_or(TeamError::Unavailable)?;

        member.role = role;
        member.history = history;

        for discussion in &mut room.discussions {
            discussion.pause(PauseReason::User);
        }

        self.store.commit(room)?;

        Ok(())
    }

    pub fn set_member_settings(
        &mut self,
        id: MemberId,
        ownership: OwnershipGeneration,
        settings: ThreadSettings,
    ) -> Result<(), TeamError> {
        let mut room = self.room().clone();

        room.set_member_settings(id, ownership, settings)?;
        self.store.commit(room)?;

        Ok(())
    }

    pub fn record_provider_identity(
        &mut self,
        id: MemberId,
        ownership: OwnershipGeneration,
        provider_id: &str,
        moderator_registered: bool,
    ) -> Result<(), TeamError> {
        let mut room = self.room().clone();

        let member = room
            .members
            .iter_mut()
            .find(|member| member.id == id && member.ownership == ownership)
            .ok_or(TeamError::Unavailable)?;

        if provider_id.trim().is_empty()
            || member
                .provider_id
                .as_deref()
                .is_some_and(|current| current != provider_id)
        {
            return Err(TeamError::Unavailable);
        }

        if member.provider_id.as_deref() == Some(provider_id)
            && member.moderator_registered == moderator_registered
        {
            return Ok(());
        }

        member.provider_id = Some(provider_id.to_owned());
        member.moderator_registered = moderator_registered;
        self.store.commit(room)?;

        Ok(())
    }

    pub fn exclude_member(&mut self, id: MemberId) -> Result<(), TeamError> {
        if !self.slots.is_idle() || !self.restored_uncertainty.is_empty() {
            return Err(TeamError::Busy);
        }

        let mut room = self.room().clone();

        room.exclude_member(id)?;

        for discussion in &mut room.discussions {
            if discussion.participants.contains(&id) || discussion.mode.report_author() == id {
                discussion.pause(PauseReason::MemberUnavailable(id));
            }
        }

        self.store.commit(room)?;
        self.readiness.remove(&id);

        Ok(())
    }

    pub fn save_attachment(
        &self,
        media_type: &str,
        bytes: &[u8],
    ) -> Result<AttachmentReference, TeamError> {
        Ok(self.store.save_attachment(media_type, bytes)?)
    }

    pub fn read_attachment(&self, reference: &AttachmentReference) -> Result<Vec<u8>, TeamError> {
        Ok(self.store.read_attachment(reference)?)
    }

    pub fn close(&mut self) -> Result<(), TeamError> {
        let mut room = self.room().clone();

        for discussion in &mut room.discussions {
            discussion.pause(PauseReason::Closed);
        }

        self.store.commit(room)?;
        self.store.checkpoint()?;

        Ok(())
    }
}
