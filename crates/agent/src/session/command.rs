//! What a view asks of a conversation, as values.
//!
//! Every change a view makes to shared conversation state goes through one
//! of these commands, each returning the outcome the view presents. A view
//! beside the session runs them directly; a view in another process sends
//! them to the host, which runs the same code, so the two paths cannot
//! drift apart. Per-view state (drafts in progress, pickers, scroll) stays
//! out of commands.

pub(crate) mod base64_bytes {
    use std::sync::Arc;

    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
    use serde::de::Error as _;
    use serde::{Deserialize, Deserializer, Serializer};

    pub(crate) fn serialize<S: Serializer>(
        bytes: &Arc<[u8]>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Arc<[u8]>, D::Error> {
        let text = String::deserialize(deserializer)?;

        STANDARD
            .decode(text)
            .map(Into::into)
            .map_err(D::Error::custom)
    }
}

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::chat::{SkillReference, SlashCommandOutcome, SlashCommandRunPolicy, ThreadSettings};
use crate::session::commands::{CommandAdmission, PendingSlashCommand};
use crate::session::controller::{SessionController, SubmissionBlock, UserInterruption};
use crate::session::delivery::RecoverablePrompt;
use crate::session::input::{ApprovalOutcome, QuestionAction, QuestionKey, Submission};
use crate::session::view::DraftAnswers;
use crate::session::{OperationError, SettingsOutcome};

pub trait AgentCommand: Serialize + DeserializeOwned {
    /// The name the command is sent under between processes.
    const METHOD: &'static str;

    type Outcome: Serialize + DeserializeOwned + Send + 'static;

    fn run(self, controller: &mut SessionController) -> Self::Outcome;
}

/// One image of a composed message. The bytes are sent as base64, because a
/// JSON message cannot hold raw bytes.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PromptImage {
    #[serde(with = "base64_bytes")]
    pub bytes: Arc<[u8]>,
    pub media_type: String,
}

/// A message as a view composed it, owning its data.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Prompt {
    /// What the harness receives, response annotations included.
    pub text: String,

    /// What the user typed, which names an unnamed conversation.
    pub title_text: String,

    /// The name a harness without title generation takes from the prompt.
    pub fallback_title: Option<String>,

    pub skill: Option<SkillReference>,
    pub images: Vec<PromptImage>,

    /// The images as files, for a harness that reads images by path. Paths
    /// name files on one machine, so the host writes its own.
    #[serde(skip)]
    pub image_paths: Vec<PathBuf>,

    /// What an interrupt before any answer gives back to the composer.
    pub recoverable: Option<RecoverablePrompt>,
}

/// A message the harness took.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Submitted {
    pub started_turn: bool,

    /// The title this prompt claimed for the conversation, shown until the
    /// harness generates one.
    pub title: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct SubmitPrompt(pub Prompt);

impl AgentCommand for SubmitPrompt {
    const METHOD: &'static str = "submit";

    type Outcome = Result<Submitted, SubmitRefusal>;

    fn run(self, controller: &mut SessionController) -> Self::Outcome {
        controller.submit_prompt(self.0)
    }
}

/// Why a message was not taken.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubmitRefusal {
    Blocked(SubmissionBlock),
    NotReady,
    Rejected { message: String },
}

#[derive(Serialize, Deserialize)]
pub struct Interrupt;

impl AgentCommand for Interrupt {
    const METHOD: &'static str = "interrupt";

    type Outcome = UserInterruption;

    fn run(self, controller: &mut SessionController) -> Self::Outcome {
        controller.interrupt_from_user()
    }
}

#[derive(Serialize, Deserialize)]
pub struct RespondApproval {
    pub decision: String,
}

impl AgentCommand for RespondApproval {
    const METHOD: &'static str = "respond_approval";

    type Outcome = ApprovalOutcome;

    fn run(self, controller: &mut SessionController) -> Self::Outcome {
        controller.respond_approval(&self.decision)
    }
}

/// Answer or skip a question batch with the answers the view composed.
#[derive(Serialize, Deserialize)]
pub struct AnswerQuestion {
    pub key: QuestionKey,
    pub action: QuestionAction,
    pub answers: DraftAnswers,
}

impl AgentCommand for AnswerQuestion {
    const METHOD: &'static str = "answer_question";

    type Outcome = Submission;

    fn run(self, controller: &mut SessionController) -> Self::Outcome {
        if let Some(draft) = controller.input_mut().draft_mut(self.key) {
            draft.set_answers(self.answers);
        }

        controller.submit_question(self.key, self.action, Instant::now())
    }
}

/// Replace the thread controls the next turn runs under.
#[derive(Serialize, Deserialize)]
pub struct UpdateSettings {
    pub settings: ThreadSettings,
}

impl AgentCommand for UpdateSettings {
    const METHOD: &'static str = "update_settings";

    type Outcome = ();

    fn run(self, controller: &mut SessionController) -> Self::Outcome {
        controller.controls.settings = self.settings;
    }
}

/// Adopt `settings` and push its model and effort to a harness that applies
/// them as their own request. `None` means no session is running to ask.
#[derive(Serialize, Deserialize)]
pub struct ApplyModelSelection {
    pub settings: ThreadSettings,
}

impl AgentCommand for ApplyModelSelection {
    const METHOD: &'static str = "apply_model_selection";

