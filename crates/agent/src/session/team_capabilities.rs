//! Structured moderator operations supported by a live provider session.

use crate::session::AgentKind;

#[derive(Clone, Debug)]
pub struct TeamLaunch {
    pub moderator: bool,
    pub restore_transcript: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapabilityFailure {
    pub reason: String,
    pub remedy: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModeratorAdmission {
    Unavailable(CapabilityFailure),
    CodexDynamicTools { backend_generation: u64 },
}

impl ModeratorAdmission {
    pub fn check(&self, backend_generation: u64) -> Result<(), CapabilityFailure> {
        match self {
            Self::CodexDynamicTools {
                backend_generation: registered,
            } if *registered == backend_generation => Ok(()),

            Self::Unavailable(failure) => Err(failure.clone()),

            Self::CodexDynamicTools { .. } => Err(CapabilityFailure {
                reason: "Moderator operations belong to an earlier session.".into(),
                remedy: "Register moderator operations in the current session.".into(),
            }),
        }
    }
}

#[derive(Clone, Debug)]
pub struct TeamCapabilities {
    pub moderation: ModeratorAdmission,
}

impl TeamCapabilities {
    pub fn unverified(kind: AgentKind) -> Self {
        Self {
            moderation: ModeratorAdmission::Unavailable(CapabilityFailure {
                reason: format!(
                    "{} has no registered moderator operations for this session.",
                    kind.display()
                ),
                remedy: "Choose fixed rounds or a member with registered moderator operations."
                    .into(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::session::AgentKind;
    use crate::session::team_capabilities::{ModeratorAdmission, TeamCapabilities};

    #[test]
    fn moderator_operations_belong_to_the_registered_session() {
        let registered = ModeratorAdmission::CodexDynamicTools {
            backend_generation: 4,
        };

        assert!(registered.check(4).is_ok());
        assert!(registered.check(5).is_err());

        for kind in AgentKind::ALL {
            assert!(
                TeamCapabilities::unverified(kind)
                    .moderation
                    .check(4)
                    .is_err()
            );
        }
    }
}
