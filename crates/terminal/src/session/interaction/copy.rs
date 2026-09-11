use futures::channel::oneshot;

use crate::render_buffer::RenderBuffer;
use crate::selection::SelectionRange;
use crate::session::interaction::TerminalInteraction;
use crate::session::request::Request;
use crate::session::{BlockPoint, TerminalSession};

#[derive(Debug)]
enum CopiedSelection {
    Live(SelectionRange),
    Frozen(BlockPoint, BlockPoint),
    FrozenPending,
    None,
}

/// Retains the selection that supplied a copy without letting a host mutate it.
#[derive(Debug)]
pub struct CopyCompletion {
    selection: CopiedSelection,
    generation: u64,
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

impl TerminalInteraction {
    pub fn copy_selection(
        &self,
        session: &TerminalSession,
        snapshot: &RenderBuffer,
    ) -> Option<PendingCopy> {
        let (request, selection) = if let Some(pending) = &self.pending_expansion {
            (pending.copy_text(session), CopiedSelection::FrozenPending)
        } else if let Some((a, b)) = self.frozen.current() {
            (
                session.frozen_selection_text(a, b),
                CopiedSelection::Frozen(a, b),
            )
        } else {
            let range = session.selection_range_in(snapshot)?;
            (
                session.selected_text_in(snapshot)?,
                CopiedSelection::Live(range),
            )
        };
        Some(PendingCopy {
            request,
            completion: CopyCompletion {
                selection,
                generation: self.selection_generation,
            },
        })
    }

    /// Clear the copied selection only after the host accepts the text. A
    /// later pointer gesture owns its selection even if the old read finishes.
    pub fn complete_copy(
        &mut self,
        session: &TerminalSession,
        snapshot: &RenderBuffer,
        completion: CopyCompletion,
    ) {
        if completion.generation != self.selection_generation {
            return;
        }
        match completion.selection {
            CopiedSelection::FrozenPending => {
                self.pending_expansion = None;
                self.frozen.clear();
            }
            CopiedSelection::Frozen(a, b) if self.frozen.current() == Some((a, b)) => {
                self.frozen.clear();
            }
            CopiedSelection::Live(range) if session.selection_range_in(snapshot) == Some(range) => {
                session.clear_selection();
            }
            _ => {}
        }
    }
}
