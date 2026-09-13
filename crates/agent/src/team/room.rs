use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::AgentWorkspace;
use crate::chat::ThreadSettings;
use crate::team::attempt::Attempt;
use crate::team::budget::Budget;
use crate::team::content::{PublicMessage, Summary, UserInput};
use crate::team::discussion::{Discussion, DiscussionError, DiscussionMode, DiscussionState};
use crate::team::identity::{ConversationId, MemberId, OperationId, OwnershipGeneration, RoomId};
use crate::team::member::{AcceptedCoverage, Member, MemberConfig};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Room {
    pub(super) id: RoomId,
    pub(super) workspace: AgentWorkspace,
    pub(super) members: Vec<Member>,
    pub(super) discussions: Vec<Discussion>,
    pub(super) messages: Vec<PublicMessage>,
    pub(super) summaries: Vec<Summary>,
    pub(super) input_history: Vec<UserInput>,
    pub(super) controls: RoomControls,
    pub(super) attempts: Vec<Attempt>,
    pub(super) direct_allowances: BTreeMap<OperationId, Budget>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomControls {
    pub automatic_summaries: bool,
}

impl Default for RoomControls {
    fn default() -> Self {
        Self {
            automatic_summaries: true,
        }
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum MemberError {
    #[error("member name must be nonempty and contain no control characters")]
    InvalidName,

    #[error("a member already uses that name")]
    DuplicateName,

    #[error("member no longer belongs to this room")]
    MissingMember,

    #[error("session ownership has changed")]
    StaleOwner,
}

impl Room {
    pub fn new(workspace: AgentWorkspace) -> Self {
        Self {
            id: RoomId::new(),
            workspace,
            members: Vec::new(),
            discussions: Vec::new(),
            messages: Vec::new(),
            summaries: Vec::new(),
            input_history: Vec::new(),
            controls: RoomControls::default(),
            attempts: Vec::new(),
            direct_allowances: BTreeMap::new(),
        }
    }

    pub fn id(&self) -> RoomId {
        self.id
    }

    pub fn workspace(&self) -> &AgentWorkspace {
        &self.workspace
    }

    pub fn members(&self) -> &[Member] {
        &self.members
    }

    pub fn member(&self, id: MemberId) -> Option<&Member> {
        self.members.iter().find(|member| member.id == id)
    }

    pub fn discussions(&self) -> &[Discussion] {
        &self.discussions
    }

    pub fn controls(&self) -> &RoomControls {
        &self.controls
    }

    pub fn messages(&self) -> &[PublicMessage] {
        &self.messages
    }

    pub fn summaries(&self) -> &[Summary] {
        &self.summaries
    }

    pub fn input_history(&self) -> &[UserInput] {
        &self.input_history
    }

    pub fn attempts(&self) -> &[Attempt] {
        &self.attempts
    }

    pub(crate) fn create_discussion(
        &mut self,
        objective: String,
        participants: Vec<MemberId>,
        mode: DiscussionMode,
    ) -> Result<&Discussion, DiscussionError> {
        if self
            .discussions
            .iter()
            .any(|discussion| discussion.state() != DiscussionState::Completed)
        {
            return Err(DiscussionError::AlreadyActive);
        }

        if participants.iter().any(|id| self.member(*id).is_none())
            || self.member(mode.report_author()).is_none()
        {
            return Err(DiscussionError::InvalidParticipants);
        }

        let discussion = Discussion::new(objective, participants, mode)?;
        let index = self.discussions.len();

        self.discussions.push(discussion);

        Ok(&self.discussions[index])
    }

    pub fn add_member(&mut self, config: MemberConfig) -> Result<MemberId, MemberError> {
        let name = self.available_name(&config.name, None)?;
        let id = MemberId::new();

        self.members.push(Member {
            id,
            name,
            profile: config.profile,
            conversation: ConversationId::new(),
            ownership: OwnershipGeneration::default(),
            roots: config.roots,
            settings: config.settings,
            role: config.role,
            history: config.history,
            coverage: AcceptedCoverage::default(),
            excluded: false,
            provider_id: None,
            moderator_registered: false,
        });

        Ok(id)
    }

    #[cfg(test)]
    pub(crate) fn rename_member(&mut self, id: MemberId, name: &str) -> Result<(), MemberError> {
        let name = self.available_name(name, Some(id))?;

        self.member_mut(id)?.name = name;

        Ok(())
    }

    pub fn set_member_settings(
        &mut self,
        id: MemberId,
        ownership: OwnershipGeneration,
        settings: ThreadSettings,
    ) -> Result<(), MemberError> {
        let member = self.member_mut(id)?;

        if member.ownership != ownership {
            return Err(MemberError::StaleOwner);
        }

        member.settings = settings;

        Ok(())
    }

    pub fn exclude_member(&mut self, id: MemberId) -> Result<(), MemberError> {
        self.member_mut(id)?.excluded = true;

        Ok(())
    }

    fn member_mut(&mut self, id: MemberId) -> Result<&mut Member, MemberError> {
        self.members
            .iter_mut()
            .find(|member| member.id == id)
            .ok_or(MemberError::MissingMember)
    }

    fn available_name(&self, name: &str, except: Option<MemberId>) -> Result<String, MemberError> {
        let name = name.trim();

        if name.is_empty() || name.chars().any(char::is_control) {
            return Err(MemberError::InvalidName);
        }

        let key = name.to_lowercase();

        if self
            .members
            .iter()
            .any(|member| Some(member.id) != except && member.name.to_lowercase() == key)
        {
            return Err(MemberError::DuplicateName);
        }

        Ok(name.to_owned())
    }
}