    type Outcome = Option<SettingsOutcome>;

    fn run(self, controller: &mut SessionController) -> Self::Outcome {
        controller.controls.settings = self.settings;

        controller.apply_model_selection()
    }
}

#[derive(Serialize, Deserialize)]
pub struct SelectAgentPreset {
    pub preset: String,
}

impl AgentCommand for SelectAgentPreset {
    const METHOD: &'static str = "select_agent_preset";

    type Outcome = Option<SettingsOutcome>;

    fn run(self, controller: &mut SessionController) -> Self::Outcome {
        controller.select_agent_preset(self.preset)
    }
}

#[derive(Serialize, Deserialize)]
pub struct WithdrawQueuedPrompt {
    pub item_id: String,
}

impl AgentCommand for WithdrawQueuedPrompt {
    const METHOD: &'static str = "withdraw_queued_prompt";

    type Outcome = bool;

    fn run(self, controller: &mut SessionController) -> Self::Outcome {
        controller.withdraw_queued_prompt(&self.item_id)
    }
}

#[derive(Serialize, Deserialize)]
pub struct PromoteQueuedPrompt {
    pub item_id: String,
}

impl AgentCommand for PromoteQueuedPrompt {
    const METHOD: &'static str = "promote_queued_prompt";

    type Outcome = bool;

    fn run(self, controller: &mut SessionController) -> Self::Outcome {
        controller.promote_queued_prompt(&self.item_id)
    }
}

#[derive(Serialize, Deserialize)]
pub struct RenameConversation {
    pub title: String,
}

impl AgentCommand for RenameConversation {
    const METHOD: &'static str = "rename";

    type Outcome = Option<Result<String, OperationError>>;

    fn run(self, controller: &mut SessionController) -> Self::Outcome {
        controller.rename_conversation(&self.title)
    }
}

#[derive(Serialize, Deserialize)]
pub struct RunSlashCommand {
    pub command: PendingSlashCommand,
}

impl AgentCommand for RunSlashCommand {
    const METHOD: &'static str = "run_slash_command";

    type Outcome = SlashCommandOutcome;

    fn run(self, controller: &mut SessionController) -> Self::Outcome {
        controller.execute_command(&self.command)
    }
}

/// Run or queue a slash command typed while a turn is running.
#[derive(Serialize, Deserialize)]
pub struct AdmitSlashCommand {
    pub command: PendingSlashCommand,
    pub policy: SlashCommandRunPolicy,
}

impl AgentCommand for AdmitSlashCommand {
    const METHOD: &'static str = "admit_slash_command";

    type Outcome = CommandAdmission;

    fn run(self, controller: &mut SessionController) -> Self::Outcome {
        controller.admit_command_while_busy(self.command, self.policy)
    }
}

/// The call a view in another process starts a new conversation with. It is
/// not an [`AgentCommand`]: replacing the conversation restarts the harness
/// process, which the session owner does instead of the controller. The
/// outcome is `Result<(), String>`, the message of a refusal.
pub const NEW_CONVERSATION_METHOD: &str = "new_conversation";

/// Why a command sent from another process did not run.
#[derive(Debug, thiserror::Error)]
pub enum RemoteCommandError {
    #[error("unknown command {0}")]
    Unknown(String),
    #[error("invalid command parameters: {0}")]
    Invalid(#[from] serde_json::Error),
}

/// Run a command a view in another process sent, returning its outcome as
/// it is sent back. `stage_images` writes a prompt's images to files for a
/// harness that reads them by path, and returns the paths.
pub fn run_remote(
    method: &str,
    params: Value,
    controller: &mut SessionController,
    stage_images: impl FnOnce(&[PromptImage]) -> Vec<PathBuf>,
) -> Result<Value, RemoteCommandError> {
    fn run<C: AgentCommand>(
        params: Value,
        controller: &mut SessionController,
    ) -> Result<Value, RemoteCommandError> {
        let command: C = serde_json::from_value(params)?;

        Ok(serde_json::to_value(command.run(controller))?)
    }

    match method {
        SubmitPrompt::METHOD => {
            let SubmitPrompt(mut prompt) = serde_json::from_value(params)?;

            prompt.image_paths = stage_images(&prompt.images);

            Ok(serde_json::to_value(SubmitPrompt(prompt).run(controller))?)
        }
        Interrupt::METHOD => run::<Interrupt>(params, controller),
        RespondApproval::METHOD => run::<RespondApproval>(params, controller),
        AnswerQuestion::METHOD => run::<AnswerQuestion>(params, controller),
        UpdateSettings::METHOD => run::<UpdateSettings>(params, controller),
        ApplyModelSelection::METHOD => run::<ApplyModelSelection>(params, controller),
        SelectAgentPreset::METHOD => run::<SelectAgentPreset>(params, controller),
        WithdrawQueuedPrompt::METHOD => run::<WithdrawQueuedPrompt>(params, controller),
        PromoteQueuedPrompt::METHOD => run::<PromoteQueuedPrompt>(params, controller),
        RenameConversation::METHOD => run::<RenameConversation>(params, controller),
        RunSlashCommand::METHOD => run::<RunSlashCommand>(params, controller),
        AdmitSlashCommand::METHOD => run::<AdmitSlashCommand>(params, controller),
        _ => Err(RemoteCommandError::Unknown(method.to_owned())),
    }
}
