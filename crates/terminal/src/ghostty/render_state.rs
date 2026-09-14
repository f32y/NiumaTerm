use std::ptr;

#[cfg(test)]
use libghostty_vt_sys::Result as VtResult;

use libghostty_vt_sys::{
    ColorRgb as VtColorRgb, RenderState as VtRenderState,
    RenderStateCursorVisualStyle as VtRenderStateCursorVisualStyle,
    RenderStateData as VtRenderStateData, RenderStateDirty as VtRenderStateDirty,
    RenderStateOption as VtRenderStateOption, RenderStateRowData as VtRenderStateRowData,
    RenderStateRowIterator as VtRenderStateRowIterator,
    RenderStateRowOption as VtRenderStateRowOption, Terminal as VtTerminal,
    TerminalData as VtTerminalData, ghostty_render_state_free, ghostty_render_state_get,
    ghostty_render_state_new, ghostty_render_state_row_get, ghostty_render_state_row_iterator_free,
    ghostty_render_state_row_iterator_new, ghostty_render_state_row_iterator_next,
    ghostty_render_state_row_set, ghostty_render_state_set, ghostty_render_state_update,
    ghostty_terminal_get,
};

#[cfg(test)]
use libghostty_vt_sys::{
    Row as VtRow, RowData as VtRowData, RowSemanticPrompt as VtRowSemanticPrompt, ghostty_row_get,
};

use crate::ansi;

use crate::ghostty::{Error, Result, SnapshotColors, SnapshotCursor};

/// The engine's render state and the row damage derived from it.
///
/// The two handles are FFI allocations this owns outright, and the row
/// versions only mean anything against the state they were read from, so the
/// four are constructed and freed as one. The terminal handle stays on the
/// parent and arrives per call, which keeps a single owner for it.
pub(super) struct RenderStateReader {
    render_state: VtRenderState,
    row_iter: VtRenderStateRowIterator,

    /// Last terminal-content revision for each visible row. Revisions persist
    /// across publications so a frontend that skips a buffer still sees every
    /// row changed since its previous frame.
    row_versions: Vec<u64>,

    content_revision: u64,
}

impl Drop for RenderStateReader {
    fn drop(&mut self) {
        unsafe {
            ghostty_render_state_row_iterator_free(self.row_iter);
            ghostty_render_state_free(self.render_state);
        }
    }
}

impl RenderStateReader {
    pub(super) fn new(rows: u16) -> Result<Self> {
        let mut render_state = ptr::null_mut();

        Error::from_code(unsafe { ghostty_render_state_new(ptr::null(), &mut render_state) })?;

        let mut row_iter = ptr::null_mut();

        if let Err(err) = Error::from_code(unsafe {
            ghostty_render_state_row_iterator_new(ptr::null(), &mut row_iter)
        }) {
            unsafe { ghostty_render_state_free(render_state) };

            return Err(err);
        }

        Ok(Self {
            render_state,
            row_iter,
            row_versions: vec![0; rows as usize],
            content_revision: 0,
        })
    }

    /// Pull the engine's current frame into the render state.
    pub(super) fn update(&mut self, terminal: VtTerminal) -> Result<()> {
        Error::from_code(unsafe { ghostty_render_state_update(self.render_state, terminal) })
    }

    pub(super) fn row_versions(&self) -> &[u64] {
        &self.row_versions
    }

