/// Engine handle of a finished command block (per-block grid). Plain value
/// type; lookup is by id, `generation` is the data version for cache keys.
pub use libghostty_vt_sys::BlockHandle;

pub use crate::ghostty::block::{AcquiredBlock, BlockRef};
pub use crate::ghostty::error::{Error, Result};
pub use crate::ghostty::types::{
    CellText, CellWide, Color, Palette, PlacementScreenPos, RowCell, ScreenRowMeta, ScreenRowRead,
    ScrollbarInfo, SnapshotColors, SnapshotCursor, SnapshotPlacement, SnapshotStyle, Underline,
};

/// VT mode identifiers for [`GhosttyTerminal::mode`].
///
/// Values mirror Ghostty's `ModeTag` (packed `u16`): a DEC private mode uses its
/// raw number; an ANSI mode sets bit 15. See Ghostty `src/terminal/modes.zig`.
pub mod mode;

mod block;
mod callbacks;
mod error;
mod format;
mod grid_read;
mod kitty;
mod render_state;

mod types;

#[cfg(test)]
mod tests;

#[cfg(test)]
use std::sync;
use std::{array, mem, os, path, ptr, slice};

#[cfg(test)]
use libghostty_vt_sys::RowSemanticPrompt as VtRowSemanticPrompt;
use libghostty_vt_sys::{
    BlockRef as VtBlockRef, ColorRgb as VtColorRgb, FormatterFormat as VtFormatterFormat,
    GridRef as VtGridRef, KITTY_KEY_DISAMBIGUATE, KITTY_KEY_REPORT_ALL,
    KITTY_KEY_REPORT_ALTERNATES, KITTY_KEY_REPORT_ASSOCIATED, KITTY_KEY_REPORT_EVENTS,
    KittyGraphics as VtKittyGraphics, KittyGraphicsImageData as VtKittyGraphicsImageData,
    Point as VtPoint, PointCoordinate as VtPointCoordinate, PointTag as VtPointTag,
    PointValue as VtPointValue, Result as VtResult, Selection as VtSelection, String as VtString,
    Terminal as VtTerminal, TerminalCursorStyle as VtTerminalCursorStyle,
    TerminalData as VtTerminalData, TerminalModeConfig as VtTerminalModeConfig,
    TerminalOption as VtTerminalOption, TerminalScrollViewport as VtTerminalScrollViewport,
    TerminalScrollViewportTag as VtTerminalScrollViewportTag,
    TerminalScrollViewportValue as VtTerminalScrollViewportValue,
    TerminalScrollbar as VtTerminalScrollbar, ghostty_block_ref_cols, ghostty_kitty_graphics_image,
    ghostty_kitty_graphics_image_get, ghostty_terminal_block_acquire, ghostty_terminal_block_at,
    ghostty_terminal_block_cols, ghostty_terminal_block_count, ghostty_terminal_block_grid_ref,
    ghostty_terminal_block_row_count, ghostty_terminal_blocks_bytes, ghostty_terminal_clear_blocks,
    ghostty_terminal_finish_block, ghostty_terminal_free, ghostty_terminal_get,
    ghostty_terminal_grid_ref, ghostty_terminal_new, ghostty_terminal_point_from_grid_ref,
    ghostty_terminal_remove_block, ghostty_terminal_resize, ghostty_terminal_scroll_viewport,
    ghostty_terminal_set, ghostty_terminal_vt_write, sized as vt_sized,
};
#[cfg(test)]
use nmt_config::colors::ColorRgb;
use nmt_config::colors::Colors;

use crate::ghostty::callbacks::{
    Callbacks, KITTY_IMAGE_STORAGE_LIMIT_BYTES, bell_cb, clipboard_write_cb, register_png_decoder,
    write_pty_cb,
};
use crate::ghostty::format::format_terminal;
use crate::ghostty::grid_read::visit_row_cells;
use crate::ghostty::kitty::{KittyState, kitty_image_graphic_data};
use crate::ghostty::render_state::RenderStateReader;
use crate::pwd::pwd_to_path;
use crate::render_buffer::RenderBuffer;
use crate::{ansi, clipboard, graphics, terminal};

