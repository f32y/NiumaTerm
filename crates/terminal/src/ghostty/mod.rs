#[cfg(test)]
use std::sync;
use std::{array, mem, os, path, ptr, slice, time};

/// Engine handle of a finished command block (per-block grid). Plain value
/// type; lookup is by id, `generation` is the data version for cache keys.
pub use libghostty_vt_sys::BlockHandle;
use libghostty_vt_sys::{
    ColorRgb as VtColorRgb, Formatter as VtFormatter, FormatterFormat as VtFormatterFormat,
    FormatterTerminalExtra as VtFormatterTerminalExtra,
    FormatterTerminalOptions as VtFormatterTerminalOptions, GridRef as VtGridRef,
    KITTY_KEY_DISAMBIGUATE, KITTY_KEY_REPORT_ALL, KITTY_KEY_REPORT_ALTERNATES,
    KITTY_KEY_REPORT_ASSOCIATED, KITTY_KEY_REPORT_EVENTS, KittyGraphics as VtKittyGraphics,
    KittyGraphicsData as VtKittyGraphicsData, KittyGraphicsImage as VtKittyGraphicsImage,
    KittyGraphicsImageData as VtKittyGraphicsImageData,
    KittyGraphicsPlacementData as VtKittyGraphicsPlacementData,
    KittyGraphicsPlacementIterator as VtKittyGraphicsPlacementIterator,
    KittyImageFormat as VtKittyImageFormat, PointCoordinate as VtPointCoordinate,
    PointTag as VtPointTag, RenderState as VtRenderState,
    RenderStateCursorVisualStyle as VtRenderStateCursorVisualStyle,
    RenderStateData as VtRenderStateData, RenderStateDirty as VtRenderStateDirty,
    RenderStateOption as VtRenderStateOption, RenderStateRowData as VtRenderStateRowData,
    RenderStateRowIterator as VtRenderStateRowIterator,
    RenderStateRowOption as VtRenderStateRowOption, Result as VtResult, Selection as VtSelection,
    String as VtString, Terminal as VtTerminal, TerminalCursorStyle as VtTerminalCursorStyle,
    TerminalData as VtTerminalData, TerminalOption as VtTerminalOption,
    TerminalOptions as VtTerminalOptions, TerminalScrollViewport as VtTerminalScrollViewport,
    TerminalScrollViewportTag as VtTerminalScrollViewportTag,
    TerminalScrollViewportValue as VtTerminalScrollViewportValue,
    TerminalScrollbar as VtTerminalScrollbar, ghostty_block_ref_placement_pos,
    ghostty_formatter_format_alloc, ghostty_formatter_free, ghostty_formatter_terminal_new,
    ghostty_free, ghostty_kitty_graphics_get, ghostty_kitty_graphics_image,
    ghostty_kitty_graphics_image_get, ghostty_kitty_graphics_placement_get,
    ghostty_kitty_graphics_placement_grid_size, ghostty_kitty_graphics_placement_iterator_free,
    ghostty_kitty_graphics_placement_iterator_new, ghostty_kitty_graphics_placement_next,
    ghostty_kitty_graphics_placement_pixel_size, ghostty_kitty_graphics_placement_source_rect,
    ghostty_kitty_graphics_placement_viewport_pos, ghostty_render_state_free,
    ghostty_render_state_get, ghostty_render_state_new, ghostty_render_state_row_get,
    ghostty_render_state_row_iterator_free, ghostty_render_state_row_iterator_new,
    ghostty_render_state_row_iterator_next, ghostty_render_state_row_set, ghostty_render_state_set,
    ghostty_render_state_update, ghostty_terminal_free, ghostty_terminal_get,
    ghostty_terminal_mode_get, ghostty_terminal_new, ghostty_terminal_point_from_grid_ref,
    ghostty_terminal_resize, ghostty_terminal_scroll_viewport, ghostty_terminal_set,
    ghostty_terminal_vt_write, sized as vt_sized,
};
#[cfg(test)]
use libghostty_vt_sys::{
    Row as VtRow, RowData as VtRowData, RowSemanticPrompt as VtRowSemanticPrompt, ghostty_row_get,
};
#[cfg(test)]
use nmt_config::colors::ColorRgb;
use nmt_config::colors::Colors;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::pwd::pwd_to_path;
use crate::render_buffer::RenderBuffer;
use crate::{ansi, clipboard, graphics, terminal};

mod block;
mod callbacks;
mod error;
mod grid_read;
mod types;

pub use crate::ghostty::block::{AcquiredBlock, BlockRef};
use crate::ghostty::callbacks::{
    Callbacks, KITTY_IMAGE_STORAGE_LIMIT_BYTES, bell_cb, clipboard_write_cb, register_png_decoder,
    write_pty_cb,
};
pub use crate::ghostty::error::{Error, Result};
pub use crate::ghostty::types::{
    CellText, CellWide, Color, Palette, PlacementScreenPos, ScreenRowMeta, ScrollbarInfo,
    SnapshotColors, SnapshotCursor, SnapshotPlacement, SnapshotStyle, Underline,
};
#[cfg(test)]
pub use crate::ghostty::types::{RowCell, ScreenRowRead};

/// VT mode identifiers for [`GhosttyTerminal::mode`].
///
/// Values mirror Ghostty's `ModeTag` (packed `u16`): a DEC private mode uses its
/// raw number; an ANSI mode sets bit 15. See Ghostty `src/terminal/modes.zig`.
pub mod mode;

