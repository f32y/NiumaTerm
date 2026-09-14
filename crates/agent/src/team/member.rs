use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::AgentWorkspace;
use crate::chat::ThreadSettings;
use crate::session::AgentKind;
use crate::team::identity::{MemberId, MessageId, OwnershipGeneration, SummaryId};

/// A lookup into protected profile storage, without resolved launch credentials.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileReference {
    pub kind: AgentKind,
    pub name: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "scope", rename_all = "snake_case")]
pub enum HistoryScope {
    #[default]
    CompletedPublic,
    FromMessage(MessageId),

    Selected {
        messages: BTreeSet<MessageId>,
        summaries: BTreeSet<SummaryId>,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptedCoverage {
    pub messages: BTreeSet<MessageId>,
    pub summaries: BTreeSet<SummaryId>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Member {
    pub(super) id: MemberId,
    pub(super) name: String,
    pub(super) profile: ProfileReference,
    pub(super) ownership: OwnershipGeneration,
    pub(super) roots: AgentWorkspace,
    pub(super) settings: ThreadSettings,
    pub(super) role: String,
    pub(super) history: HistoryScope,
    pub(super) coverage: AcceptedCoverage,
    pub(super) excluded: bool,
    #[serde(default)]
    pub(super) provider_id: Option<String>,
    #[serde(default)]
    pub(super) moderator_registered: bool,
}

/// New members receive their own copy of settings; profile defaults stay shared
/// only in the profile store, never through mutable member state.
pub struct MemberConfig {
    pub name: String,
    pub profile: ProfileReference,
    pub roots: AgentWorkspace,
    pub settings: ThreadSettings,
    pub role: String,
    pub history: HistoryScope,
}

impl Member {
    pub fn provider_id(&self) -> Option<&str> {
        self.provider_id.as_deref()
    }

    pub fn moderator_registered(&self) -> bool {
        self.moderator_registered
    }

    pub fn id(&self) -> MemberId {
        self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn profile(&self) -> &ProfileReference {
        &self.profile
    }

    pub fn ownership(&self) -> OwnershipGeneration {
        self.ownership
    }

    pub fn roots(&self) -> &AgentWorkspace {
        &self.roots
    }

    pub fn settings(&self) -> &ThreadSettings {
        &self.settings
    }

    pub fn role(&self) -> &str {
        &self.role
    }

    pub fn history(&self) -> &HistoryScope {
        &self.history
    }

    pub fn coverage(&self) -> &AcceptedCoverage {
        &self.coverage
    }

    pub fn excluded(&self) -> bool {
        self.excluded
    }
}
