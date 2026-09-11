use std::sync::atomic::Ordering;

use nmt_platform::EventedPty;
use tracing::warn;

use crate::event::{EventListener, Msg, TerminalEvent};
use crate::ghostty::{BlockHandle, GhosttyTerminal};
use crate::pty_pipe::PtyPipe;
use crate::session::page::{PAGE_ROWS, PageSource, RowPage};
use crate::session::request::{Checkpoint, Query, RequestError, TextSource};
use crate::session::selection::block_selection_range;

impl<T: EventedPty + Send + 'static, U: EventListener + Send + 'static> PtyPipe<T, U> {
    pub(super) fn handle_request(&mut self, request: Msg) {
        match request {
            Msg::Scroll(delta) => {
                self.ghostty.scroll_viewport_delta(delta);
                self.publish_command();
            }
            Msg::ScrollTo(target) => {
                let scrollbar = self.ghostty.scrollbar();
                let target = target.min(scrollbar.total.saturating_sub(scrollbar.len));
                let delta = (i128::from(target) - i128::from(scrollbar.offset))
                    .clamp(isize::MIN as i128, isize::MAX as i128)
                    as isize;
                self.ghostty.scroll_viewport_delta(delta);
                self.publish_command();
            }
            Msg::ScrollToEnd => {
                self.ghostty.scroll_viewport_bottom();
                self.publish_command();
            }
            Msg::Theme(colors) => {
                self.ghostty.set_theme_colors(&colors);
                self.theme_revision = self.theme_revision.wrapping_add(1);
                self.publish_command();
            }
            Msg::CursorShape { shape, reply } => {
                let result = self
                    .ghostty
                    .set_default_cursor_shape(shape)
                    .map_err(|error| RequestError::Engine(error.to_string()));
                if result.is_ok() {
                    self.publish_command();
                }
                let _ = reply.send(result);
            }
            Msg::Query(query) => {
                answer_query(
                    &mut self.ghostty,
                    self.content_version.load(Ordering::Relaxed),
                    self.theme_revision,
                    query,
                );
                self.event_proxy
                    .send_event(TerminalEvent::ReadReady, self.window_id);
            }
            Msg::Checkpoint(request) => {
                let result = self
                    .ghostty
                    .format_vt_state()
                    .map(|vt| Checkpoint {
                        vt,
                        cols: self.ghostty.cols(),
                        rows: self.ghostty.rows(),
                    })
                    .map_err(|error| RequestError::Engine(error.to_string()));
                (request.0)(result);
            }
            Msg::Input(_) | Msg::Resize(_) | Msg::Shutdown => {
                unreachable!("handled by the PTY loop")
            }
        }
    }

    fn publish_command(&mut self) {
        self.content_version.fetch_add(1, Ordering::Relaxed);
        self.snapshot_pending = true;
        if let Err(error) = self.flush_engine_state(true) {
            warn!("failed to publish terminal update: {error}");
        }
    }
}