pub struct GhosttyTerminal {
    terminal: VtTerminal,
    render_state: VtRenderState,
    row_iter: VtRenderStateRowIterator,
    /// Reused each `snapshot()` to walk kitty placements. Allocated once;
    /// `ghostty_kitty_graphics_get(PLACEMENT_ITERATOR)` re-points it at the live
    /// storage with no allocation, so a no-graphics batch costs ~3 FFI calls.
    placement_iter: VtKittyGraphicsPlacementIterator,
    cols: u16,
    rows: u16,
    /// Last terminal-content revision for each visible row. Revisions persist
    /// across publications so a frontend that skips a buffer still sees every
    /// row changed since its previous frame.
    row_versions: Vec<u64>,
    content_revision: u64,
    /// Boxed so its heap address stays fixed across `GhosttyTerminal` moves;
    /// registered with the engine as the callback userdata pointer.
    callbacks: Box<Callbacks>,
    last_title: String,
    last_pwd: String,
    /// Kitty image-delta cache: `image_id → (width, height, data_len)`
    /// of every image already shipped to the frontend. Owned by the PTY reader
    /// thread (only `take_image_deltas` mutates it). A key change (re-transmit with
    /// new size/length) re-ships the pixels; a vanished id is removed.
    shipped_images: FxHashMap<u32, (u32, u32, usize)>,
    scrollbar_override: Option<ScrollbarInfo>,
}

// The Ghostty `Terminal` and its render-state handles are raw FFI pointers
// (`!Send`). A `GhosttyTerminal` owns them exclusively and is only ever touched
// from the single thread that holds it (the PTY reader thread), so moving the
// whole value across threads is sound. It is not `Sync` — no shared access.
unsafe impl Send for GhosttyTerminal {}

impl GhosttyTerminal {
    pub fn new(cols: u16, rows: u16, max_scrollback: usize) -> Result<Self> {
        if cols == 0 || rows == 0 {
            return Err(Error::InvalidValue);
        }

        let mut terminal = ptr::null_mut();

        let options = VtTerminalOptions {
            cols,
            rows,
            max_scrollback,
        };

        Error::from_code(unsafe { ghostty_terminal_new(ptr::null(), &mut terminal, options) })?;

        let mut render_state = ptr::null_mut();
        if let Err(err) =
            Error::from_code(unsafe { ghostty_render_state_new(ptr::null(), &mut render_state) })
        {
            unsafe { ghostty_terminal_free(terminal) };
            return Err(err);
        }

        let mut row_iter = ptr::null_mut();
        if let Err(err) = Error::from_code(unsafe {
            ghostty_render_state_row_iterator_new(ptr::null(), &mut row_iter)
        }) {
            unsafe {
                ghostty_render_state_free(render_state);
                ghostty_terminal_free(terminal);
            }
            return Err(err);
        }

        let mut placement_iter = ptr::null_mut();
        if let Err(err) = Error::from_code(unsafe {
            ghostty_kitty_graphics_placement_iterator_new(ptr::null(), &mut placement_iter)
        }) {
            unsafe {
                ghostty_render_state_row_iterator_free(row_iter);
                ghostty_render_state_free(render_state);
                ghostty_terminal_free(terminal);
            }
            return Err(err);
        }

        // Register the process-global PNG decoder once for Kitty `f=100` payloads.
        register_png_decoder();

        // Raise the kitty image storage limit from the conservative 10 MB `.lib`
        // default; a non-zero limit also enables the protocol.
        let limit = KITTY_IMAGE_STORAGE_LIMIT_BYTES;
        unsafe {
            ghostty_terminal_set(
                terminal,
                VtTerminalOption::KITTY_IMAGE_STORAGE_LIMIT,
                (&limit as *const u64).cast(),
            );
        }

        // Register synchronous callbacks. Userdata points at the boxed
        // `Callbacks`; its heap address is stable across moves of `Self`.
        let mut callbacks = Box::new(Callbacks::default());
        let userdata = &mut *callbacks as *mut Callbacks as *mut os::raw::c_void;
        unsafe {
            ghostty_terminal_set(terminal, VtTerminalOption::USERDATA, userdata);
            ghostty_terminal_set(
                terminal,
                VtTerminalOption::WRITE_PTY,
                write_pty_cb as *const os::raw::c_void,
            );
            ghostty_terminal_set(
                terminal,
                VtTerminalOption::BELL,
                bell_cb as *const os::raw::c_void,
            );
            ghostty_terminal_set(
                terminal,
                VtTerminalOption::CLIPBOARD_WRITE,
                clipboard_write_cb as *const os::raw::c_void,
            );
        }

        // Match conhost/ConPTY, which defaults to grapheme clustering (mode 2027,
        // permanently on). Without this ghostty measures ZWJ/multi-emoji clusters
        // per-codepoint (a family emoji = 6 cols) while ConPTY uses the clustered
        // width (2 cols), so the cursor misaligns on any line with such a cluster —
        // independent of resize. Real ptys (macOS/Linux) let the app drive 2027, so
        // this default is Windows-only.
        #[cfg(windows)]
        unsafe {
            let seq = b"\x1b[?2027h";
            ghostty_terminal_vt_write(terminal, seq.as_ptr(), seq.len());
        }

        Ok(Self {
            terminal,
            render_state,
            row_iter,
            placement_iter,
            cols,
            rows,
            row_versions: vec![0; rows as usize],
            content_revision: 0,
            callbacks,
            last_title: String::new(),
            last_pwd: String::new(),
            shipped_images: FxHashMap::default(),
            scrollbar_override: None,
        })
    }

    /// Drain bytes the terminal wants written back to the PTY (query/DSR/DA
    /// responses). Returns empty when there is nothing to send.
    pub fn take_pty_writes(&mut self) -> Vec<u8> {
        mem::take(&mut self.callbacks.pty_writes)
    }

    /// Drain and reset the bell counter (number of BELs since last call).
    pub fn take_bell(&mut self) -> u32 {
        mem::replace(&mut self.callbacks.bell_count, 0)
    }

    /// Drain clipboard writes decoded from OSC 52 or iTerm2 OSC 1337.
    pub fn take_clipboard_writes(&mut self) -> Vec<(clipboard::ClipboardType, String)> {
        mem::take(&mut self.callbacks.clipboard_writes)
    }

