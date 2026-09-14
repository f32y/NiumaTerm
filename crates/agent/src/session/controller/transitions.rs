use crate::session::branch::BranchFailure;

pub struct SessionStart {
    pub epoch: u64,
    pub reset_branch: bool,
}

pub struct SessionFailure {
    pub branch: Option<BranchFailure>,
    pub resume_failed: bool,
    pub cancelled_commands: bool,
}