/// What the engine last reported for the title and the working directory.
///
/// Both are read fresh from the engine on every poll, so the only thing kept
/// here is the previous answer: it is what turns an unconditional read into a
/// change report, and neither value is used for anything else.
#[derive(Default)]
struct TitleMirror {
    title: String,
    pwd: String,
}

impl TitleMirror {
    /// Report `latest` only when it differs from the last reported title.
    fn note_title(&mut self, latest: String) -> Option<String> {
        if latest == self.title {
            return None;
        }

        self.title = latest.clone();

        Some(latest)
    }

    /// Report `latest` only when it differs from the last reported directory.
    fn note_pwd(&mut self, latest: String) -> Option<String> {
        if latest == self.pwd {
            return None;
        }

        self.pwd = latest.clone();

        Some(latest)
    }
}

pub struct GhosttyTerminal {
    terminal: VtTerminal,
    render: RenderStateReader,
    kitty: KittyState,
    cols: u16,
    rows: u16,

    /// Boxed so its heap address stays fixed across `GhosttyTerminal` moves;
    /// registered with the engine as the callback userdata pointer.
    callbacks: Box<Callbacks>,

    titles: TitleMirror,
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

        Error::from_code(unsafe { ghostty_terminal_new(ptr::null(), &mut terminal, cols, rows) })?;

        // A new terminal starts on the engine's own scrollback default, so the
        // caller's budget has to be applied before any output reaches it. A
        // rejected budget is the caller's error, as it was when the budget was
        // a construction parameter, so the half-built terminal is released.
        let scrollback = unsafe {
            ghostty_terminal_set(
                terminal,
                VtTerminalOption::SCROLLBACK_MAX_BYTES,
                (&max_scrollback as *const usize).cast(),
            )
        };

        if let Err(err) = Error::from_code(scrollback) {
            unsafe { ghostty_terminal_free(terminal) };

            return Err(err);
        }

        // The reader frees its own handles when it goes out of scope, so the
        // failure paths below only have to release the terminal.
        let render = match RenderStateReader::new(rows) {
            Ok(render) => render,

            Err(err) => {
                unsafe { ghostty_terminal_free(terminal) };

                return Err(err);
            }
        };

        let kitty = match KittyState::new() {
            Ok(kitty) => kitty,

            Err(err) => {
                unsafe { ghostty_terminal_free(terminal) };

                return Err(err);
            }
        };

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
        if nmt_platform::USES_CONPTY {
            unsafe {
                let seq = b"\x1b[?2027h";

                ghostty_terminal_vt_write(terminal, seq.as_ptr(), seq.len());
            }
        }

