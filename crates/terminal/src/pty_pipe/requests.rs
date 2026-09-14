use crate::ghostty::{BlockHandle, GhosttyTerminal};
use crate::session::page::{PAGE_ROWS, PageSource, RowPage};
use crate::session::request::{Query, RequestError, TextSource};
use crate::session::selection::block_selection_range;

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
                } => engine
                    .block_acquire(handle)
                    .ok_or(RequestError::Unavailable)
                    .and_then(|block| {
                        if block.handle().generation != handle.generation {
                            return Err(RequestError::Stale);
                        }

                        let (start, end) =
                            block_selection_range(&block, &engine.color_palette(), line, col, kind)
                                .ok_or(RequestError::Unavailable)?;

                        block
                            .format_range_clamped(Some(start), Some(end))
                            .map_err(|error| RequestError::Engine(error.to_string()))?
                            .ok_or(RequestError::Unavailable)
                    }),
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
                                .format_range_clamped(piece.start, piece.end)
                                .map_err(|error| RequestError::Engine(error.to_string()))?
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
                let Ok(row) = u32::try_from(row) else {
                    break;
                };

                let Some(row) = engine
                    .read_screen_row(row)
                    .map_err(|error| RequestError::Engine(error.to_string()))?
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
