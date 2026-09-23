//! Conversation selection and replay admission without list or widget state.

#[cfg(test)]
#[path = "restore_tests.rs"]
mod restore_tests;

use std::path::Path;

use nmt_platform::filesystem::path_identity;

use crate::chat::{ReplayTurn, SessionSummary};
use crate::claude_code::sessions;
use crate::session::lifecycle::{SessionRuntime, Status};
use crate::session::{AgentKind, RecoveryIdentity, ResumeOutcome};

/// Only controls absent from the provider's resumed settings are seeded locally.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SettingsSeed {
    Defaults,
    Reviewer,
    #[default]
    None,
}

impl SettingsSeed {
    pub fn resumed(kind: AgentKind) -> Self {
        match kind {
            AgentKind::Codex => Self::Reviewer,
            AgentKind::Claude | AgentKind::DeepSeek => Self::Defaults,
        }
    }
}

/// Native path identity also handles case and separator differences on Windows.
pub fn directories_match(left: Option<&str>, right: Option<&str>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => {
            path_identity(Path::new(left)) == path_identity(Path::new(right))
        }
        // Missing directory metadata does not establish a different workspace.
        _ => true,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplayRead {
    generation: u64,
    epoch: u64,
    cwd: Option<String>,
    identity: RecoveryIdentity,
}

/// A conversation read from disk for a restore: its turns, and the name its
/// transcript records for it.
pub struct LoadedReplay {
    pub turns: Vec<ReplayTurn>,
    pub title: Option<String>,
}

impl ReplayRead {
    /// Synchronous disk work; the caller chooses its background executor.
    pub fn load(&self) -> Result<LoadedReplay, String> {
        sessions::try_load_replay(self.cwd.as_deref(), &self.identity.id)
    }
}

pub enum ResumeStart {
    Busy,
    Elsewhere { cwd: String, session_id: String },
    Rejected,
    Requested,
    ReadReplay(ReplayRead),
}

pub enum ReplayLoaded {
    Stale,
    Cancelled,
    Failed(String),
    Restart(RecoveryIdentity),
}

pub enum ReadyAction {
    Ignore,
    Apply,
    Replay(Vec<ReplayTurn>),
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ReplayAction {
    Ignore,
    Append,
    Replace,
}

enum PendingRestore {
    Reading {
        request: ReplayRead,
        previous: Status,
    },
    Prepared {
        identity: RecoveryIdentity,
        replay: Vec<ReplayTurn>,
    },
    AwaitingReady {
        epoch: u64,
        replay: Vec<ReplayTurn>,
    },
    AwaitingReplay {
        epoch: u64,
        previous: Status,
    },
}

#[derive(Default)]
pub struct ConversationRestore {
    generation: u64,
    pending: Option<PendingRestore>,

    /// Name of the conversation the latest restore switches to, as the
    /// history list showed it. Most harnesses report no name on resume, so
    /// this is what names the tab once the switch lands. Every successful
    /// restore passes through `begin`, which replaces it, so a failed
    /// restore's name never reaches a later one.
    title: Option<String>,
}

impl ConversationRestore {
    pub fn begin(
        &mut self,
        runtime: &mut SessionRuntime,
        kind: AgentKind,
        summary: &SessionSummary,
        cwd: Option<&str>,
    ) -> ResumeStart {
        if self.pending.is_some() {
            return ResumeStart::Busy;
        }

        if !directories_match(summary.cwd.as_deref(), cwd) {
            return ResumeStart::Elsewhere {
                cwd: summary.cwd.clone().expect("different directory is present"),
                session_id: summary.id.clone(),
            };
        }

        let previous = runtime.begin_conversation_change();

        self.title = Some(summary.title.trim())
            .filter(|title| !title.is_empty())
            .map(str::to_owned);

        let outcome = match runtime.backend_mut() {
            Some(backend) => backend.resume_thread(&summary.id),
            None => ResumeOutcome::without_session(kind),
        };

        match outcome {
            ResumeOutcome::Rejected => {
                runtime.conversation_change_rejected(previous);

                ResumeStart::Rejected
            }
            ResumeOutcome::SwitchedInPlace => {
                self.pending = Some(PendingRestore::AwaitingReplay {
                    epoch: runtime.epoch(),
                    previous,
                });

                ResumeStart::Requested
            }
            ResumeOutcome::NeedsReplayRead => {
                self.generation = self
                    .generation
                    .checked_add(1)
                    .expect("restore generation exhausted");

                let request = ReplayRead {
                    generation: self.generation,
                    epoch: runtime.epoch(),
                    cwd: cwd.map(str::to_owned),
                    identity: RecoveryIdentity::new(kind, summary.id.clone()),
                };

                self.pending = Some(PendingRestore::Reading {
                    request: request.clone(),
                    previous,
                });

                ResumeStart::ReadReplay(request)
            }
        }
    }

    pub(crate) fn loaded(
        &mut self,
        runtime: &mut SessionRuntime,
        request: ReplayRead,
        cwd: Option<&str>,
        replay: Result<LoadedReplay, String>,
    ) -> ReplayLoaded {
        let Some(PendingRestore::Reading {
            request: active,
            previous,
        }) = &self.pending
        else {
            return ReplayLoaded::Stale;
        };

        if active != &request {
            return ReplayLoaded::Stale;
        }

        let previous = *previous;

        if !runtime.is_current(request.epoch) || request.cwd.as_deref() != cwd {
            self.pending = None;

            if runtime.is_current(request.epoch) {
                runtime.conversation_change_rejected(previous);

                return ReplayLoaded::Cancelled;
            }

            return ReplayLoaded::Stale;
        }

        match replay {
            Ok(replay) => {
                // A restore that names no conversation, such as a tab
                // reopening on launch, takes the name its transcript records.
                self.title = self.title.take().or(replay.title);

                self.pending = Some(PendingRestore::Prepared {
                    identity: request.identity.clone(),
                    replay: replay.turns,
                });

                ReplayLoaded::Restart(request.identity)
            }
            Err(message) => {
                self.pending = None;

                runtime.conversation_change_rejected(previous);

                ReplayLoaded::Failed(message)
            }
        }
    }

    /// A matching restart transfers the loaded buffer; every other start retires it.
    pub fn starting(&mut self, epoch: u64, recovery: Option<&RecoveryIdentity>) {
        self.pending = match self.pending.take() {
            Some(PendingRestore::Prepared { identity, replay }) if recovery == Some(&identity) => {
                Some(PendingRestore::AwaitingReady { epoch, replay })
            }
            _ => None,
        };
    }

    /// Take the local replay once, only after its replacement session is ready.
    pub fn ready(&mut self, epoch: u64) -> ReadyAction {
        match &self.pending {
            None => ReadyAction::Apply,
            Some(PendingRestore::AwaitingReplay { epoch: active, .. }) if *active == epoch => {
                ReadyAction::Apply
            }
            Some(PendingRestore::AwaitingReady { epoch: active, .. }) if *active == epoch => {
                let Some(PendingRestore::AwaitingReady { replay, .. }) = self.pending.take() else {
                    unreachable!();
                };

                ReadyAction::Replay(replay)
            }
            // The old backend can finish its handshake while disk work runs.
            // Its settings must not release the pending conversation change.
            _ => ReadyAction::Ignore,
        }
    }

    pub(crate) fn replayed(&mut self, epoch: u64) -> ReplayAction {
        match &self.pending {
            None => ReplayAction::Append,
            Some(PendingRestore::AwaitingReplay { epoch: active, .. }) if *active == epoch => {
                self.pending = None;

                ReplayAction::Replace
            }
            _ => ReplayAction::Ignore,
        }
    }

    /// Rejecting an in-session operation restores the status it displaced.
    /// A failed replacement keeps the runtime's startup failure instead.
    pub fn failed(&mut self, runtime: &mut SessionRuntime) -> bool {
        let Some(pending) = self.pending.take() else {
            return false;
        };

        let prior = match pending {
            PendingRestore::Reading { request, previous } => Some((request.epoch, previous)),
            PendingRestore::AwaitingReplay { epoch, previous } => Some((epoch, previous)),
            PendingRestore::AwaitingReady { epoch, .. } if runtime.status() == Status::Starting => {
                Some((epoch, Status::Idle))
            }
            _ => None,
        };

        if let Some((epoch, previous)) = prior
            && runtime.is_current(epoch)
        {
            runtime.conversation_change_rejected(previous);
        }

        true
    }

    pub(crate) fn cancel(&mut self) {
        self.pending = None;
    }

    /// The restored conversation's name, released once its replay replaced
    /// the conversation.
    pub(crate) fn take_title(&mut self) -> Option<String> {
        self.title.take()
    }
}
