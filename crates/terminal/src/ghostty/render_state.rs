use libghostty_vt_sys::{
    ColorRgb as VtColorRgb, PointTag as VtPointTag,
    RenderStateCursorVisualStyle as VtRenderStateCursorVisualStyle,
    RenderStateData as VtRenderStateData, RenderStateDirty as VtRenderStateDirty,
    RenderStateOption as VtRenderStateOption, RenderStateRowData as VtRenderStateRowData,
    RenderStateRowIterator as VtRenderStateRowIterator,
    RenderStateRowOption as VtRenderStateRowOption, Result as VtResult,
    TerminalData as VtTerminalData, ghostty_render_state_get, ghostty_render_state_row_get,
    ghostty_render_state_row_iterator_next, ghostty_render_state_row_set, ghostty_render_state_set,
    ghostty_render_state_update, ghostty_terminal_get,
};
#[cfg(test)]
use libghostty_vt_sys::{
    Row as VtRow, RowData as VtRowData, RowSemanticPrompt as VtRowSemanticPrompt, ghostty_row_get,
};

use crate::ansi;
use crate::ghostty::{Error, GhosttyTerminal, Result, SnapshotColors, SnapshotCursor};
use crate::render_buffer::RenderBuffer;

impl GhosttyTerminal {
    /// Probe: whether any visible row carries a PROMPT semantic tag (command-blocks-
    /// rendering — mark-forwarding regression checks in terminal pipeline tests).
    #[cfg(test)]
    pub(crate) fn has_prompt_tagged_row(&mut self) -> bool {
        self.row_semantic_prompts()
            .map(|tags| tags.contains(&VtRowSemanticPrompt::PROMPT))
            .unwrap_or(false)
    }

    /// Probe the engine's `SEMANTIC_PROMPT` tag per visible row. This verifies that
    /// headless parsing preserves OSC 133 metadata used by command-block rendering.
    #[cfg(test)]
    pub(super) fn row_semantic_prompts(&mut self) -> Result<Vec<VtRowSemanticPrompt::Type>> {
        Error::from_code(unsafe { ghostty_render_state_update(self.render_state, self.terminal) })?;

        Error::from_code(unsafe {
            ghostty_render_state_get(
                self.render_state,
                VtRenderStateData::ROW_ITERATOR,
                (&mut self.row_iter as *mut VtRenderStateRowIterator).cast(),
            )
        })?;

        let mut out = Vec::with_capacity(self.rows as usize);

        while unsafe { ghostty_render_state_row_iterator_next(self.row_iter) } {
            let mut tag: VtRowSemanticPrompt::Type = VtRowSemanticPrompt::NONE;
            let mut raw_row: VtRow = 0;

            if unsafe {
                ghostty_render_state_row_get(
                    self.row_iter,
                    VtRenderStateRowData::RAW,
                    (&mut raw_row as *mut VtRow).cast(),
                )
            } == VtResult::SUCCESS
            {
                let _ = unsafe {
                    ghostty_row_get(
                        raw_row,
                        VtRowData::SEMANTIC_PROMPT,
                        (&mut tag as *mut VtRowSemanticPrompt::Type).cast(),
                    )
                };
            }

            out.push(tag);
        }

        Ok(out)
    }

    /// Populate a reusable render buffer from the full visible viewport.
    pub fn snapshot_into(&mut self, buffer: &mut RenderBuffer) -> Result<()> {
        Error::from_code(unsafe { ghostty_render_state_update(self.render_state, self.terminal) })?;

        self.consume_render_damage()?;

        let cursor = self.cursor().unwrap_or(SnapshotCursor {
            x: 0,
            y: 0,
            visible: false,
            shape: ansi::CursorShape::Block,
            blinking: false,
        });

        let palette = self.color_palette();

        buffer.begin_capture(self.cols as usize, self.rows as usize);

        // A transient row lookup failure blanks only that row; publishing the
        // remaining viewport is safer than withholding an otherwise valid frame.
        for y in 0..self.rows {
            let meta = self
                .grid_ref_at(VtPointTag::VIEWPORT, 0, y as u32)
                .and_then(|grid_ref| {
                    Self::visit_row_cells(grid_ref, self.cols, &palette, |x, text, wide, style| {
                        buffer.write_cell(x as usize, y as usize, text.as_str(), wide, &style);
                    })
                })
                .unwrap_or_default();

            buffer.write_row_meta(y as usize, meta.wrapped, meta.virtual_placeholder);
        }

        let colors = self.colors();
        let placements = self.placements();
        let scrollbar = self.scrollbar();

        buffer.finish_capture(cursor, colors, placements, scrollbar, &self.row_versions);

        Ok(())
    }

