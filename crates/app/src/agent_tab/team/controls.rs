use nmt_agent::chat::ThreadSettings;
use nmt_agent::team::content::UserInput;
use nmt_agent::team::discussion::DiscussionMode;
use nmt_agent::team::identity::{AttemptId, DiscussionId, MemberId, OperationId};

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
