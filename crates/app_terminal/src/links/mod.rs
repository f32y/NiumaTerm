use std::ops::Range;

use gpui::{
    App, Bounds, Context, Modifiers, ModifiersChangedEvent, Pixels, Point, Window, point, px, size,
};
use nmt_terminal::ghostty::BlockHandle;

use crate::block_list::BlockListPoint;
use crate::view::{TerminalPane, terminal_cell_at_position};
/// A link resolved under the pointer: the URL plus underline rects relative
/// to the content origin (only the visible rows of a wrapped URL get rects).
#[derive(Clone, Debug, PartialEq)]
pub(super) struct LinkHit {
    pub(super) url: String,
    pub(super) rects: Vec<Bounds<Pixels>>,
}

/// Schemes Ctrl+click will open. A gate, not just a matcher: OSC 8 URIs come
/// from whatever program printed them, and an escape sequence must not be
/// able to launch arbitrary protocol handlers.
const URL_SCHEMES: [&str; 4] = ["https://", "http://", "file://", "mailto:"];

fn open_allowed(url: &str) -> bool {
    URL_SCHEMES
        .iter()
        .any(|scheme| url.len() > scheme.len() && url[..scheme.len()].eq_ignore_ascii_case(scheme))
}

/// Characters that can appear inside a URL (RFC 3986 plus `%`). ASCII-only,
/// so byte and char offsets coincide within a matched token.
fn is_url_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || "-._~:/?#[]@!$&'()*+,;=%".contains(c)
}

/// The URL covering char index `col` of `text`, if any, plus its char range
/// in `text`: expand over URL characters around the click, anchor at a known
/// scheme, and trim trailing punctuation that in practice ends the sentence
/// rather than the URL.
fn url_at_col(text: &str, col: usize) -> Option<(String, Range<usize>)> {
    let chars: Vec<char> = text.chars().collect();

    if !chars.get(col).copied().is_some_and(is_url_char) {
        return None;
    }

    let start = (0..col)
        .rev()
        .take_while(|&i| is_url_char(chars[i]))
        .last()
        .unwrap_or(col);

    let end = (col..chars.len())
        .take_while(|&i| is_url_char(chars[i]))
        .last()
        .map_or(col, |i| i + 1);

    let token: String = chars[start..end].iter().collect();

    // The scheme anchors the URL start; anything before it in the token
    // (quotes, parens, "url=") is surrounding text.
    let lower = token.to_ascii_lowercase();

    let scheme_at = URL_SCHEMES
        .iter()
        .filter_map(|scheme| lower.find(scheme))
        .min()?;

    let mut url = token[scheme_at..].trim_end_matches(['.', ',', ';', ':', '!', '?', '\'']);

    // Trailing closers are kept only while balanced, so a URL with literal
    // parens survives but the closer of a surrounding "(...)" is dropped.
    for (open, close) in [('(', ')'), ('[', ']')] {
        while url.ends_with(close) && url.matches(open).count() < url.matches(close).count() {
            url = &url[..url.len() - 1];
        }
    }

    // The click must land inside the URL itself, past any trimmed tail.
    let url_range = start + scheme_at..start + scheme_at + url.len();

    (url_range.contains(&col) && open_allowed(url)).then(|| (url.to_string(), url_range))
}

impl TerminalPane {
    /// Track the Ctrl-hover link underline: set while Ctrl is held over a
    /// link inside the content area, cleared otherwise.
    pub(super) fn update_hovered_link(
        &mut self,
        position: Point<Pixels>,
        modifiers: Modifiers,
        cx: &mut Context<Self>,
    ) {
        let inside = self
            .content_bounds
            .is_some_and(|bounds| bounds.contains(&position));

        let hit = (inside && modifiers.control && !modifiers.alt && !modifiers.shift)
            .then(|| self.link_at_position(position, cx))
            .flatten();

        if self.hovered_link != hit {
            self.hovered_link = hit;
            cx.notify();
        }
    }

    pub(super) fn on_modifiers_changed(
        &mut self,
        event: &ModifiersChangedEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(position) = self.last_mouse_position {
            self.update_hovered_link(position, event.modifiers, cx);
        }
    }

