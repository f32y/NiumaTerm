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
}

pub struct SessionReplay {
    pub branch: Option<SessionBranch>,
    pub replace: bool,
}
