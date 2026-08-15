#[cfg(test)]
use std::sync;
use std::{array, mem, os, path, ptr, slice};

/// Engine handle of a finished command block (per-block grid). Plain value
/// type; lookup is by id, `generation` is the data version for cache keys.
pub use libghostty_vt_sys::BlockHandle;
use libghostty_vt_sys::{
    ColorRgb as VtColorRgb, GridRef as VtGridRef, KITTY_KEY_DISAMBIGUATE, KITTY_KEY_REPORT_ALL,
    KITTY_KEY_REPORT_ALTERNATES, KITTY_KEY_REPORT_ASSOCIATED, KITTY_KEY_REPORT_EVENTS,
    KittyGraphics as VtKittyGraphics,
    KittyGraphicsPlacementIterator as VtKittyGraphicsPlacementIterator,
    PointCoordinate as VtPointCoordinate, PointTag as VtPointTag, RenderState as VtRenderState,
    RenderStateRowIterator as VtRenderStateRowIterator, Result as VtResult, String as VtString,
    Terminal as VtTerminal, TerminalCursorStyle as VtTerminalCursorStyle,
    TerminalData as VtTerminalData, TerminalOption as VtTerminalOption,
    TerminalOptions as VtTerminalOptions, TerminalScrollViewport as VtTerminalScrollViewport,
    TerminalScrollViewportTag as VtTerminalScrollViewportTag,
    TerminalScrollViewportValue as VtTerminalScrollViewportValue,
    TerminalScrollbar as VtTerminalScrollbar, ghostty_kitty_graphics_image,
    ghostty_kitty_graphics_placement_iterator_free, ghostty_kitty_graphics_placement_iterator_new,
    ghostty_render_state_free, ghostty_render_state_new, ghostty_render_state_row_iterator_free,
    ghostty_render_state_row_iterator_new, ghostty_terminal_free, ghostty_terminal_get,
    ghostty_terminal_mode_get, ghostty_terminal_new, ghostty_terminal_point_from_grid_ref,
    ghostty_terminal_resize, ghostty_terminal_scroll_viewport, ghostty_terminal_set,
    ghostty_terminal_vt_write,
};
#[cfg(test)]
use libghostty_vt_sys::{
    RowSemanticPrompt as VtRowSemanticPrompt, Selection as VtSelection, sized as vt_sized,
};
#[cfg(test)]
use nmt_config::colors::ColorRgb;
use nmt_config::colors::Colors;
use rustc_hash::FxHashMap;

#[cfg(test)]
use crate::graphics;
use crate::pwd::pwd_to_path;
#[cfg(test)]
use crate::render_buffer::RenderBuffer;
use crate::{ansi, clipboard, terminal};

mod block;
mod callbacks;
mod error;
mod format;
mod grid_read;
mod kitty;
mod render_state;
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