    /// Resolve the link under a pointer position: the row's OSC 8 span if one
    /// covers the pointed-at cell, else a URL-shaped token in the row text.
    /// Soft-wrapped neighbor rows are joined so long URLs match whole. Also
    /// yields underline rects (content-origin-relative) for hover feedback.
    pub(super) fn link_at_position(&self, position: Point<Pixels>, cx: &App) -> Option<LinkHit> {
        let cell_metrics = self.cell_metrics?;

        enum RowSource {
            Screen(i64),
            Block {
                handle: BlockHandle,
                item: usize,
                line: i64,
            },
        }

        let block_list = self.block_list_mode(cx);

        // Bottom-anchor slack is uniform across rows (see
        // `bottom_anchor_offsets`), so one value shifts every underline.
        let slack = self.current_row_offsets(cx).first().copied().unwrap_or(0.0);

        let viewport_top = self.surface.viewport_top_screen_row();

        let (source, col) = match self.block_list_point_at(position, cx) {
            Some(BlockListPoint::Frozen(pt)) => {
                // The handle lookup releases the store lock before the engine
                // reads below (the PTY thread nests engine → store, so the
                // reverse nesting would deadlock).
                let handle = {
                    let store = self.surface.block_store();
                    let store = store.lock();
                    store.items().get(pt.item)?.handle()?
                };
                (
                    RowSource::Block {
                        handle,
                        item: pt.item,
                        line: pt.line as i64,
                    },
                    pt.col as usize,
                )
            }
            Some(BlockListPoint::LiveHistory { row, col }) => {
                (RowSource::Screen(row as i64), col as usize)
            }
            None => {
                let offsets = self.current_row_offsets(cx);

                let mut origin = self.content_origin();

                if block_list {
                    origin.y += px(self.frozen_hit.active_top);
                }

                let (cell, _) = terminal_cell_at_position(position, origin, cell_metrics, &offsets);
                (
                    RowSource::Screen(viewport_top? as i64 + cell.row as i64),
                    cell.col as usize,
                )
            }
        };

        let row_at = |delta: i64| match source {
            RowSource::Screen(row) => u32::try_from(row + delta)
                .ok()
                .and_then(|row| self.surface.pointer_screen_row(row)),
            RowSource::Block { handle, line, .. } => usize::try_from(line + delta)
                .ok()
                .and_then(|line| self.surface.pointer_block_row(handle, line)),
        };

        // Content-local y of the row `delta` rows below the pointed-at one;
        // `None` when it is scrolled out of view (that segment gets no rect).
        let row_y = |delta: i64| -> Option<f32> {
            match source {
                RowSource::Screen(row) => {
                    let row = row + delta;
                    let top = viewport_top? as i64;

                    if row < top {
                        // A live-history row above the engine viewport.
                        return self
                            .frozen_hit
                            .row_top(usize::MAX, usize::try_from(row).ok()?);
                    }

                    let below = (row - top) as f32 * cell_metrics.height_px;

                    Some(if block_list {
                        self.frozen_hit.active_top + below
                    } else {
                        below + slack
                    })
                }
                RowSource::Block { item, line, .. } => self
                    .frozen_hit
                    .row_top(item, usize::try_from(line + delta).ok()?),
            }
        };
        let underline = |delta: i64, start_col: usize, cols: usize| -> Option<Bounds<Pixels>> {
            let y = row_y(delta)?;

            Some(Bounds::new(
                point(
                    px(start_col as f32 * cell_metrics.width_px),
                    px(y + cell_metrics.height_px - 1.5),
                ),
                size(px(cols as f32 * cell_metrics.width_px), px(1.0)),
            ))
        };

        let pointed = row_at(0)?;

        if let Some((start, end, uri)) = pointed
            .hyperlinks
            .iter()
            .find(|(start, end, _)| (*start as usize..=*end as usize).contains(&col))
        {
            if !open_allowed(uri) {
                return None;
            }

            return Some(LinkHit {
                url: uri.clone(),
                rects: underline(0, *start as usize, (*end - *start) as usize + 1)
                    .into_iter()
                    .collect(),
            });
        }

        // Join cap bounds the engine row reads per hover/click: a wrapped
        // logical line can chain through the whole scrollback (e.g. `cat` of
        // a minified file), and each joined row is a locked engine read. A
        // URL wrapping further than ±8 rows truncates at the cap.
        const JOIN_CAP: i64 = 8;

        let width = pointed.text.chars().count();

        let mut text = pointed.text;
        let mut col = col;
        let mut wrapped_down = pointed.wrapped;

        for delta in 1..=JOIN_CAP {
            if !wrapped_down {
                break;
            }

            let Some(next) = row_at(delta) else { break };

            text.push_str(&next.text);

            wrapped_down = next.wrapped;
        }

        let mut back = 0i64;

        for delta in 1..=JOIN_CAP {
            let Some(prev) = row_at(-delta).filter(|prev| prev.wrapped) else {
                break;
            };

            back = delta;

            col += prev.text.chars().count();

            text.insert_str(0, &prev.text);
        }

        let (url, range) = url_at_col(&text, col)?;

        // Every joined segment is exactly `width` chars (rows are padded to
        // the grid width), so the URL's char range maps directly onto rows.
        let mut rects = Vec::new();

        if let (Some(first_seg), Some(last_seg)) = (
            range.start.checked_div(width),
            (range.end - 1).checked_div(width),
        ) {
            for seg in first_seg..=last_seg {
                let start = range.start.max(seg * width);
                let end = range.end.min((seg + 1) * width);

                rects.extend(underline(
                    seg as i64 - back,
                    start - seg * width,
                    end - start,
                ));
            }
        }
        Some(LinkHit { url, rects })
    }
}

#[cfg(test)]
mod tests;
