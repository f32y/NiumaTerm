use gpui::Context;
use nmt_agent::chat::ThreadSettings;
use nmt_agent::team::content::UserInput;
use nmt_agent::team::discussion::{DiscussionMode, PauseReason};
use nmt_agent::team::identity::{AttemptId, DiscussionId, MemberId, OperationId};
use nmt_agent::team::session::TeamError;

use crate::agent_tab::team::TeamRuntime;
use crate::agent_tab::team::dispatch::{CONTEXT_LIMITS, work_status};

pub enum TeamCommand {
    Direct {
        input: UserInput,
        recipients: Vec<MemberId>,
    },

    Start {
        input: UserInput,
        participants: Vec<MemberId>,
        mode: DiscussionMode,
    },

    Correction(UserInput),
    Pause(DiscussionId),
    Continue(DiscussionId),

    AddTurns {
        discussion: DiscussionId,
        count: u32,
    },

    Finish(DiscussionId),

    Skip {
        discussion: DiscussionId,
        operation: OperationId,
    },

    ChangeMode {
        discussion: DiscussionId,
        mode: DiscussionMode,
    },

    AutomaticSummaries(bool),

    MemberSettings {
        member: MemberId,
        settings: ThreadSettings,
    },

    Exclude(MemberId),
    Stop(MemberId),
    AbandonRestored(AttemptId),
}

impl TeamRuntime {
    pub fn command(
        &mut self,
        command: TeamCommand,
        cx: &mut Context<Self>,
    ) -> Result<(), TeamError> {
        match command {
            TeamCommand::AbandonRestored(id) => {
                let attempt = self
                    .session
                    .pending_recovery()
                    .find(|attempt| attempt.id == id)
                    .ok_or(TeamError::Unresolved)?;

                if self
                    .hosts
                    .get(&attempt.intent.recipient)
                    .is_some_and(|host| {
                        host.active.is_some()
                            || work_status(host.owner.session().read(cx)) != Default::default()
                    })
                {
                    return Err(TeamError::Busy);
                }

                self.session.abandon_restored_attempt(id)?;
            }

            TeamCommand::Direct { input, recipients } => {
                self.session
                    .direct_request(input, recipients, &CONTEXT_LIMITS)?;
            }

            TeamCommand::Start {
                input,
                participants,
                mode,
            } => {
                self.session.start_discussion(input, participants, mode)?;
            }

            TeamCommand::Correction(input) => {
                self.session.record_user_input(input)?;
            }

            TeamCommand::Pause(id) => {
                self.session.pause_discussion(id, PauseReason::User)?;
            }

            TeamCommand::Continue(id) => self.session.continue_discussion(id)?,

            TeamCommand::AddTurns { discussion, count } => {
                self.session.add_turns(discussion, count)?
            }

            TeamCommand::Finish(id) => self.session.finish_with_report(id)?,

            TeamCommand::Skip {
                discussion,
                operation,
            } => self.session.skip_arrangement(discussion, operation)?,

            TeamCommand::ChangeMode { discussion, mode } => {
                self.session.change_mode(discussion, mode)?
            }

            TeamCommand::AutomaticSummaries(enabled) => {
                self.session.set_automatic_summaries(enabled)?
            }

            TeamCommand::MemberSettings { member, settings } => {
                let ownership = self
                    .room()
                    .member(member)
                    .ok_or(TeamError::Unavailable)?
                    .ownership();

                self.session
                    .set_member_settings(member, ownership, settings)?;

                let execution = self
                    .member_session(member)
                    .cloned()
                    .ok_or(TeamError::Unavailable)?;

                let settings = self
                    .room()
                    .member(member)
                    .ok_or(TeamError::Unavailable)?
                    .settings()
                    .clone();

                execution.update(cx, |session, cx| {
                    session.controller.borrow_mut().set_settings(settings);

                    cx.notify();
                });
            }

            TeamCommand::Exclude(member) => self.session.exclude_member(member)?,

            TeamCommand::Stop(member) => {
                let discussions: Vec<_> = self
                    .room()
                    .discussions()
                    .iter()
                    .map(|discussion| discussion.id())
                    .collect();

                for discussion in discussions {
                    self.session
                        .pause_discussion(discussion, PauseReason::User)?;
                }

                let execution = self
                    .member_session(member)
                    .cloned()
                    .ok_or(TeamError::Unavailable)?;

                execution.update(cx, |session, cx| {
                    session.controller.borrow_mut().interrupt_from_user();

                    cx.notify();
                });
            }
        }

        self.error = None;
        self.schedule(cx);

        Ok(())
    }
}
