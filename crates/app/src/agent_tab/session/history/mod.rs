//! The recent-conversation list: which conversations a tab offers to reopen,
//! where that list comes from, and what reopening one does.
//!
//! A harness that lists over the protocol is asked; one that keeps its
//! transcripts on disk is read here instead, which is why the list has a
//! loading shape of its own rather than simply arriving.

pub(crate) use nmt_agent::session::history::FilesystemHistoryRequest;

#[cfg(test)]
mod restore_tests;
#[cfg(test)]
mod tests;

use gpui::{Pixels, Point};
use gpui_component::VirtualListScrollHandle;
use nmt_agent::chat::SessionSummary;
use nmt_agent::session::history::{CountPublication, SessionHistory};

use crate::agent_tab::fade::Fade;

impl SessionHistoryUi {
    pub(crate) fn publish_filesystem_count(
        &mut self,
        request: &FilesystemHistoryRequest,
        cwd: Option<&str>,
        epoch: u64,
        count: usize,
    ) -> CountPublication {
        let result = self
            .data
            .publish_filesystem_count(request, cwd, epoch, count);

        if matches!(result, CountPublication::Empty) {
            self.selected = 0;
        }

        result
    }

    pub(crate) fn publish_filesystem_rows(
        &mut self,
        request: &FilesystemHistoryRequest,
        cwd: Option<&str>,
        epoch: u64,
        rows: Vec<SessionSummary>,
    ) -> bool {
        if !self.data.publish_filesystem_rows(request, cwd, epoch, rows) {
            return false;
        }

        self.selected = self
            .selected
            .min(self.data.sessions.len().saturating_sub(1));

        true
    }

    /// Hand the highlight to the row under the pointer, reporting whether the
    /// highlight moved.
    ///
    /// Guarded on the pointer having actually moved. Keyboard navigation
    /// scrolls the list to keep its row in view, which slides a different row
    /// under a pointer resting over the strip; letting that count as pointing
    /// would take the highlight straight back off the arrow keys. Real
    /// movement takes it back unconditionally, because a reader who has picked
    /// the pointer up again is looking at where the pointer is.
    pub(crate) fn point_at(&mut self, index: usize, position: Point<Pixels>) -> bool {
        if self.pointer == Some(position) {
            return false;
        }

        self.pointer = Some(position);

        if self.selected == index {
            return false;
        }

        self.selected = index;

        true
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum RecentSessionsMode {
    #[default]
    Automatic,
    Hidden,
    Open,
    Loading,
}

impl RecentSessionsMode {
    /// The automatic list is a blank tab's default surface, and a composer
    /// with anything in it -- typed text or the placeholder a pasted image
    /// leaves -- means the tab is being used for a new conversation, so the
    /// list steps aside and comes back once the composer is empty again. An
    /// explicit `/resume` list stays up over text: typing into it narrows
    /// the rows.
    pub(crate) fn is_visible(
        self,
        transcript_empty: bool,
        composer_empty: bool,
        rows: usize,
    ) -> bool {
        rows > 0
            && match self {
                Self::Automatic => transcript_empty && composer_empty,
                Self::Open => true,
                Self::Hidden | Self::Loading => false,
            }
    }

    /// An outside click dismisses only an explicit `/resume` list. The
    /// automatic list on a blank tab is that tab's default surface, so a
    /// click on the empty pane keeps it open; hiding it there would strand
    /// the tab with no way back except `/resume`.
    pub(crate) fn dismisses_on_outside_click(self) -> bool {
        !matches!(self, Self::Automatic)
    }
}

/// Recent-session list shown above the composer.
pub(crate) struct SessionHistoryUi {
    pub(crate) data: SessionHistory,

    /// Blank conversations show the list automatically; `/resume` can reopen
    /// the same list after a conversation has started.
    pub(crate) mode: RecentSessionsMode,

    /// The one highlighted row, whether the pointer or the arrow keys put it
    /// there. A list has a single current row: what a click opens and what
    /// Enter opens are the same row, and only one thing on screen says so.
    pub(crate) selected: usize,

    /// Whether the pointer is over the list. A search narrows the rows while
    /// the arrow keys still belong to the input, so the keyboard's highlight
    /// is not drawn then; a pointer over the list is reason enough to draw it,
    /// because the row under the pointer is what a click would open.
    pub(crate) pointer_inside: bool,

    /// Where the pointer last was over the list, so a row sliding under a
    /// pointer that has not moved cannot take the highlight back. Keyboard
    /// navigation scrolls the list, which does exactly that.
    pub(crate) pointer: Option<Point<Pixels>>,

    pub(crate) scroll: VirtualListScrollHandle,
    pub(crate) transcript_blur: Fade,
}

impl Default for SessionHistoryUi {
    fn default() -> Self {
        Self {
            data: SessionHistory::default(),
            mode: RecentSessionsMode::Automatic,
            selected: 0,
            pointer_inside: false,
            pointer: None,
            scroll: VirtualListScrollHandle::new(),
            transcript_blur: Fade::default(),
        }
    }
}