    /// Probe the engine's `SEMANTIC_PROMPT` tag per visible row. This verifies that
    /// headless parsing preserves OSC 133 metadata used by command-block rendering.
    #[cfg(test)]
    pub(super) fn row_semantic_prompts(
        &mut self,
        terminal: VtTerminal,
        rows: u16,
    ) -> Result<Vec<VtRowSemanticPrompt::Type>> {
        self.update(terminal)?;

        Error::from_code(unsafe {
            ghostty_render_state_get(
                self.render_state,
                VtRenderStateData::ROW_ITERATOR,
                (&mut self.row_iter as *mut VtRenderStateRowIterator).cast(),
            )
        })?;

        let mut out = Vec::with_capacity(rows as usize);

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

    /// Transfer Ghostty's transient render damage into persistent row versions.
    /// Both damage layers must be cleared after consumption; otherwise every
    /// later capture would repeat the first dirty update indefinitely.
    pub(super) fn consume_damage(&mut self, rows: u16) -> Result<()> {
        let mut dirty: VtRenderStateDirty::Type = VtRenderStateDirty::FALSE;

        Error::from_code(unsafe {
            ghostty_render_state_get(
                self.render_state,
                VtRenderStateData::DIRTY,
                (&mut dirty as *mut VtRenderStateDirty::Type).cast(),
            )
        })?;

        let rows = rows as usize;
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

    pub(super) fn cursor(&self) -> Result<SnapshotCursor> {
        let mut visible = false;

        Error::from_code(unsafe {
            ghostty_render_state_get(
                self.render_state,
                VtRenderStateData::CURSOR_VISIBLE,
                (&mut visible as *mut bool).cast(),
            )
        })?;

        // DECSCUSR determines the cursor shape reported by the render state.
        let mut style: VtRenderStateCursorVisualStyle::Type = VtRenderStateCursorVisualStyle::BLOCK;

        Error::from_code(unsafe {
            ghostty_render_state_get(
                self.render_state,
                VtRenderStateData::CURSOR_VISUAL_STYLE,
                (&mut style as *mut VtRenderStateCursorVisualStyle::Type).cast(),
            )
        })?;

        let shape = match style {
            VtRenderStateCursorVisualStyle::BAR => ansi::CursorShape::Beam,
            VtRenderStateCursorVisualStyle::UNDERLINE => ansi::CursorShape::Underline,
            // BLOCK and BLOCK_HOLLOW → Block (terminal renders hollow from focus state).
            _ => ansi::CursorShape::Block,
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
        })
    }

    /// Effective default colors from the render-state.
    pub(super) fn colors(&self, terminal: VtTerminal) -> Result<SnapshotColors> {
        use nmt_config::colors::ColorRgb;

        let read = |data: VtRenderStateData::Type| -> Result<ColorRgb> {
            let mut color = VtColorRgb::default();

            Error::from_code(unsafe {
                ghostty_render_state_get(
                    self.render_state,
                    data,
                    (&mut color as *mut VtColorRgb).cast(),
                )
            })?;

            Ok(ColorRgb {
                r: color.r,
                g: color.g,
                b: color.b,
            })
        };

        let fg = read(VtRenderStateData::COLOR_FOREGROUND)?;
        let bg = read(VtRenderStateData::COLOR_BACKGROUND)?;
        let mut has_cursor = false;

        Error::from_code(unsafe {
            ghostty_render_state_get(
                self.render_state,
                VtRenderStateData::COLOR_CURSOR_HAS_VALUE,
                (&mut has_cursor as *mut bool).cast(),
            )
        })?;

        let cursor = if has_cursor {
            Some(read(VtRenderStateData::COLOR_CURSOR)?)
        } else {
            None
        };

        // Compare engine colors directly so theme conversion rounding cannot
        // be mistaken for an OSC 11 override.
        let read_term = |data: VtTerminalData::Type| -> Result<Option<ColorRgb>> {
            let mut color = VtColorRgb::default();

            let result = Error::from_code(unsafe {
                ghostty_terminal_get(terminal, data, (&mut color as *mut VtColorRgb).cast())
            });

            // A terminal without configured colors has no default or override.
            // The render state still supplies its built-in display colors.
            match result {
                Ok(()) => {}
                Err(Error::NoValue) => return Ok(None),
                Err(error) => return Err(error),
            }

            Ok(Some(ColorRgb {
                r: color.r,
                g: color.g,
                b: color.b,
            }))
        };

        let bg_effective = read_term(VtTerminalData::COLOR_BACKGROUND)?;
        let bg_default = read_term(VtTerminalData::COLOR_BACKGROUND_DEFAULT)?;
        let bg_override = bg_effective.filter(|color| Some(*color) != bg_default);

        Ok(SnapshotColors {
            fg,
            bg,
            cursor,
            bg_override,
        })
    }
}
