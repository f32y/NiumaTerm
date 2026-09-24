use nmt_agent::team::discussion::DiscussionMode;
use nmt_agent::team::model::{AttemptId, DiscussionId, MemberId, UserInput};

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
    ChangeMode {
        discussion: DiscussionId,
        mode: DiscussionMode,
    },
    Exclude(MemberId),
    Stop(MemberId),
    AbandonRestored(AttemptId),
}
