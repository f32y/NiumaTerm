//! The two cards that block a turn until the user answers them.
//!
//! An approval request and an `AskUserQuestion` batch are the same shape of
//! interruption: the backend stops, the pane draws a card above the input, and
//! the answer goes back over the session. At most one of each can be open, and
//! a session replacement drops both, so they are held together.

use crate::QuestionPrompt;
use crate::composer::PaletteControl;

/// What one keyboard press did to the question card.
pub(crate) enum QuestionControl {
    /// The card did not use the key; the caller falls through to the surfaces
    /// that share it.
    Ignored,
    /// The highlight moved, so the card needs redrawing.
    Moved,
    /// The highlighted option was picked.
    Toggled { question: usize, option: usize },
}

#[derive(Default)]
pub(crate) struct PendingPrompts {
    /// Description of the approval request blocking the turn, shown as the
    /// card above the input; the request id lives in the session.
    approval: Option<String>,
    /// Questions the model wants answered before it continues, plus the
    /// selection the user has built so far. The request id lives in the
    /// session, so this holds only what the card renders.
    questions: Option<QuestionPrompt>,
}

impl PendingPrompts {
    pub(crate) fn approval(&self) -> Option<&str> {
        self.approval.as_deref()
    }

    pub(crate) fn questions(&self) -> Option<&QuestionPrompt> {
        self.questions.as_ref()
    }

    pub(crate) fn approval_open(&self) -> bool {
        self.approval.is_some()
    }

    pub(crate) fn questions_open(&self) -> bool {
        self.questions.is_some()
    }

    pub(crate) fn ask_approval(&mut self, description: String) {
        self.approval = Some(description);
    }

    pub(crate) fn ask_questions(&mut self, prompt: QuestionPrompt) {
        self.questions = Some(prompt);
    }

    pub(crate) fn dismiss_approval(&mut self) {
        self.approval = None;
    }

    pub(crate) fn dismiss_questions(&mut self) {
        self.questions = None;
    }

    /// Record a pick without answering yet; the card stays open until the user
    /// submits, so multi-select questions can accumulate choices. Clicking also
    /// moves the highlight, so a switch to the keyboard continues from the
    /// option the user just touched rather than from wherever the arrows were
    /// left.
    pub(crate) fn toggle_option(&mut self, question: usize, option: usize) -> bool {
        let Some(prompt) = self.questions.as_mut() else {
            return false;
        };

        prompt.toggle(question, option);
        prompt.focus = (question, option);

        true
    }

    /// Drive the card from the keyboard.
    ///
    /// Enter answers the highlighted option rather than submitting the card:
    /// with several questions, or a multi-select one, the user is rarely done
    /// after one press, and a key that sometimes submits and sometimes selects
    /// cannot be predicted from what is on screen.
    pub(crate) fn handle_control(&mut self, control: PaletteControl) -> QuestionControl {
        let Some(prompt) = self.questions.as_mut() else {
            return QuestionControl::Ignored;
        };

        match control {
            PaletteControl::Previous | PaletteControl::Next => {
                match prompt.move_focus(matches!(control, PaletteControl::Next)) {
                    true => QuestionControl::Moved,
                    false => QuestionControl::Ignored,
                }
            }
            PaletteControl::Activate => {
                let (question, option) = prompt.focus;

                QuestionControl::Toggled { question, option }
            }
            // Completion belongs to the composer, and dismissing the card would
            // answer the question by refusing it, which needs the visible
            // control rather than a keystroke.
            PaletteControl::Complete | PaletteControl::Dismiss => QuestionControl::Ignored,
        }
    }

    /// Take the card down and report the answers to send, or `None` when the
    /// user declined or left it incomplete.
    pub(crate) fn take_answers(&mut self, submit: bool) -> Option<Option<Vec<Vec<String>>>> {
        let prompt = self.questions.take()?;

        Some((submit && prompt.is_complete()).then(|| prompt.answers()))
    }
}