        Ok(Self {
            terminal,
            render,
            kitty,
            cols,
            rows,
            callbacks,
            titles: TitleMirror::default(),
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
        let title = self.read_string(VtTerminalData::TITLE);

        self.titles.note_title(title)
    }

    /// Poll the working directory (OSC 7); returns `Some(pwd)` only when it
    /// changed since the last poll.
    pub fn poll_pwd(&mut self) -> Option<String> {
        let pwd = self.read_string(VtTerminalData::PWD);

        self.titles.note_pwd(pwd)
    }

    /// The current OSC window title (peek — reads the engine's live value, no
    /// change-detection). `poll_title` is the producer's change-detecting variant;
    /// this is for on-demand frontend reads (title template), replacing the mirror.
    pub fn title(&self) -> String {
        self.read_string(VtTerminalData::TITLE)
    }

    /// The current OSC 7 working directory (peek) as a path, or `None` when unset.
    /// Replaces the mirror's `current_directory` for the title template.
    pub fn current_directory(&self) -> Option<path::PathBuf> {
        let pwd = self.read_string(VtTerminalData::PWD);

        if pwd.is_empty() {
            None
        } else {
            Some(pwd_to_path(&pwd))
        }
    }

    /// Read a `GhosttyString`-typed terminal datum as an owned `String`. The
    /// borrowed pointer is only valid until the next mutating call, so we copy
    /// immediately.
    fn read_string(&self, data: VtTerminalData::Type) -> String {
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
        // The mode identifier goes in and its value comes back out through the
        // same config struct, so it is both the input and the output here.
        let mut config = VtTerminalModeConfig {
            mode: id,
            value: false,
        };

        let ok = unsafe {
            ghostty_terminal_get(
                self.terminal,
                VtTerminalData::MODE,
                (&mut config as *mut VtTerminalModeConfig).cast(),
            )
        };

        ok == VtResult::SUCCESS && config.value
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
        use nmt_config::colors::{ColorArray, ColorRgb, NamedColor};

        let list: List = colors.into();

        let to_rgb = |color: ColorArray| {
            let color: ColorRgb = color.into();

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

    /// Finish the current command block: freeze the primary screen into the
    /// engine's block set (O(1) ownership move) and continue on a fresh
    /// primary screen with writer state carried over. Returns `None` when
    /// the active screen has no content (no block created). Errors with
    /// `InvalidValue` if the alternate screen is active — callers gate on
    /// the primary screen because alternate-screen content should not enter history.
    pub fn finish_block(&mut self) -> Result<Option<BlockHandle>> {
        let mut handle = BlockHandle::default();

        match unsafe { ghostty_terminal_finish_block(self.terminal, &mut handle) } {
            VtResult::SUCCESS => Ok(Some(handle)),
            VtResult::NO_VALUE => Ok(None),

            other => {
                Error::from_code(other)?;

                Ok(None)
            }
        }
    }

    /// Remove and destroy all finished blocks (user clear; `;K` path).
    pub fn clear_blocks(&mut self) {
        unsafe { ghostty_terminal_clear_blocks(self.terminal) }
    }

    /// Remove and destroy one finished block. Returns `false` for a stale
    /// handle (already removed/evicted).
    pub fn remove_block(&mut self, handle: BlockHandle) -> bool {
        (unsafe { ghostty_terminal_remove_block(self.terminal, handle) }) == VtResult::SUCCESS
    }

    pub fn block_count(&self) -> usize {
        unsafe { ghostty_terminal_block_count(self.terminal) }
    }

    /// The handle of the finished block at `index`, oldest first.
    pub fn block_at(&self, index: usize) -> Option<BlockHandle> {
        let mut handle = BlockHandle::default();

        (unsafe { ghostty_terminal_block_at(self.terminal, index, &mut handle) }
            == VtResult::SUCCESS)
            .then_some(handle)
    }

    /// Logical row count of a finished block (trailing blanks after the
    /// finish-time cursor truncated). `None` for a stale handle.
    pub fn block_row_count(&self, handle: BlockHandle) -> Option<usize> {
        let mut rows: usize = 0;

        (unsafe { ghostty_terminal_block_row_count(self.terminal, handle, &mut rows) }
            == VtResult::SUCCESS)
            .then_some(rows)
    }

    /// The column count the block was frozen at (can differ from the live
    /// terminal width after a resize). `None` for a stale handle.
    pub fn block_cols(&self, handle: BlockHandle) -> Option<u16> {
        let mut cols: u16 = 0;

        (unsafe { ghostty_terminal_block_cols(self.terminal, handle, &mut cols) }
            == VtResult::SUCCESS)
            .then_some(cols)
    }

    /// Total page-storage bytes of all finished blocks — the value the
    /// block byte budget is enforced against.
    pub fn blocks_bytes(&self) -> usize {
        unsafe { ghostty_terminal_blocks_bytes(self.terminal) }
    }

    /// Set the finished-block byte budget. Oldest blocks are evicted
    /// immediately (and on every finish) while the total exceeds it; the
    /// newest block is never evicted. Zero means unlimited.
    pub fn set_block_budget_bytes(&mut self, bytes: usize) -> Result<()> {
        Error::from_code(unsafe {
            ghostty_terminal_set(
                self.terminal,
                VtTerminalOption::BLOCK_BUDGET_BYTES,
                (&bytes as *const usize).cast(),
            )
        })
    }

    /// Take a read reference on a finished block (engine-refcounted; any
    /// thread). `None` for a stale handle or while the engine is
    /// reflowing the block — retry next frame. The reference pins an
    /// immutable snapshot: the block cannot be freed or mutated while it
    /// is held, and reads through it take no engine lock. Keep it
    /// short-lived (one read pass) — a held reference blocks the writer's
    /// resize reflow.
    pub fn block_acquire(&self, handle: BlockHandle) -> Option<BlockRef> {
        let mut raw: VtBlockRef = ptr::null_mut();

        if unsafe { ghostty_terminal_block_acquire(self.terminal, handle, &mut raw) }
            != VtResult::SUCCESS
            || raw.is_null()
        {
            return None;
        }

        let mut cols: u16 = 0;

        unsafe {
            let _ = ghostty_block_ref_cols(raw, &mut cols);
        }

        Some(BlockRef { raw, cols })
    }

    /// [`Self::block_acquire`] plus everything a frame's read pass needs
    /// from under the engine lock in one call: the palette styles resolve
    /// against and the block's Kitty placements in block-relative
    /// coordinates. Every subsequent text read through the
    /// returned reference is lock-free.
    pub fn acquire_block_snapshot(&mut self, handle: BlockHandle) -> Option<AcquiredBlock> {
        let block = self.block_acquire(handle)?;
        let palette = self.color_palette();
        let placements = self.block_placements(&block);

        Some(AcquiredBlock {
            block,
            palette,
            placements,
        })
    }

    /// Walk one row of a finished block with styles — the frozen-block
    /// counterpart of [`Self::read_screen_row_visit`]. Returns `None` for a
    /// stale handle or a row at/beyond the block's logical row count.
    /// Unlike active-screen refs, block refs stay valid until the block is
    /// removed, but this still reads within one call (same visitor shape).
    pub fn read_block_row_visit(
        &self,
        handle: BlockHandle,
        row: usize,
        palette: &[VtColorRgb; 256],
        on_cell: impl FnMut(u16, CellText, CellWide, SnapshotStyle),
    ) -> Result<Option<ScreenRowMeta>> {
        let mut grid_ref = VtGridRef::default();

        match unsafe { ghostty_terminal_block_grid_ref(self.terminal, handle, row, &mut grid_ref) }
        {
            VtResult::SUCCESS => {}
            VtResult::NO_VALUE | VtResult::INVALID_VALUE => return Ok(None),

            other => {
                Error::from_code(other)?;

                return Ok(None);
            }
        }

        let cols = self.block_cols(handle).unwrap_or(self.cols);

        Ok(Some(visit_row_cells(grid_ref, cols, palette, on_cell)?))
    }

    /// Materializing convenience over [`Self::read_block_row_visit`] — test-only.
    pub fn read_block_row(&self, handle: BlockHandle, row: usize) -> Result<Option<ScreenRowRead>> {
        let palette = self.color_palette();
        let cols = self.block_cols(handle).unwrap_or(self.cols) as usize;
        let mut cells = Vec::with_capacity(cols);

        let meta = self.read_block_row_visit(handle, row, &palette, |x, text, wide, style| {
            cells.push(RowCell {
                x,
                text,
                wide,
                style,
            })
        })?;

        Ok(meta.map(|meta| ScreenRowRead {
            cells,
            wrapped: meta.wrapped,
            prompt_start: meta.prompt_start,
            hyperlinks: meta.hyperlinks,
        }))
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
        format_terminal(
            self.terminal,
            VtFormatterFormat::PLAIN,
            selection,
            unwrap,
            trim,
        )
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    }

    /// Export the complete terminal state as a VT stream. Replaying the returned
    /// bytes reconstructs the current screen, styles, modes, palette, and cursor,
    /// which lets a newly attached client start from a consistent checkpoint.
    pub fn format_vt_state(&mut self) -> Result<Vec<u8>> {
        format_terminal(self.terminal, VtFormatterFormat::VT, None, false, false)
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

    /// Resolve a point (in the given coordinate system) to a `GridRef`. Fast for
    /// `VIEWPORT`/`ACTIVE`; **O(scrollback) for `SCREEN`/`HISTORY`**. The ref is
    /// valid only until the next mutating call (`write_vt`/`resize`/
    /// `scroll_viewport`) — use it within one read pass, never cache it.
    pub fn grid_ref_at(&self, tag: VtPointTag::Type, x: u16, y: u32) -> Result<VtGridRef> {
        let point = VtPoint {
            tag,
            value: VtPointValue {
                coordinate: VtPointCoordinate { x, y },
            },
        };

        let mut grid_ref = VtGridRef::default();

        Error::from_code(unsafe {
            ghostty_terminal_grid_ref(self.terminal, point, &mut grid_ref)
        })?;

        Ok(grid_ref)
    }

    /// The SCREEN row of the top visible row (`viewport_top`) — the constant that
    /// maps between SCREEN and visible coordinates (`screen_row = viewport_top +
    /// visible_row`). One cheap viewport `grid_ref`; `None` if the viewport is
    /// empty. Selection rendering uses this to translate coordinate spaces.
    pub fn viewport_top_screen(&self) -> Option<u32> {
        let r = self.grid_ref_at(VtPointTag::VIEWPORT, 0, 0).ok()?;

        self.point_from_grid_ref(&r, VtPointTag::SCREEN)
            .ok()
            .flatten()
            .map(|(_, y)| y)
    }

    /// Read one absolute `SCREEN` row into a materialized `Vec` — test-only
    /// convenience over [`Self::read_screen_row_visit`].
    pub fn read_screen_row(&self, row: u32) -> Result<Option<ScreenRowRead>> {
        let mut cells = Vec::with_capacity(self.cols as usize);

        let meta =
            self.read_screen_row_visit(row, &self.color_palette(), |x, text, wide, style| {
                cells.push(RowCell {
                    x,
                    text,
                    wide,
                    style,
                })
            })?;

        Ok(meta.map(|meta| ScreenRowRead {
            cells,
            wrapped: meta.wrapped,
            prompt_start: meta.prompt_start,
            hyperlinks: meta.hyperlinks,
        }))
    }

    /// Walk one absolute `SCREEN` row with styles, invoking `on_cell` for each
    /// content cell (sparse: blank default cells are skipped) instead of
    /// materializing a `Vec` — the harvester constructs its `LineCell`s in
    /// place, so no intermediate row buffer exists on the freeze hot path.
    /// Colors resolve against a caller-supplied palette (hoisted out of
    /// per-row cost: the palette is a 256-entry FFI copy and cannot change
    /// while the engine lock is held). Reaches any scrollback row without
    /// moving the viewport or refreshing the render state. Returns `None`
    /// when `row` is out of range.
    ///
    /// The pin lookup is O(scrollback page hops); per-cell reads are O(cols).
    /// The `GridRef`s are created and dropped within this call so mutations cannot
    /// invalidate a cached reference.
    /// Per-cell FFI is tag-driven: blank/plain-codepoint cells never touch the
    /// grapheme or style readers, keeping the row-harvest hot path free of unnecessary FFI.
    pub fn read_screen_row_visit(
        &self,
        row: u32,
        palette: &[VtColorRgb; 256],
        on_cell: impl FnMut(u16, CellText, CellWide, SnapshotStyle),
    ) -> Result<Option<ScreenRowMeta>> {
        let grid_ref = match self.grid_ref_at(VtPointTag::SCREEN, 0, row) {
            Ok(r) => r,
            Err(Error::InvalidValue) => return Ok(None),
            Err(e) => return Err(e),
        };

        Ok(Some(visit_row_cells(
            grid_ref, self.cols, palette, on_cell,
        )?))
    }

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

    /// Screen positions of every kitty placement pinned by one frozen block.
    pub fn block_placements(&mut self, block: &BlockRef) -> Vec<PlacementScreenPos> {
        self.kitty.block_placements(self.terminal, block)
    }

    /// Image pixels the frontend has not been sent yet, plus the ids the
    /// engine has dropped.
    pub fn take_image_deltas(
        &mut self,
        placements: &[SnapshotPlacement],
    ) -> (Vec<(u32, graphics::GraphicData)>, Vec<u32>) {
        self.kitty.take_image_deltas(self.terminal, placements)
    }

    /// Probe: whether any visible row carries a PROMPT semantic tag (command-blocks-
    /// rendering — mark-forwarding regression checks in terminal pipeline tests).
    #[cfg(test)]
    pub(crate) fn has_prompt_tagged_row(&mut self) -> bool {
        self.semantic_prompt_tags()
            .map(|tags| tags.contains(&VtRowSemanticPrompt::PROMPT))
            .unwrap_or(false)
    }

    /// The engine's `SEMANTIC_PROMPT` tag per visible row.
    #[cfg(test)]
    fn semantic_prompt_tags(&mut self) -> Result<Vec<VtRowSemanticPrompt::Type>> {
        self.render.row_semantic_prompts(self.terminal, self.rows)
    }

    /// Populate a reusable render buffer from the full visible viewport.
    pub fn snapshot_into(&mut self, buffer: &mut RenderBuffer) -> Result<()> {
        self.render.update(self.terminal)?;
        self.render.consume_damage(self.rows)?;

        let cursor = self.render.cursor().unwrap_or(SnapshotCursor {
            x: 0,
            y: 0,
            visible: false,
            shape: ansi::CursorShape::Block,
            blinking: false,
        });

        let palette = self.color_palette();

        buffer.begin_capture(self.cols as usize, self.rows as usize);
        buffer.viewport_top = self.viewport_top_screen();
        buffer.title = self.title();

        buffer.current_directory = self
            .current_directory()
            .map(|path| path.to_string_lossy().into_owned());

        // A transient row lookup failure blanks only that row; publishing the
        // remaining viewport is safer than withholding an otherwise valid frame.
        for y in 0..self.rows {
            let meta = self
                .grid_ref_at(VtPointTag::VIEWPORT, 0, y as u32)
                .and_then(|grid_ref| {
                    visit_row_cells(grid_ref, self.cols, &palette, |x, text, wide, style| {
                        buffer.write_cell(x as usize, y as usize, text.as_str(), wide, &style);
                    })
                })
                .unwrap_or_default();

            buffer.write_row_meta(y as usize, meta);
        }

        let colors = self.render.colors(self.terminal);
        let placements = self.kitty.placements(self.terminal);
        let scrollbar = self.scrollbar();

        buffer.finish_capture(
            cursor,
            colors,
            placements,
            scrollbar,
            self.render.row_versions(),
        );

        Ok(())
    }

    /// Allocate and populate an owned render buffer for diagnostics and tests.
    pub fn snapshot(&mut self) -> Result<RenderBuffer> {
        let mut buffer = RenderBuffer::new(self.cols as usize, self.rows as usize);

        self.snapshot_into(&mut buffer)?;

        Ok(buffer)
    }
}

impl Drop for GhosttyTerminal {
    fn drop(&mut self) {
        unsafe { ghostty_terminal_free(self.terminal) };
    }
}
