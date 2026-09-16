//! One Team member's live agent session and the Team work it is carrying.

use gpui::{App, Subscription};
use nmt_agent::chat::{SendOutcome, TeamDecisionRequest, ThreadSettings};
use nmt_agent::session::delivery::Submission;
use nmt_agent::session::lifecycle::Status;
use nmt_agent::session::{ImageAttachment, RecoveryIdentity};
use nmt_agent::team::attempt::DispatchIntent;
use nmt_agent::team::execution_slots::WorkStatus;
use nmt_agent::team::model::{AttemptId, InteractionId};

use crate::agent_tab::composer::attachments::scratch_dir;
use crate::agent_tab::execution::SessionOwner;
use crate::agent_tab::team::dispatch::work_status;

pub(super) struct MemberHost {
    pub(super) owner: SessionOwner,

    /// The attempt this member was last sent and has not finished.
    pub(super) active: Option<AttemptId>,

    /// The interaction the member's session is waiting on, which holds the
    /// open discussions paused until it resolves.
    pub(super) interaction: Option<InteractionId>,

    /// The backend epoch the room last recorded this member ready for.
    pub(super) ready_epoch: Option<u64>,

    _subscriptions: Vec<Subscription>,
}

impl MemberHost {
    pub(super) fn new(owner: SessionOwner, subscriptions: Vec<Subscription>) -> Self {
        Self {
            owner,
            active: None,
            interaction: None,
            ready_epoch: None,
            _subscriptions: subscriptions,
        }
    }

    /// Whether the member still has Team work in flight or its session is
    /// doing anything at all, either of which rules out sending it more.
    pub(super) fn is_busy(&self, cx: &App) -> bool {
        self.active.is_some() || work_status(self.owner.session().read(cx)) != WorkStatus::default()
    }

    /// Start the member's session, resuming `recovery` when it has one.
    pub(super) fn start(&self, recovery: Option<RecoveryIdentity>, cx: &mut App) {
        self.owner.session().update(cx, |session, cx| {
            session.start(recovery, true, |_, _| {}, cx);
        });
    }

    /// Make `settings` the settings the member's next turn runs with.
    pub(super) fn apply_settings(&self, settings: ThreadSettings, cx: &mut App) {
        self.owner.session().update(cx, |session, cx| {
            session
                .controller
                .borrow_mut()
                .controls
                .set_settings(settings);

            cx.notify();
        });
    }

    pub(super) fn interrupt(&self, cx: &mut App) {
        self.owner.session().update(cx, |session, cx| {
            session.controller.borrow_mut().interrupt_from_user();

            cx.notify();
        });
    }

    /// Send `intent` as the member's next turn with `settings`, carrying the
    /// image bytes `attachments` read for the intent's attachment references.
    /// A session that is busy or suspended for an update is not ready for it.
    pub(super) fn submit(
        &self,
        intent: &DispatchIntent,
        settings: &ThreadSettings,
        attachments: &[Vec<u8>],
        cx: &mut App,
    ) -> SendOutcome {
        self.owner.session().update(cx, |session, cx| {
            let mut state = session.controller.borrow_mut();

            if state.runtime.status() != Status::Idle || state.runtime.update_suspension().is_some()
            {
                return SendOutcome::NotReady;
            }

            let scratch = scratch_dir(session.agent_route().as_str());

            let result = state.submit(
                intent.prepared_text.clone(),
                |backend, text| {
                    backend.send_user_message(
                        text,
                        settings,
                        None,
                        attachments
                            .iter()
                            .zip(&intent.attachments)
                            .map(|(bytes, reference)| ImageAttachment {
                                bytes,
                                media_type: &reference.media_type,
                            }),
                        &scratch,
                    )
                },
                || None,
            );

            cx.notify();

            match result {
                Ok(Submission::Started { .. }) => SendOutcome::StartedTurn,
                Ok(Submission::Queued) => SendOutcome::Steered,
                Ok(Submission::Rejected { message }) => SendOutcome::Rejected { message },
                Ok(Submission::NotReady) => SendOutcome::NotReady,
                Err(blocker) => {
                    tracing::warn!(?blocker, "team submission was blocked before sending");

                    SendOutcome::NotReady
                }
            }
        })
    }

    /// Tell the moderator whether the room saved the decision it requested.
    pub(super) fn respond_decision(
        &self,
        request: &TeamDecisionRequest,
        accepted: bool,
        cx: &mut App,
    ) {
        let explanation = if accepted {
            "The decision is saved. It will run after this moderator turn finishes."
        } else {
            "The decision was rejected. The discussion is paused for user review."
        };

        self.owner.session().update(cx, |session, cx| {
            if let Some(backend) = session.controller.borrow_mut().runtime.backend_mut() {
                backend.respond_team_decision(request, accepted, explanation);
            }

            cx.notify();
        });
    }
}
