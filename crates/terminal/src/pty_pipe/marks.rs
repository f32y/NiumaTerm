use std::{cell, path};

use tracing::warn;

use crate::event::{self, EventListener, TerminalEvent, WindowId};
use crate::ghostty::{self, GhosttyTerminal, mode};
use crate::prompt_sniffer::SnifferMark;

const BLOCK_BOUNDARY_CLEAR: &[u8] = b"\x1b[2J\x1b[3J\x1b[H";

/// Engine-blocks mode: the engine's live finished-block list, oldest first,
/// with current row counts — the payload of
/// [`crate::event::BlockEvent::EngineBlocksSync`]. Cheap FFI walk; called
/// under the engine lock on the PTY thread after finish/resize.
pub(super) fn engine_blocks_live_list(
    engine: &GhosttyTerminal,
) -> Vec<(ghostty::BlockHandle, usize)> {
    let count = engine.block_count();

    let mut live = Vec::with_capacity(count);

    for index in 0..count {
        if let Some(handle) = engine.block_at(index) {
            live.push((handle, engine.block_row_count(handle).unwrap_or(0)));
        }
    }

    live
}

/// Apply a recognized OSC 133 mark before any later PTY bytes reach the engine.
/// Launch cwd, block finalization, and clear handling depend on engine state at
/// this exact stream position.
pub(super) fn apply_sniffer_mark(
    engine_cell: &cell::RefCell<&mut GhosttyTerminal>,
    launch_cwd: &mut Option<Option<path::PathBuf>>,
    mark_seq: &mut u64,
    event_proxy: &impl EventListener,
    window_id: WindowId,
    engine_blocks: bool,
    mut mark: SnifferMark<'_>,
) {
    engine_cell.borrow_mut().write_vt(mark.bytes);

    if mark.prompt_started && mark.trusted {
        // The block IS the segment: only the sequence
        // number is registered here, for metadata
        // marriage at the eventual finish.
        *mark_seq += 1;
        event_proxy.send_event(TerminalEvent::PromptStarted, window_id);
    }

    if let Some(report) = mark.progress.take() {
        event_proxy.send_event(TerminalEvent::ProgressReport(report), window_id);
    }

    if let Some(mut start) = mark.command_started.take() {
        // ;C — latch the launch cwd while the engine still
        // holds THIS prompt's OSC 7, and surface the in-flight block.
        let cwd = engine_cell.borrow().current_directory();

        start.seq = *mark_seq;
        start.cwd = cwd.clone();

        *launch_cwd = Some(cwd);

        event_proxy.send_event(TerminalEvent::CommandStarted(start), window_id);
    }

    if let Some(mut cmd) = mark.command_finished.take() {
        // ;D — attach the launch metadata.
        let cwd = launch_cwd.take().unwrap_or(None);

        cmd.seq = *mark_seq;
        cmd.cwd = cwd;

        let in_alt_screen = engine_cell.borrow().mode(mode::ALT_SCREEN);

        if mark.trusted && !in_alt_screen && engine_blocks {
            // Engine-blocks mode freezes
            // the command into a finished engine block
            // (O(1)) and hands the HANDLE to the store —
            // rendering reads the block via `BlockRef`;
            // nothing is materialized. Budget eviction may
            // fire inside finish_block, so the same batch
            // carries the live list for the store to prune
            // against. Clearing the boundary gives ConPTY's
            // cursor model a fresh grid for the next command.
            let mut engine = engine_cell.borrow_mut();

            let events = match engine.finish_block() {
                Ok(Some(handle)) => {
                    let rows = engine.block_row_count(handle).unwrap_or(0);

                    vec![
                        event::BlockEvent::EngineBlock {
                            seq: *mark_seq,
                            handle,
                            rows,
                        },
                        event::BlockEvent::EngineBlocksSync(engine_blocks_live_list(&engine)),
                    ]
                }
                // Empty command: no block, no segment.
                Ok(None) => Vec::new(),
                Err(err) => {
                    warn!("finish_block failed: {err:?}");
                    Vec::new()
                }
            };

            if !events.is_empty() {
                event_proxy.send_event(TerminalEvent::BlockBatch(events), window_id);
            }

            engine.write_vt(BLOCK_BOUNDARY_CLEAR);
        }

        // Classic mode keeps one continuous grid:
        // no finish, no boundary clear — plain single
        // grid; only the metadata event fires.
        event_proxy.send_event(TerminalEvent::CommandFinished(cmd), window_id);
    }

    if mark.history_cleared && mark.trusted && engine_blocks {
        // ;K — the Clear-Host wrapper announces a user
        // A trusted clear drops every finished engine
        // block and wipe the active grid (the shell's
        // own clear follows through ConPTY).
        let in_alt_screen = engine_cell.borrow().mode(mode::ALT_SCREEN);

        if !in_alt_screen {
            engine_cell.borrow_mut().write_vt(BLOCK_BOUNDARY_CLEAR);

            engine_cell.borrow_mut().clear_blocks();

            event_proxy.send_event(
                TerminalEvent::BlockBatch(vec![event::BlockEvent::HistoryCleared]),
                window_id,
            );
        }
    }
}
