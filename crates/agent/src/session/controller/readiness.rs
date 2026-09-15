use crate::session::branch::FileProgress;

pub struct SessionBranch {
    pub prompt: String,
    pub files: FileProgress,
    pub replayed: bool,
}

pub struct SessionReady {
    pub branch: Option<SessionBranch>,
    pub replaced: bool,
    pub selection: Option<Result<(), String>>,

    /// The outcome of sending a remembered permission preset to a harness
    /// that pinned its own default into the new conversation.
    pub approval: Option<Result<(), String>>,
}

pub struct SessionReplay {
    pub branch: Option<SessionBranch>,
    pub replace: bool,
}