    /// Poll the terminal title; returns `Some(title)` only when it changed
    /// since the last poll.
    pub fn poll_title(&mut self) -> Option<String> {
        let title = self.get_string(VtTerminalData::TITLE);

        if title != self.last_title {
            self.last_title = title.clone();
            Some(title)
        } else {
            None
        }
    }

    /// Poll the working directory (OSC 7); returns `Some(pwd)` only when it
    /// changed since the last poll.
    pub fn poll_pwd(&mut self) -> Option<String> {
        let pwd = self.get_string(VtTerminalData::PWD);

        if pwd != self.last_pwd {
            self.last_pwd = pwd.clone();
            Some(pwd)
        } else {
            None
        }
    }

    /// The current OSC window title (peek — reads the engine's live value, no
    /// change-detection). `poll_title` is the producer's change-detecting variant;
    /// this is for on-demand frontend reads (title template), replacing the mirror.
    pub fn title(&self) -> String {
        self.get_string(VtTerminalData::TITLE)
    }

    /// The current OSC 7 working directory (peek) as a path, or `None` when unset.
    /// Replaces the mirror's `current_directory` for the title template.
    pub fn current_directory(&self) -> Option<path::PathBuf> {
        let pwd = self.get_string(VtTerminalData::PWD);

        if pwd.is_empty() {
            None
        } else {
            Some(pwd_to_path(&pwd))
        }
    }

    /// Set the working directory directly. OSC 7 populates the same engine state;
    /// this direct setter is
    /// kept for tests and programmatic cwd updates.
    pub fn set_pwd(&mut self, pwd: &str) {
        let s = VtString {
            ptr: pwd.as_ptr(),
            len: pwd.len(),
        };

        unsafe {
            ghostty_terminal_set(
                self.terminal,
                VtTerminalOption::PWD,
                (&s as *const VtString).cast(),
            );
        }
    }

    /// Read a `GhosttyString`-typed terminal datum as an owned `String`. The
    /// borrowed pointer is only valid until the next mutating call, so we copy
    /// immediately.
    fn get_string(&self, data: VtTerminalData::Type) -> String {
        let mut s = VtString {
            ptr: ptr::null(),
            len: 0,
        };

        let ok =
            unsafe { ghostty_terminal_get(self.terminal, data, (&mut s as *mut VtString).cast()) };

        if ok != VtResult::SUCCESS || s.ptr.is_null() || s.len == 0 {
            return String::new();
        }

        let bytes = unsafe { slice::from_raw_parts(s.ptr, s.len) };

        String::from_utf8_lossy(bytes).into_owned()
    }

    /// The cursor's row in the **active screen** (the region CUP addresses),
    /// 0-based, independent of the viewport scroll pin. Reads
    /// `terminal.screens.active.cursor.y` via the engine — unlike
    /// `snapshot().cursor.y` (render-state, **viewport-relative**), this stays valid
    /// when the viewport is scrolled into history or has blank rows below the prompt.
    /// Used to realign ConPTY resize echoes to the true prompt row. `None` on error.
    pub fn active_cursor_row(&self) -> Option<u16> {
        let mut out: u16 = 0;

        let ok = unsafe {
            ghostty_terminal_get(
                self.terminal,
                VtTerminalData::CURSOR_Y,
                (&mut out as *mut u16).cast(),
            )
        };

        (ok == VtResult::SUCCESS).then_some(out)
    }

    pub fn cols(&self) -> u16 {
        self.cols
    }

    pub fn rows(&self) -> u16 {
        self.rows
    }

    /// The engine's scrollbar geometry for the current viewport pin.
    /// **Expensive for arbitrary (scrolled) pins** — read it via `snapshot()`, not
    /// per render frame.
    pub fn scrollbar(&self) -> ScrollbarInfo {
        if let Some(sb) = self.scrollbar_override {
            return sb;
        }

        self.raw_scrollbar()
    }

    fn raw_scrollbar(&self) -> ScrollbarInfo {
        let mut sb = VtTerminalScrollbar::default();

        unsafe {
            ghostty_terminal_get(
                self.terminal,
                VtTerminalData::SCROLLBAR,
                (&mut sb as *mut VtTerminalScrollbar).cast(),
            );
        }

        ScrollbarInfo {
            total: sb.total,
            offset: sb.offset,
            len: sb.len,
        }
    }

    /// Feed raw VT bytes through the terminal's stream parser.
    pub fn write_vt(&mut self, data: &[u8]) {
        unsafe { ghostty_terminal_vt_write(self.terminal, data.as_ptr(), data.len()) };

        if self.scrollbar_override.is_some() {
            self.update_scrollbar_override();
        }
    }

    /// Set the Kitty-image storage limit in bytes; `new()` applies the default. A non-zero limit
    /// also enables the protocol; 0 disables it. Exposed for tests/eviction.
    pub fn set_kitty_storage_limit(&mut self, bytes: u64) {
        unsafe {
            ghostty_terminal_set(
                self.terminal,
                VtTerminalOption::KITTY_IMAGE_STORAGE_LIMIT,
                (&bytes as *const u64).cast(),
            );
        }
    }

    /// Whether the engine currently holds a kitty image with this id —
    /// cheap id lookup, no pixel copy. Used to observe transmit/delete/eviction.
    pub fn kitty_image_exists(&self, image_id: u32) -> bool {
        let mut graphics: VtKittyGraphics = ptr::null_mut();

        let have = unsafe {
            ghostty_terminal_get(
                self.terminal,
                VtTerminalData::KITTY_GRAPHICS,
                (&mut graphics as *mut VtKittyGraphics).cast(),
            )
        } == VtResult::SUCCESS
            && !graphics.is_null();

        have && !unsafe { ghostty_kitty_graphics_image(graphics, image_id) }.is_null()
    }

    /// Read the current value of a VT mode (identifiers in [`mode`]). Returns
    /// `false` for unknown/unset modes.
    pub fn mode(&self, id: u16) -> bool {
        let mut value = false;

        let ok = unsafe { ghostty_terminal_mode_get(self.terminal, id, &mut value as *mut bool) };

        ok == VtResult::SUCCESS && value
    }

