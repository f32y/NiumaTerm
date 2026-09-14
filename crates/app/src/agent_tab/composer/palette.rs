use std::rc::Rc;
use std::time::Duration;

use gpui::{Context, Pixels, ScrollHandle, SharedString, px};
use nmt_agent::chat::{ForkCheckpoint, SkillCatalog, SkillInfo, SkillReference, SlashCommandInfo};
use nmt_agent::claude_code::sessions;
use nmt_agent::session::commands::CommandQueue;

use crate::agent_tab::AgentPane;
use crate::agent_tab::composer::{CommandFeedback, CommandFeedbackKind, RewindAction};

/// Tallest the palette grows before its own rows scroll: nine rows and the
/// note under them. The transcript reads this as the height the picker covers
/// when it floats over the bottom of the pane.
pub(crate) const PALETTE_MAX_HEIGHT: Pixels = px(9. * 48. + 36.);

#[derive(Clone)]
pub(crate) enum PaletteAction {
    Command(SlashCommandInfo),
    Choice { command: String, value: String },
    Skill(SkillInfo),
    RewindCheckpoint(sessions::ClaudeCheckpoint),
    RewindAction(RewindAction),
    ForkCheckpoint(ForkCheckpoint),
    ForkCancel,
}

/// One drawn palette row. The text is `SharedString` because the model is
/// rebuilt on every frame the palette paints: catalog text taken straight from
/// the translation catalogs borrows instead of copying, and text that is
/// composed still reaches `child` without a second copy.
#[derive(Clone)]
pub(crate) struct PaletteRow {
    pub(crate) label: SharedString,
    pub(crate) description: SharedString,
    pub(crate) hint: Option<SharedString>,
    pub(crate) disabled_reason: Option<SharedString>,
    pub(crate) action: PaletteAction,
}

pub(crate) struct PaletteModel {
    pub(crate) rows: Vec<PaletteRow>,
    pub(crate) note: Option<SharedString>,
}

/// The merged `/` catalog, held so it is not rebuilt from the local, adapter,
/// and provider lists on every frame the palette paints. `language` is part of
/// the key because local entries carry translated descriptions and the user can
/// switch language while a pane is open.
pub(crate) struct CachedCatalog {
    pub(crate) language: String,
    pub(crate) commands: Rc<[SlashCommandInfo]>,
}

/// Slash-command palette, skill picker, and pending-command state.
#[derive(Default)]
pub(crate) struct SlashPalette {
    /// Provider discovery is a replacement snapshot; adapter/local entries
    /// remain available independently of whether discovery has arrived.
    pub(crate) provider_commands: Vec<SlashCommandInfo>,

    pub(crate) provider_commands_ready: bool,

    /// Derived from `provider_commands`; every write to that list must drop
    /// this, or the palette keeps offering commands the harness has withdrawn.
    pub(crate) catalog: Option<CachedCatalog>,

    /// `None` means Codex discovery is still loading. A populated catalog can
    /// contain both usable skills and non-fatal per-file errors.
    pub(crate) skill_catalog: Option<SkillCatalog>,

    /// Exact picker identity retained while the composer keeps its `$name`
    /// token. It is validated against `skill_catalog` before every send.
    pub(crate) skill_binding: Option<SkillReference>,

    pub(crate) selected: usize,
    pub(crate) dismissed: bool,
    pub(crate) scroll: ScrollHandle,
    pub(crate) feedback: Option<CommandFeedback>,

    /// Order of the latest feedback shown. A delayed dismissal compares
    /// against it so it can only retire the message it was started for.
    pub(crate) feedback_seq: u64,
}

impl SlashPalette {
    /// Provider commands and their cached catalog belong to one session, so
    /// resetting discovery must invalidate both together.
    pub(crate) fn reset_discovery(&mut self, commands_ready: bool) {
        self.provider_commands.clear();

        self.provider_commands_ready = commands_ready;
        self.catalog = None;
        self.selected = 0;
        self.dismissed = false;
    }

    /// Stand down for text the composer did not type. A recalled entry is a
    /// whole message, so a skill bound to what was there no longer applies and
    /// the palette must not reopen on the leading `/` the entry may carry.
    pub(crate) fn reset_for_recall(&mut self) {
        self.skill_binding = None;
        self.dismissed = true;
        self.selected = 0;
    }

    /// Show a one-line result above the composer, replacing whatever was
    /// there.
    pub(crate) fn set_feedback(
        &mut self,
        kind: CommandFeedbackKind,
        message: impl Into<SharedString>,
        cx: &mut Context<AgentPane>,
    ) {
        self.feedback_seq += 1;

        let seq = self.feedback_seq;

        self.feedback = Some(CommandFeedback {
            kind,
            message: message.into(),
        });

        cx.notify();

        if !feedback_is_transient(kind) {
            return;
        }

        // A notice acknowledges a request before anything visible happens. A
        // command that then runs a whole turn fills the transcript with its
        // real answer, and the acknowledgement above the composer becomes a
        // line the user cannot dismiss, because only typing clears it.
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(FEEDBACK_LIFETIME).await;

            let _ = this.update(cx, |this, cx| {
                if this.palette.feedback_seq == seq {
                    this.palette.feedback = None;

                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// The message worth showing right now, if any.
    pub(crate) fn visible_feedback(&self, commands: &CommandQueue) -> Option<&CommandFeedback> {
        self.feedback
            .as_ref()
            .filter(|feedback| feedback_is_current(feedback.kind, commands.queue.is_empty()))
    }

    /// Whether the discovered skill catalog carries this name.
    pub(crate) fn names_a_skill(&self, name: &str) -> bool {
        self.skill_catalog
            .as_ref()
            .is_some_and(|catalog| catalog.skills.iter().any(|skill| skill.name == name))
    }
}

/// How long an acknowledgement stays before retiring itself. Long enough to
/// read after a glance away, short enough that it does not outlive the command
/// it describes.
const FEEDBACK_LIFETIME: Duration = Duration::from_secs(6);

/// Whether a message still describes the situation. A queued message counts
/// the command queue, and several paths empty that queue without going through
/// the palette -- a failed spawn, an update stopping active work, a
/// conversation reset. Deciding this where the message is shown keeps a future
/// path from reintroducing a count of commands that are no longer waiting.
pub(super) fn feedback_is_current(kind: CommandFeedbackKind, queue_is_empty: bool) -> bool {
    !(kind == CommandFeedbackKind::Queued && queue_is_empty)
}

/// Whether a message is a passing acknowledgement rather than something the
/// user still has to act on. An error stays until it is read, and a queued
/// list describes work still waiting rather than work already accepted.
pub(super) fn feedback_is_transient(kind: CommandFeedbackKind) -> bool {
    match kind {
        CommandFeedbackKind::Notice => true,
        CommandFeedbackKind::Status | CommandFeedbackKind::Error | CommandFeedbackKind::Queued => {
            false
        }
    }
}
