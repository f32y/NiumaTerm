use nmt_terminal::session::BlockPoint as FrozenPoint;
use nmt_terminal::terminal::square::Wide;

use crate::frame::TerminalLine;

/// A selectable row rendered by the block list. Finished blocks use their
/// immutable block coordinates; the active block's history keeps the engine's
/// absolute SCREEN row so selection and copy remain owned by Ghostty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BlockListPoint {
    Frozen(FrozenPoint),
    LiveHistory { row: u32, col: u16 },
}

pub(super) fn expand_wide_span(
    line: &TerminalLine,
    (mut start, mut end): (u16, u16),
) -> (u16, u16) {
    for cell in line.cells() {
        if cell.wide != Wide::Wide {
            continue;
        }

        let spacer = cell.col.saturating_add(1);

        if start == spacer {
            start = cell.col;
        }

        if end == spacer {
            end = spacer.saturating_add(1);
        }
    }

    (start, end)
}