    /// The active kitty keyboard protocol flags, mapped to terminal `Mode` bits. These
    /// live in the engine's kitty-keyboard flag stack, NOT the DEC private modes, so
    /// `mode()` can't read them — the vt_modes facade folds these in separately so
    /// `session_key_flags` / the input path see kitty press+release encoding
    /// for key press and release encoding. Empty when the protocol is inactive.
    pub fn kitty_keyboard_modes(&self) -> terminal::Mode {
        use crate::terminal::Mode;
        let mut flags: u8 = 0;

        let ok = unsafe {
            ghostty_terminal_get(
                self.terminal,
                VtTerminalData::KITTY_KEYBOARD_FLAGS,
                (&mut flags as *mut u8).cast(),
            )
        };

        if ok != VtResult::SUCCESS {
            return Mode::empty();
        }

        let mut m = Mode::empty();

        m.set(
            Mode::DISAMBIGUATE_ESC_CODES,
            flags & KITTY_KEY_DISAMBIGUATE != 0,
        );

        m.set(
            Mode::REPORT_EVENT_TYPES,
            flags & KITTY_KEY_REPORT_EVENTS != 0,
        );

        m.set(
            Mode::REPORT_ALTERNATE_KEYS,
            flags & KITTY_KEY_REPORT_ALTERNATES != 0,
        );

        m.set(
            Mode::REPORT_ALL_KEYS_AS_ESC,
            flags & KITTY_KEY_REPORT_ALL != 0,
        );

        m.set(
            Mode::REPORT_ASSOCIATED_TEXT,
            flags & KITTY_KEY_REPORT_ASSOCIATED != 0,
        );

        m
    }

    pub fn resize(
        &mut self,
        cols: u16,
        rows: u16,
        cell_width_px: u32,
        cell_height_px: u32,
    ) -> Result<()> {
        if cols == 0 || rows == 0 || cell_width_px == 0 || cell_height_px == 0 {
            return Err(Error::InvalidValue);
        }

        // Workaround for an upstream integer overflow in Ghostty's column
        // reflow. When columns AND rows both change in one resize, Ghostty's
        // `PageList.resizeCols` computes `self.rows - c.y - 1` against the
        // already-reduced row count; if the cursor sits on a row at or below
        // the new bottom (common — shells leave the cursor near the last row),
        // that unsigned subtraction underflows and the Zig side panics
        // ("integer overflow"), aborting the PTY thread. Shrinking the window
        // reproduces this every time.
        //
        // We avoid the buggy path by never changing cols and rows together:
        // first resize columns against the *current* row count (the cursor is
        // always within bounds there, so the reflow math can't underflow), then
        // resize rows with columns unchanged (which skips the column-reflow path
        // entirely). Do NOT collapse this back into a single call.
        if cols != self.cols && rows != self.rows {
            Error::from_code(unsafe {
                ghostty_terminal_resize(
                    self.terminal,
                    cols,
                    self.rows,
                    cell_width_px,
                    cell_height_px,
                )
            })?;
        }

        Error::from_code(unsafe {
            ghostty_terminal_resize(self.terminal, cols, rows, cell_width_px, cell_height_px)
        })?;

        self.cols = cols;
        self.rows = rows;

        self.update_scrollbar_override();

        Ok(())
    }

    fn update_scrollbar_override(&mut self) {
        self.scrollbar_override = None;

        let raw = self.raw_scrollbar();

        if raw.total <= raw.len {
            return;
        }

        let Ok(text) = self.format_text(None, false, true) else {
            return;
        };

        if text.lines().count() > self.rows as usize {
            return;
        }

        self.scroll_viewport_top_raw();

        self.scrollbar_override = Some(ScrollbarInfo {
            total: raw.len,
            offset: 0,
            len: raw.len,
        });
    }

    /// Set the shape used until a program overrides it with DECSCUSR and again
    /// after that program resets the cursor style with `CSI 0 SP q`.
    pub fn set_default_cursor_shape(&mut self, shape: ansi::CursorShape) -> Result<()> {
        let style: VtTerminalCursorStyle::Type = match shape {
            ansi::CursorShape::Beam => VtTerminalCursorStyle::BAR,
            ansi::CursorShape::Underline => VtTerminalCursorStyle::UNDERLINE,
            ansi::CursorShape::Block | ansi::CursorShape::Hidden => VtTerminalCursorStyle::BLOCK,
        };

        Error::from_code(unsafe {
            ghostty_terminal_set(
                self.terminal,
                VtTerminalOption::DEFAULT_CURSOR_STYLE,
                (&style as *const VtTerminalCursorStyle::Type).cast(),
            )
        })
    }

    /// Push default foreground/background/cursor colors and the 256-color
    /// palette into the engine so SGR-indexed and default colors resolve to the
    /// host theme rather than Ghostty's built-in palette.
    pub fn set_colors(
        &mut self,
        fg: [u8; 3],
        bg: [u8; 3],
        cursor: [u8; 3],
        palette: &[[u8; 3]; 256],
    ) {
        let rgb = |c: [u8; 3]| VtColorRgb {
            r: c[0],
            g: c[1],
            b: c[2],
        };

        let f = rgb(fg);
        let b = rgb(bg);
        let c = rgb(cursor);

        let pal: [VtColorRgb; 256] = array::from_fn(|i| rgb(palette[i]));

        unsafe {
            ghostty_terminal_set(
                self.terminal,
                VtTerminalOption::COLOR_FOREGROUND,
                (&f as *const VtColorRgb).cast(),
            );

            ghostty_terminal_set(
                self.terminal,
                VtTerminalOption::COLOR_BACKGROUND,
                (&b as *const VtColorRgb).cast(),
            );

            ghostty_terminal_set(
                self.terminal,
                VtTerminalOption::COLOR_CURSOR,
                (&c as *const VtColorRgb).cast(),
            );

            ghostty_terminal_set(
                self.terminal,
                VtTerminalOption::COLOR_PALETTE,
                pal.as_ptr().cast(),
            );
        }
    }