    /// Transfer Ghostty's transient render damage into persistent row versions.
    /// Both damage layers must be cleared after consumption; otherwise every
    /// later capture would repeat the first dirty update indefinitely.
    fn consume_render_damage(&mut self) -> Result<()> {
        let mut dirty: VtRenderStateDirty::Type = VtRenderStateDirty::FALSE;

        Error::from_code(unsafe {
            ghostty_render_state_get(
                self.render_state,
                VtRenderStateData::DIRTY,
                (&mut dirty as *mut VtRenderStateDirty::Type).cast(),
            )
        })?;

        let rows = self.rows as usize;
        let dimensions_changed = self.row_versions.len() != rows;

        if dirty == VtRenderStateDirty::FALSE && !dimensions_changed {
            return Ok(());
        }

        self.content_revision = self.content_revision.wrapping_add(1);

        let revision = self.content_revision;

        self.row_versions.resize(rows, revision);

        let full = dimensions_changed || dirty != VtRenderStateDirty::PARTIAL;
        if full {
            self.row_versions.fill(revision);
        }

        Error::from_code(unsafe {
            ghostty_render_state_get(
                self.render_state,
                VtRenderStateData::ROW_ITERATOR,
                (&mut self.row_iter as *mut VtRenderStateRowIterator).cast(),
            )
        })?;

        let clean = false;
        let mut row = 0usize;

        while unsafe { ghostty_render_state_row_iterator_next(self.row_iter) } {
            let mut row_dirty = false;

            Error::from_code(unsafe {
                ghostty_render_state_row_get(
                    self.row_iter,
                    VtRenderStateRowData::DIRTY,
                    (&mut row_dirty as *mut bool).cast(),
                )
            })?;

            if !full
                && row_dirty
                && let Some(version) = self.row_versions.get_mut(row)
            {
                *version = revision;
            }

            Error::from_code(unsafe {
                ghostty_render_state_row_set(
                    self.row_iter,
                    VtRenderStateRowOption::DIRTY,
                    (&clean as *const bool).cast(),
                )
            })?;

            row += 1;
        }

        let clean = VtRenderStateDirty::FALSE;

        Error::from_code(unsafe {
            ghostty_render_state_set(
                self.render_state,
                VtRenderStateOption::DIRTY,
                (&clean as *const VtRenderStateDirty::Type).cast(),
            )
        })
    }

    /// Allocate and populate an owned render buffer for diagnostics and tests.
    pub fn snapshot(&mut self) -> Result<RenderBuffer> {
        let mut buffer = RenderBuffer::new(self.cols as usize, self.rows as usize);

        self.snapshot_into(&mut buffer)?;

        Ok(buffer)
    }

