use futures::channel::oneshot;

use crate::selection::SelectionRange;
use crate::session::BlockPoint;
use crate::session::request::Request;

#[derive(Debug)]
pub(super) enum CopiedSelection {
    Live(SelectionRange),
    Frozen(BlockPoint, BlockPoint),
    FrozenPending,
    None,
}

/// Retains the selection that supplied a copy without letting a host mutate it.
#[derive(Debug)]
pub struct CopyCompletion {
    pub(super) selection: CopiedSelection,
    pub(super) generation: u64,
}

#[derive(Debug)]
pub struct PendingCopy {
    pub request: Request<String>,
    pub completion: CopyCompletion,
}

impl PendingCopy {
    pub fn ready(text: String) -> Self {
        let (reply, request) = oneshot::channel();
        let _ = reply.send(Ok(text));
        Self::from_request(request)
    }

    pub fn from_request(request: Request<String>) -> Self {
        Self {
            request,
            completion: CopyCompletion {
                selection: CopiedSelection::None,
                generation: 0,
            },
        }
    }
}