    pub fn set_theme_colors(&mut self, colors: &Colors) {
        use nmt_config::colors::term::List;
        use nmt_config::colors::{ColorRgb, NamedColor};

        let list = List::from(colors);
        let to_rgb = |color| {
            let color = ColorRgb::from_color_arr(color);
            [color.r, color.g, color.b]
        };

        let palette = array::from_fn(|index| to_rgb(list[index]));

        self.set_colors(
            to_rgb(list[NamedColor::Foreground]),
            to_rgb(list[NamedColor::Background]),
            to_rgb(list[NamedColor::Cursor]),
            &palette,
        );
    }

    /// Probe: whether any visible row carries a PROMPT semantic tag (command-blocks-
    /// rendering — mark-forwarding regression checks in terminal pipeline tests).
    #[cfg(test)]
    pub(crate) fn has_prompt_tagged_row(&mut self) -> bool {
        self.row_semantic_prompts()
            .map(|tags| tags.iter().any(|&t| t == VtRowSemanticPrompt::PROMPT))
            .unwrap_or(false)
    }

    /// Probe the engine's `SEMANTIC_PROMPT` tag per visible row. This verifies that
    /// headless parsing preserves OSC 133 metadata used by command-block rendering.
    #[cfg(test)]
    fn row_semantic_prompts(&mut self) -> Result<Vec<VtRowSemanticPrompt::Type>> {
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

            if !full && row_dirty {
                if let Some(version) = self.row_versions.get_mut(row) {
                    *version = revision;
                }
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

    /// Walk the engine's kitty-graphics placements into owned `SnapshotPlacement`s
    /// Re-points the persistent iterator at the live storage (no alloc),
    /// then for each placement reads the scalar fields and — for non-virtual visible
    /// placements — the viewport-relative geometry. Returns empty when graphics are
    /// disabled or there are no placements (the common case: ~3 FFI calls).
    fn placements(&mut self) -> Vec<SnapshotPlacement> {
        let mut out = Vec::new();

        // Borrowed handle to the active screen's image storage; valid until the next
        // mutating terminal call (we only read here).
        let mut graphics: VtKittyGraphics = ptr::null_mut();

        if unsafe {
            ghostty_terminal_get(
                self.terminal,
                VtTerminalData::KITTY_GRAPHICS,
                (&mut graphics as *mut VtKittyGraphics).cast(),
            )
        } != VtResult::SUCCESS
            || graphics.is_null()
        {
            return out;
        }

        // Re-point the persistent iterator at the live placement set (no alloc).
        if unsafe {
            ghostty_kitty_graphics_get(
                graphics,
                VtKittyGraphicsData::PLACEMENT_ITERATOR,
                (&mut self.placement_iter as *mut VtKittyGraphicsPlacementIterator).cast(),
            )
        } != VtResult::SUCCESS
        {
            return out;
        }

        while unsafe { ghostty_kitty_graphics_placement_next(self.placement_iter) } {
            let iter = self.placement_iter;
            let image_id = placement_scalar::<u32>(iter, VtKittyGraphicsPlacementData::IMAGE_ID);
            let placement_id =
                placement_scalar::<u32>(iter, VtKittyGraphicsPlacementData::PLACEMENT_ID);

            let mut is_virtual = false;

            unsafe {
                ghostty_kitty_graphics_placement_get(
                    iter,
                    VtKittyGraphicsPlacementData::IS_VIRTUAL,
                    (&mut is_virtual as *mut bool).cast(),
                );
            }

            // Virtual placements have no engine viewport position (terminal reads the
            // placeholder cells instead), but carry the identity + grid size + z the
            // frame path needs to match placeholder runs to a placement.
            if is_virtual {
                out.push(SnapshotPlacement {
                    image_id,
                    placement_id,
                    is_virtual: true,
                    viewport_col: 0,
                    viewport_row: 0,
                    pixel_width: 0,
                    pixel_height: 0,
                    grid_cols: placement_scalar::<u32>(iter, VtKittyGraphicsPlacementData::COLUMNS),
                    grid_rows: placement_scalar::<u32>(iter, VtKittyGraphicsPlacementData::ROWS),
                    cell_x_offset: 0,
                    cell_y_offset: 0,
                    source_x: 0,
                    source_y: 0,
                    source_width: 0,
                    source_height: 0,
                    z: placement_scalar::<i32>(iter, VtKittyGraphicsPlacementData::Z),
                });

                continue;
            }

            // Geometry needs the image handle.
            let image = unsafe { ghostty_kitty_graphics_image(graphics, image_id) };

            if image.is_null() {
                continue;
            }

            let (mut vp_col, mut vp_row) = (0i32, 0i32);
            if unsafe {
                ghostty_kitty_graphics_placement_viewport_pos(
                    iter,
                    image,
                    self.terminal,
                    &mut vp_col,
                    &mut vp_row,
                )
            } != VtResult::SUCCESS
            {
                // Off-screen (NO_VALUE) — invisible this frame, nothing to paint.
                continue;
            }

            let (mut px_w, mut px_h) = (0u32, 0u32);

            unsafe {
                ghostty_kitty_graphics_placement_pixel_size(
                    iter,
                    image,
                    self.terminal,
                    &mut px_w,
                    &mut px_h,
                );
            }

            let (g_cols, g_rows, [sx, sy, sw, sh]) = placement_geometry(iter, image, self.terminal);

            out.push(SnapshotPlacement {
                image_id,
                placement_id,
                is_virtual: false,
                viewport_col: vp_col,
                viewport_row: vp_row,
                pixel_width: px_w,
                pixel_height: px_h,
                grid_cols: g_cols,
                grid_rows: g_rows,
                cell_x_offset: placement_scalar::<u32>(
                    iter,
                    VtKittyGraphicsPlacementData::X_OFFSET,
                ),
                cell_y_offset: placement_scalar::<u32>(
                    iter,
                    VtKittyGraphicsPlacementData::Y_OFFSET,
                ),
                source_x: sx,
                source_y: sy,
                source_width: sw,
                source_height: sh,
                z: placement_scalar::<i32>(iter, VtKittyGraphicsPlacementData::Z),
            });
        }

        out
    }

    /// Enumerate a finished block's Kitty placements with **block-relative**
    /// positions: `screen_col`/`screen_row`
    /// of the returned entries are in the block's own row space (the same
    /// rows `BlockRef::read_row_visit` reads). Requires the engine lock —
    /// the grid-size helpers read the live terminal's cell metrics — but the
    /// placements themselves come from the frozen storage pinned by `block`.
    /// Virtual placements and evicted pins are skipped; empty when graphics
    /// are disabled or the block has none (~2 FFI calls).
    pub fn block_placements(&mut self, block: &BlockRef) -> Vec<PlacementScreenPos> {
        let mut out = Vec::new();

        let Some(graphics) = block.kitty_graphics_raw() else {
            return out;
        };

        if unsafe {
            ghostty_kitty_graphics_get(
                graphics,
                VtKittyGraphicsData::PLACEMENT_ITERATOR,
                (&mut self.placement_iter as *mut VtKittyGraphicsPlacementIterator).cast(),
            )
        } != VtResult::SUCCESS
        {
            return out;
        }

        while unsafe { ghostty_kitty_graphics_placement_next(self.placement_iter) } {
            let iter = self.placement_iter;
            let (mut col, mut row) = (0u32, 0u32);

            if unsafe { ghostty_block_ref_placement_pos(block.raw, iter, &mut col, &mut row) }
                != VtResult::SUCCESS
            {
                // Virtual placement (unicode placeholder) — no pin to resolve.
                continue;
            }

            let image_id = placement_scalar::<u32>(iter, VtKittyGraphicsPlacementData::IMAGE_ID);
            let image = unsafe { ghostty_kitty_graphics_image(graphics, image_id) };

            if image.is_null() {
                continue;
            }
            let (g_cols, g_rows, [sx, sy, sw, sh]) = placement_geometry(iter, image, self.terminal);

            out.push(PlacementScreenPos {
                image_id,
                placement_id: placement_scalar::<u32>(
                    iter,
                    VtKittyGraphicsPlacementData::PLACEMENT_ID,
                ),
                screen_col: col,
                screen_row: row,
                grid_cols: g_cols,
                grid_rows: g_rows,
                source_x: sx,
                source_y: sy,
                source_width: sw,
                source_height: sh,
                z: placement_scalar::<i32>(iter, VtKittyGraphicsPlacementData::Z),
            });
        }

        out
    }

    /// Copy one frozen image's decoded pixels out of a finished block's
    /// Kitty storage. The caller keys the lazily uploaded result by
    /// `(block_id, image_id)`. `None` if the block holds no such image. Engine lock
    /// held by the caller; the pixels are copied out before returning.
    pub fn block_image_pixels(
        &self,
        block: &BlockRef,
        image_id: u32,
    ) -> Option<graphics::GraphicData> {
        let graphics = block.kitty_graphics_raw()?;
        let image = unsafe { ghostty_kitty_graphics_image(graphics, image_id) };

        if image.is_null() {
            return None;
        }

        let read_u32 = |data: VtKittyGraphicsImageData::Type| -> u32 {
            let mut v: u32 = 0;
            unsafe {
                ghostty_kitty_graphics_image_get(image, data, (&mut v as *mut u32).cast());
            }
            v
        };

        let width = read_u32(VtKittyGraphicsImageData::WIDTH);
        let height = read_u32(VtKittyGraphicsImageData::HEIGHT);

        let mut data_len: usize = 0;

        unsafe {
            ghostty_kitty_graphics_image_get(
                image,
                VtKittyGraphicsImageData::DATA_LEN,
                (&mut data_len as *mut usize).cast(),
            );
        }

        unsafe { kitty_image_graphic_data(image, image_id, width, height, data_len) }
    }

    /// Diff the live kitty images against the shipped set and return the pixel
    /// deltas. Called **only on the PTY reader thread** after `snapshot`
    /// (engine lock held by the caller); `placements` is that snapshot's placement
    /// list, so no second iterator walk. New or changed images (`(id,w,h,len)` key)
    /// have their pixels copied (`gray`/`gray_alpha` → rgba); vanished ids are
    /// reported for removal. Empty in steady state.
    pub fn take_image_deltas(
        &mut self,
        placements: &[SnapshotPlacement],
    ) -> (Vec<(u32, graphics::GraphicData)>, Vec<u32>) {
        let mut pending = Vec::new();
        let mut live: FxHashSet<u32> = FxHashSet::default();

        let mut graphics: VtKittyGraphics = ptr::null_mut();

        let have_graphics = unsafe {
            ghostty_terminal_get(
                self.terminal,
                VtTerminalData::KITTY_GRAPHICS,
                (&mut graphics as *mut VtKittyGraphics).cast(),
            )
        } == VtResult::SUCCESS
            && !graphics.is_null();

        if have_graphics {
            for p in placements {
                if !live.insert(p.image_id) {
                    continue;
                }

                let image = unsafe { ghostty_kitty_graphics_image(graphics, p.image_id) };

                if image.is_null() {
                    continue;
                }

                let read_u32 = |data: VtKittyGraphicsImageData::Type| -> u32 {
                    let mut v: u32 = 0;
                    unsafe {
                        ghostty_kitty_graphics_image_get(image, data, (&mut v as *mut u32).cast());
                    }
                    v
                };

                let width = read_u32(VtKittyGraphicsImageData::WIDTH);
                let height = read_u32(VtKittyGraphicsImageData::HEIGHT);

                let mut data_len: usize = 0;

                unsafe {
                    ghostty_kitty_graphics_image_get(
                        image,
                        VtKittyGraphicsImageData::DATA_LEN,
                        (&mut data_len as *mut usize).cast(),
                    );
                }

                let key = (width, height, data_len);

                if self.shipped_images.get(&p.image_id) == Some(&key) {
                    continue; // unchanged — already shipped
                }

                let Some(data) = (unsafe {
                    kitty_image_graphic_data(image, p.image_id, width, height, data_len)
                }) else {
                    continue;
                };

                pending.push((p.image_id, data));

                self.shipped_images.insert(p.image_id, key);
            }
        }

        // "Live" means the engine still holds the image, not that it has a visible
        // placement". An image scrolled off-screen is still live, so it must not
        // be evicted/re-shipped on scroll-back ("a scroll must not emit graphics
        // churn"). Removal fires only on kitty delete-with-free or storage
        // eviction. `live` (above) is only a per-batch placement dedup.
        let removed: Vec<u32> = self
            .shipped_images
            .keys()
            .copied()
            .filter(|id| {
                !have_graphics || unsafe { ghostty_kitty_graphics_image(graphics, *id) }.is_null()
            })
            .collect();

        for id in &removed {
            self.shipped_images.remove(id);
        }

        (pending, removed)
    }

    /// The engine's current 256-color palette (OSC 4 overrides applied). Used to
    /// resolve palette-tagged style colors into concrete RGB at read time — by
    /// the harvester (once per batch) and by app-side `BlockRef` readers (once
    /// per acquire).
    pub fn color_palette(&self) -> [VtColorRgb; 256] {
        let mut palette = [VtColorRgb::default(); 256];

        unsafe {
            let _ = ghostty_terminal_get(
                self.terminal,
                VtTerminalData::COLOR_PALETTE,
                palette.as_mut_ptr().cast(),
            );
        }

        palette
    }

    /// Convert a `GridRef` back to a coordinate in the given system. Returns
    /// `None` when the ref isn't representable there (e.g. a history cell asked in
    /// viewport coords). Used to anchor a clicked viewport cell to a stable
    /// `SCREEN` coordinate.
    pub fn point_from_grid_ref(
        &self,
        grid_ref: &VtGridRef,
        tag: VtPointTag::Type,
    ) -> Result<Option<(u16, u32)>> {
        let mut out = VtPointCoordinate::default();

        match unsafe {
            ghostty_terminal_point_from_grid_ref(self.terminal, grid_ref, tag, &mut out)
        } {
            VtResult::SUCCESS => Ok(Some((out.x, out.y))),
            VtResult::NO_VALUE => Ok(None),
            other => {
                Error::from_code(other)?;
                Ok(None)
            }
        }
    }

    /// Export terminal text via the engine formatter. `selection = None`
    /// formats the whole screen + scrollback; otherwise only the selection range.
    /// `unwrap` rejoins soft-wrapped lines (no inserted newline at a wrap point);
    /// `trim` drops trailing blanks. Used for selection-to-string and the search
    /// corpus.
    pub fn format_text(
        &mut self,
        selection: Option<&VtSelection>,
        unwrap: bool,
        trim: bool,
    ) -> Result<String> {
        self.format_terminal(VtFormatterFormat::PLAIN, selection, unwrap, trim)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    }

    /// Export the complete terminal state as a VT stream. Replaying the returned
    /// bytes reconstructs the current screen, styles, modes, palette, and cursor,
    /// which lets a newly attached client start from a consistent checkpoint.
    pub fn format_vt_state(&mut self) -> Result<Vec<u8>> {
        self.format_terminal(VtFormatterFormat::VT, None, false, false)
    }

    fn format_terminal(
        &mut self,
        emit: VtFormatterFormat::Type,
        selection: Option<&VtSelection>,
        unwrap: bool,
        trim: bool,
    ) -> Result<Vec<u8>> {
        let mut opts = vt_sized!(VtFormatterTerminalOptions);

        opts.emit = emit;
        opts.unwrap = unwrap;
        opts.trim = trim;
        opts.extra = vt_sized!(VtFormatterTerminalExtra);
        opts.selection = selection
            .map(|s| s as *const VtSelection)
            .unwrap_or(ptr::null());

        let mut formatter: VtFormatter = ptr::null_mut();

        Error::from_code(unsafe {
            ghostty_formatter_terminal_new(ptr::null(), &mut formatter, self.terminal, opts)
        })?;

        let mut out_ptr: *mut u8 = ptr::null_mut();
        let mut out_len: usize = 0;

        let res = Error::from_code(unsafe {
            ghostty_formatter_format_alloc(formatter, ptr::null(), &mut out_ptr, &mut out_len)
        });

        let bytes = res.map(|_| {
            if out_ptr.is_null() || out_len == 0 {
                Vec::new()
            } else {
                let bytes = unsafe { slice::from_raw_parts(out_ptr, out_len) };
                bytes.to_vec()
            }
        });

        if !out_ptr.is_null() {
            unsafe { ghostty_free(ptr::null(), out_ptr, out_len) };
        }

        unsafe { ghostty_formatter_free(formatter) };

        bytes
    }

    /// Selection-to-string for a SCREEN-coordinate range (inclusive endpoints).
    /// Used when the selection reaches past the viewport into scrollback:
    /// the O(scrollback) endpoint resolve is one-shot on copy, and the extract is
    /// O(selection).
    pub fn format_screen_range(
        &mut self,
        start: (u16, u32),
        end: (u16, u32),
        rectangle: bool,
        unwrap: bool,
        trim: bool,
    ) -> Result<String> {
        let start_ref = self.grid_ref_at(VtPointTag::SCREEN, start.0, start.1)?;
        let end_ref = self.grid_ref_at(VtPointTag::SCREEN, end.0, end.1)?;

        let mut sel = vt_sized!(VtSelection);

        sel.start = start_ref;
        sel.end = end_ref;
        sel.rectangle = rectangle;

        self.format_text(Some(&sel), unwrap, trim)
    }

    /// Scroll the viewport by `delta` rows (negative = up into scrollback).
    /// Mutating: invalidates any outstanding `GridRef`.
    pub fn scroll_viewport_delta(&mut self, delta: isize) {
        if self.scrollbar_override.is_some() {
            return;
        }

        self.scroll_viewport_delta_raw(delta);
    }

    fn scroll_viewport_delta_raw(&mut self, delta: isize) {
        let behavior = VtTerminalScrollViewport {
            tag: VtTerminalScrollViewportTag::DELTA,
            value: VtTerminalScrollViewportValue { delta },
        };

        unsafe { ghostty_terminal_scroll_viewport(self.terminal, behavior) };
    }

    /// Scroll the viewport to the bottom (active area).
    pub fn scroll_viewport_bottom(&mut self) {
        if self.scrollbar_override.is_some() {
            return;
        }

        let behavior = VtTerminalScrollViewport {
            tag: VtTerminalScrollViewportTag::BOTTOM,
            value: VtTerminalScrollViewportValue { delta: 0 },
        };

        unsafe { ghostty_terminal_scroll_viewport(self.terminal, behavior) };
    }

    /// Scroll the viewport to the top of the scrollback.
    pub fn scroll_viewport_top(&mut self) {
        if self.scrollbar_override.is_some() {
            return;
        }

        self.scroll_viewport_top_raw();
    }

    fn scroll_viewport_top_raw(&mut self) {
        let behavior = VtTerminalScrollViewport {
            tag: VtTerminalScrollViewportTag::TOP,
            value: VtTerminalScrollViewportValue { delta: 0 },
        };

        unsafe { ghostty_terminal_scroll_viewport(self.terminal, behavior) };
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

/// Read one datum of the placement the iterator is positioned on. `T` must be
/// the 32-bit integer type the FFI writes for `data` (u32 or i32).
fn placement_scalar<T: Default>(
    iter: VtKittyGraphicsPlacementIterator,
    data: VtKittyGraphicsPlacementData::Type,
) -> T {
    let mut v = T::default();

    unsafe {
        ghostty_kitty_graphics_placement_get(iter, data, (&mut v as *mut T).cast());
    }
    v
}

/// Grid size + resolved source rectangle of the current placement — the shared
/// tail of every placement walk. Returns `(grid_cols, grid_rows, [sx, sy, sw, sh])`.
fn placement_geometry(
    iter: VtKittyGraphicsPlacementIterator,
    image: VtKittyGraphicsImage,
    terminal: VtTerminal,
) -> (u32, u32, [u32; 4]) {
    let (mut g_cols, mut g_rows) = (0u32, 0u32);
    let (mut sx, mut sy, mut sw, mut sh) = (0u32, 0u32, 0u32, 0u32);

    unsafe {
        ghostty_kitty_graphics_placement_grid_size(iter, image, terminal, &mut g_cols, &mut g_rows);

        ghostty_kitty_graphics_placement_source_rect(
            iter, image, &mut sx, &mut sy, &mut sw, &mut sh,
        );
    }

    (g_cols, g_rows, [sx, sy, sw, sh])
}

/// Copy a decoded kitty image's pixels into a [`crate::graphics::GraphicData`],
/// converting gray forms to RGBA (the engine already decoded PNG/zlib, so
/// only raw pixel formats reach here). Shared by the live delta shipper and
/// the frozen-block lazy read.
///
/// # Safety
/// `image` must be a live image handle from the storage the caller currently
/// pins (engine lock or an acquired block ref).
unsafe fn kitty_image_graphic_data(
    image: VtKittyGraphicsImage,
    image_id: u32,
    width: u32,
    height: u32,
    data_len: usize,
) -> Option<graphics::GraphicData> {
    use crate::graphics::{ColorType, GraphicData, GraphicId};

    let mut format: VtKittyImageFormat::Type = VtKittyImageFormat::RGBA;

    unsafe {
        ghostty_kitty_graphics_image_get(
            image,
            VtKittyGraphicsImageData::FORMAT,
            (&mut format as *mut VtKittyImageFormat::Type).cast(),
        );
    }

    let mut data_ptr: *const u8 = ptr::null();

    unsafe {
        ghostty_kitty_graphics_image_get(
            image,
            VtKittyGraphicsImageData::DATA_PTR,
            (&mut data_ptr as *mut *const u8).cast(),
        );
    }

    if data_ptr.is_null() || data_len == 0 {
        return None;
    }

    let raw = unsafe { slice::from_raw_parts(data_ptr, data_len) };

    let (pixels, color_type) = match format {
        VtKittyImageFormat::RGB => (raw.to_vec(), ColorType::Rgb),
        VtKittyImageFormat::RGBA => (raw.to_vec(), ColorType::Rgba),
        VtKittyImageFormat::GRAY => {
            let mut px = Vec::with_capacity(raw.len() * 4);

            for &g in raw {
                px.extend_from_slice(&[g, g, g, 255]);
            }

            (px, ColorType::Rgba)
        }
        VtKittyImageFormat::GRAY_ALPHA => {
            let mut px = Vec::with_capacity(raw.len() * 2);

            for ga in raw.chunks_exact(2) {
                px.extend_from_slice(&[ga[0], ga[0], ga[0], ga[1]]);
            }

            (px, ColorType::Rgba)
        }
        _ => return None, // PNG/unknown shouldn't reach here post-decode
    };

    let is_opaque = color_type == ColorType::Rgb;

    Some(GraphicData {
        id: GraphicId(image_id as u64),
        width: width as usize,
        height: height as usize,
        color_type,
        pixels,
        is_opaque,
        resize: None,
        display_width: None,
        display_height: None,
        transmit_time: time::Instant::now(),
    })
}

impl Drop for GhosttyTerminal {
    fn drop(&mut self) {
        unsafe {
            ghostty_kitty_graphics_placement_iterator_free(self.placement_iter);
            ghostty_render_state_row_iterator_free(self.row_iter);
            ghostty_render_state_free(self.render_state);
            ghostty_terminal_free(self.terminal);
        }
    }
}

#[cfg(test)]
mod tests;