    fn cursor(&self) -> Result<SnapshotCursor> {
        let mut visible = false;

        Error::from_code(unsafe {
            ghostty_render_state_get(
                self.render_state,
                VtRenderStateData::CURSOR_VISIBLE,
                (&mut visible as *mut bool).cast(),
            )
        })?;

        // DECSCUSR shape and modes-based blink come from the render state.
        let mut style: VtRenderStateCursorVisualStyle::Type = VtRenderStateCursorVisualStyle::BLOCK;

        let _ = unsafe {
            ghostty_render_state_get(
                self.render_state,
                VtRenderStateData::CURSOR_VISUAL_STYLE,
                (&mut style as *mut VtRenderStateCursorVisualStyle::Type).cast(),
            )
        };

        let shape = match style {
            VtRenderStateCursorVisualStyle::BAR => ansi::CursorShape::Beam,
            VtRenderStateCursorVisualStyle::UNDERLINE => ansi::CursorShape::Underline,
            // BLOCK and BLOCK_HOLLOW → Block (terminal renders hollow from focus state).
            _ => ansi::CursorShape::Block,
        };

        let mut blinking = false;

        let _ = unsafe {
            ghostty_render_state_get(
                self.render_state,
                VtRenderStateData::CURSOR_BLINKING,
                (&mut blinking as *mut bool).cast(),
            )
        };

        let mut has_viewport = false;

        Error::from_code(unsafe {
            ghostty_render_state_get(
                self.render_state,
                VtRenderStateData::CURSOR_VIEWPORT_HAS_VALUE,
                (&mut has_viewport as *mut bool).cast(),
            )
        })?;

        if !has_viewport {
            return Ok(SnapshotCursor {
                x: 0,
                y: 0,
                visible: false,
                shape,
                blinking,
            });
        }

        let mut x = 0u16;
        let mut y = 0u16;

        Error::from_code(unsafe {
            ghostty_render_state_get(
                self.render_state,
                VtRenderStateData::CURSOR_VIEWPORT_X,
                (&mut x as *mut u16).cast(),
            )
        })?;

        Error::from_code(unsafe {
            ghostty_render_state_get(
                self.render_state,
                VtRenderStateData::CURSOR_VIEWPORT_Y,
                (&mut y as *mut u16).cast(),
            )
        })?;

        Ok(SnapshotCursor {
            x,
            y,
            visible,
            shape,
            blinking,
        })
    }

    /// Effective default colors from the render-state.
    fn colors(&self) -> SnapshotColors {
        use nmt_config::colors::ColorRgb;

        let read = |data: VtRenderStateData::Type| -> Option<ColorRgb> {
            let mut c = VtColorRgb::default();

            match unsafe {
                ghostty_render_state_get(
                    self.render_state,
                    data,
                    (&mut c as *mut VtColorRgb).cast(),
                )
            } {
                VtResult::SUCCESS => Some(ColorRgb {
                    r: c.r,
                    g: c.g,
                    b: c.b,
                }),
                _ => None,
            }
        };

        let fg = read(VtRenderStateData::COLOR_FOREGROUND).unwrap_or_default();
        let bg = read(VtRenderStateData::COLOR_BACKGROUND).unwrap_or_default();

        let mut has_cursor = false;

        let _ = unsafe {
            ghostty_render_state_get(
                self.render_state,
                VtRenderStateData::COLOR_CURSOR_HAS_VALUE,
                (&mut has_cursor as *mut bool).cast(),
            )
        };

        let cursor = if has_cursor {
            read(VtRenderStateData::COLOR_CURSOR)
        } else {
            None
        };

        // Detect OSC 11 overrides by comparing the effective background
        // (override OR default) to the engine's *default* (ignoring OSC). Both come
        // from the engine, so there's no config↔u8 conversion mismatch. An override
        // is active iff they differ; `bg_override` is then `Some(effective)`.
        let read_term = |data: VtTerminalData::Type| -> Option<ColorRgb> {
            let mut c = VtColorRgb::default();

            match unsafe {
                ghostty_terminal_get(self.terminal, data, (&mut c as *mut VtColorRgb).cast())
            } {
                VtResult::SUCCESS => Some(ColorRgb {
                    r: c.r,
                    g: c.g,
                    b: c.b,
                }),
                _ => None,
            }
        };

        let bg_effective = read_term(VtTerminalData::COLOR_BACKGROUND);
        let bg_default = read_term(VtTerminalData::COLOR_BACKGROUND_DEFAULT);
        let bg_override = if bg_effective != bg_default {
            bg_effective
        } else {
            None
        };

        SnapshotColors {
            fg,
            bg,
            cursor,
            bg_override,
        }
    }
}