pub(crate) fn answer_query(
    engine: &mut GhosttyTerminal,
    current_revision: u64,
    theme_revision: u64,
    query: Query,
) {
    match query {
        Query::Image {
            handle,
            image_id,
            reply,
        } => {
            if reply.is_canceled() {
                return;
            }
            let result = engine
                .block_acquire(handle)
                .and_then(|block| {
                    (block.handle().generation == handle.generation)
                        .then(|| engine.block_image_pixels(&block, image_id))
                        .flatten()
                })
                .ok_or(RequestError::Unavailable);
            let _ = reply.send(result);
        }
        Query::Rows {
            source,
            start,
            reply,
        } => {
            if reply.is_canceled() {
                return;
            }
            let result = match source {
                PageSource::Screen { revision } if revision != current_revision => {
                    Err(RequestError::Stale)
                }
                PageSource::Block { theme, .. } if theme != theme_revision => {
                    Err(RequestError::Stale)
                }
                _ => read_page(engine, source, start),
            };
            let _ = reply.send(result);
        }
        Query::Text { source, reply } => {
            if reply.is_canceled() {
                return;
            }
            let result = match source {
                TextSource::BlockSelection {
                    handle,
                    line,
                    col,
                    kind,
                } => {
                    let result = engine.block_acquire(handle).and_then(|block| {
                        if block.handle().generation != handle.generation {
                            return None;
                        }
                        let (start, end) = block_selection_range(
                            &block,
                            &engine.color_palette(),
                            line,
                            col,
                            kind,
                        )?;
                        block.format_range_clamped(Some(start), Some(end), true, true)
                    });
                    result.ok_or(RequestError::Unavailable)
                }
                TextSource::Screen {
                    revision,
                    start,
                    end,
                    rectangle,
                } => {
                    if revision != current_revision {
                        Err(RequestError::Stale)
                    } else {
                        engine
                            .format_screen_range(start, end, rectangle, true, true)
                            .map_err(|error| RequestError::Engine(error.to_string()))
                    }
                }
                TextSource::Blocks(pieces) => {
                    let mut text = String::new();
                    let result = pieces
                        .into_iter()
                        .enumerate()
                        .try_for_each(|(index, piece)| {
                            let block = engine
                                .block_acquire(piece.handle)
                                .ok_or(RequestError::Unavailable)?;
                            if block.handle().generation != piece.handle.generation {
                                return Err(RequestError::Stale);
                            }
                            let part = block
                                .format_range_clamped(piece.start, piece.end, true, true)
                                .ok_or(RequestError::Unavailable)?;
                            if index > 0 {
                                text.push('\n');
                            }
                            text.push_str(&part);
                            Ok(())
                        });
                    result.map(|()| text)
                }
            };
            let _ = reply.send(result);
        }
        Query::ExpandSelection {
            handle,
            line,
            col,
            kind,
            reply,
        } => {
            if reply.is_canceled() {
                return;
            }
            let result = engine
                .block_acquire(handle)
                .and_then(|block| {
                    (block.handle().generation == handle.generation)
                        .then(|| {
                            block_selection_range(&block, &engine.color_palette(), line, col, kind)
                        })
                        .flatten()
                })
                .ok_or(RequestError::Unavailable);
            let _ = reply.send(result);
        }
    }
}
fn read_page(
    engine: &mut GhosttyTerminal,
    source: PageSource,
    start: usize,
) -> Result<RowPage, RequestError> {
    let mut page = RowPage {
        source,
        start,
        cols: engine.cols(),
        rows: Vec::new(),
        placements: Vec::new(),
    };
    match source {
        PageSource::Screen { .. } => {
            for row in start..start.saturating_add(PAGE_ROWS) {
                let Some(row) = u32::try_from(row)
                    .ok()
                    .and_then(|row| engine.read_screen_row(row).ok().flatten())
                else {
                    break;
                };
                page.rows.push(row);
            }
        }
        PageSource::Block { id, generation, .. } => {
            let handle = BlockHandle { id, generation };
            let acquired = engine
                .acquire_block_snapshot(handle)
                .ok_or(RequestError::Unavailable)?;
            if acquired.block.handle().generation != generation {
                return Err(RequestError::Stale);
            }
            page.cols = acquired.block.cols();
            let end = start
                .saturating_add(PAGE_ROWS)
                .min(acquired.block.row_count());
            for row in start..end {
                let row = engine
                    .read_block_row(handle, row)
                    .map_err(|error| RequestError::Engine(error.to_string()))?
                    .ok_or(RequestError::Unavailable)?;
                page.rows.push(row);
            }
            page.placements = acquired
                .placements
                .into_iter()
                .filter(|placement| {
                    let first = placement.screen_row as usize;
                    first < end && first.saturating_add(placement.grid_rows as usize) > start
                })
                .collect();
        }
    }
    Ok(page)
}
