use std::{fmt, ptr};

use libghostty_vt_sys as vt;
/// Engine handle of a finished command block (per-block grid). Plain value
/// type; lookup is by id, `generation` is the data version for cache keys.
pub use libghostty_vt_sys::BlockHandle;

use crate::render_buffer::RenderBuffer;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    OutOfMemory,
    InvalidValue,
    OutOfSpace,
    NoValue,
    Unknown(i32),
}

impl Error {
    fn from_code(code: vt::Result::Type) -> Result<()> {
        match code {
            vt::Result::SUCCESS => Ok(()),
            vt::Result::OUT_OF_MEMORY => Err(Self::OutOfMemory),
            vt::Result::INVALID_VALUE => Err(Self::InvalidValue),
            vt::Result::OUT_OF_SPACE => Err(Self::OutOfSpace),
            vt::Result::NO_VALUE => Err(Self::NoValue),
            other => Err(Self::Unknown(other)),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutOfMemory => f.write_str("libghostty-vt allocation failed"),
            Self::InvalidValue => {
                f.write_str("libghostty-vt received or returned an invalid value")
            }
            Self::OutOfSpace => f.write_str("buffer is too small for libghostty-vt output"),
            Self::NoValue => f.write_str("libghostty-vt value is absent"),
            Self::Unknown(code) => write!(f, "unknown libghostty-vt result code {code}"),
        }
    }
}

impl std::error::Error for Error {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl From<vt::ColorRgb> for Color {
    fn from(value: vt::ColorRgb) -> Self {
        let mut r = 0;
        let mut g = 0;
        let mut b = 0;
        unsafe {
            // ghostty 53bd14f: the accessor takes the color by const pointer.
            vt::ghostty_color_rgb_get(&value, &mut r, &mut g, &mut b);
        }
        Self { r, g, b }
    }
}

/// Style flags for a cell. `fg`/`bg` are `None` when the cell uses the
/// terminal default color (the caller resolves the default).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SnapshotStyle {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub bold: bool,
    pub italic: bool,
    pub faint: bool,
    pub blink: bool,
    pub inverse: bool,
    pub invisible: bool,
    pub strikethrough: bool,
    pub overline: bool,
    pub underline: Underline,
    pub underline_color: Option<Color>,
}

/// Underline style, mirroring `vt::SgrUnderline::*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Underline {
    #[default]
    None,
    Single,
    Double,
    Curly,
    Dotted,
    Dashed,
}

impl From<vt::SgrUnderline::Type> for Underline {
    fn from(value: vt::SgrUnderline::Type) -> Self {
        match value {
            v if v == vt::SgrUnderline::SINGLE => Self::Single,
            v if v == vt::SgrUnderline::DOUBLE => Self::Double,
            v if v == vt::SgrUnderline::CURLY => Self::Curly,
            v if v == vt::SgrUnderline::DOTTED => Self::Dotted,
            v if v == vt::SgrUnderline::DASHED => Self::Dashed,
            _ => Self::None,
        }
    }
}

/// Width classification of a cell, mirroring `vt::CellWide::*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CellWide {
    #[default]
    Narrow,
    /// First cell of a double-width character.
    Wide,
    /// Second cell of a double-width character (no glyph).
    SpacerTail,
    /// Padding before a wide char at a soft-wrap boundary (no glyph).
    SpacerHead,
}

impl From<i32> for CellWide {
    fn from(value: i32) -> Self {
        match value {
            v if v == vt::CellWide::WIDE => Self::Wide,
            v if v == vt::CellWide::SPACER_TAIL => Self::SpacerTail,
            v if v == vt::CellWide::SPACER_HEAD => Self::SpacerHead,
            _ => Self::Narrow,
        }
    }
}

/// The engine's resolved 256-color palette, fetched once per lock hold and
/// threaded through batch row reads (see `read_screen_row_visit`).
pub type Palette = [vt::ColorRgb; 256];

/// Cell text with inline storage for short content. Almost every cell is a
/// single codepoint (≤4 UTF-8 bytes); heap-allocating a `String` per cell was
/// the dominant harvest cost (61 ns/cell, 91% of PTY time under scroll floods).
/// Content up to 22 bytes — every single codepoint and all common grapheme
/// clusters — stays inline; longer clusters (rare ZWJ chains) spill to heap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellText(CellTextRepr);

#[derive(Debug, Clone, PartialEq, Eq)]
enum CellTextRepr {
    Inline { len: u8, buf: [u8; 22] },
    Heap(String),
}

impl CellText {
    pub fn from_char(c: char) -> Self {
        let mut buf = [0u8; 22];
        let len = c.encode_utf8(&mut buf).len() as u8;
        CellText(CellTextRepr::Inline { len, buf })
    }

    pub fn as_str(&self) -> &str {
        match &self.0 {
            // Invariant: constructors only store valid UTF-8 prefixes.
            CellTextRepr::Inline { len, buf } => unsafe {
                std::str::from_utf8_unchecked(&buf[..*len as usize])
            },
            CellTextRepr::Heap(s) => s,
        }
    }

    pub fn is_empty(&self) -> bool {
        match &self.0 {
            CellTextRepr::Inline { len, .. } => *len == 0,
            CellTextRepr::Heap(s) => s.is_empty(),
        }
    }
}

impl Default for CellText {
    fn default() -> Self {
        CellText(CellTextRepr::Inline {
            len: 0,
            buf: [0; 22],
        })
    }
}

impl From<&str> for CellText {
    fn from(s: &str) -> Self {
        if s.len() <= 22 {
            let mut buf = [0u8; 22];
            buf[..s.len()].copy_from_slice(s.as_bytes());
            CellText(CellTextRepr::Inline {
                len: s.len() as u8,
                buf,
            })
        } else {
            CellText(CellTextRepr::Heap(s.to_string()))
        }
    }
}

impl From<String> for CellText {
    fn from(s: String) -> Self {
        if s.len() <= 22 {
            CellText::from(s.as_str())
        } else {
            CellText(CellTextRepr::Heap(s))
        }
    }
}

impl std::ops::Deref for CellText {
    type Target = str;
    fn deref(&self) -> &str {
        self.as_str()
    }
}

/// One sparse cell of a [`ScreenRowRead`], with inline [`CellText`] instead of
/// a per-cell `String`.
/// Test-only: production reads visit cells in place (`read_screen_row_visit`).
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowCell {
    pub x: u16,
    /// Grapheme cluster for the cell. Empty for blank cells.
    pub text: CellText,
    pub wide: CellWide,
    pub style: SnapshotStyle,
}

/// Row-level results of a [`GhosttyTerminal::read_screen_row_visit`] walk —
/// everything about the row that is not a per-cell callback.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScreenRowMeta {
    /// This row soft-wraps into the next one (logical-line join point).
    pub wrapped: bool,
    /// OSC 133 `;A` tag: this row starts a prompt (harvest attribution anchor).
    pub prompt_start: bool,
    /// This row holds a kitty unicode placeholder.
    pub virtual_placeholder: bool,
    /// OSC 8 spans: `(start_col, end_col_inclusive, uri)`.
    pub hyperlinks: Vec<(u16, u16, String)>,
}

/// A materialized styled row read — test-only convenience over the visitor.
/// `cells` follows the snapshot convention: sparse (blank default cells are
/// skipped), ascending `x`, with `y` fixed at 0 (row-local).
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenRowRead {
    pub cells: Vec<RowCell>,
    /// This row soft-wraps into the next one (logical-line join point).
    pub wrapped: bool,
    /// OSC 133 `;A` tag: this row starts a prompt (harvest attribution anchor).
    pub prompt_start: bool,
    /// OSC 8 spans: `(start_col, end_col_inclusive, uri)`.
    pub hyperlinks: Vec<(u16, u16, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotCursor {
    pub x: u16,
    pub y: u16,
    /// `true` when the cursor should be shown — DECTCEM on **and** within the
    /// viewport (render-state `CURSOR_VISIBLE` ∧ `CURSOR_VIEWPORT_HAS_VALUE`).
    pub visible: bool,
    /// DECSCUSR shape from the render-state `CURSOR_VISUAL_STYLE`.
    pub shape: crate::ansi::CursorShape,
    /// Modes-based blink from the render-state `CURSOR_BLINKING`.
    pub blinking: bool,
}

/// The terminal's effective default colors from the render state:
/// `fg`/`bg` (OSC 10/11, always present) and `cursor` (OSC 12, only when set).
/// These become the `term_colors` OSC-override layer over the config palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SnapshotColors {
    pub fg: nmt_config::colors::ColorRgb,
    pub bg: nmt_config::colors::ColorRgb,
    pub cursor: Option<nmt_config::colors::ColorRgb>,
    /// The **effective** window background (terminal-level `COLOR_BACKGROUND`): an
    /// OSC 11 override, or the config default pushed at init, or `None` when no bg
    /// is set at all. The renderer compares it to the config default to tell an OSC
    /// 11 override from a reset/default and keep config opacity/image.
    pub bg_override: Option<nmt_config::colors::ColorRgb>,
}

/// A kitty-graphics placement captured from the engine. Positions are
/// **viewport-relative** (`placement_viewport_pos` already did scroll/cull), so the
/// renderer uses them directly — no `dest_row − (history_size − display_offset)`.
/// Virtual placements (unicode placeholders) carry only `image_id`/`is_virtual`
/// (the engine returns no position for them); terminal positions those from the
/// placeholder cells instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotPlacement {
    pub image_id: u32,
    /// Kitty placement ID. Distinguishes multiple placements of one image; needed
    /// to associate virtual-placeholder runs with the right placement.
    pub placement_id: u32,
    pub is_virtual: bool,
    /// Viewport-relative top-left. `row` may be negative (scrolled partly above).
    pub viewport_col: i32,
    pub viewport_row: i32,
    /// Rendered pixel size of the placement.
    pub pixel_width: u32,
    pub pixel_height: u32,
    /// Grid cells the placement spans.
    pub grid_cols: u32,
    pub grid_rows: u32,
    /// Sub-cell pixel offsets (kitty `X=`/`Y=`).
    pub cell_x_offset: u32,
    pub cell_y_offset: u32,
    /// Resolved source rectangle in image pixels (kitty `x=`/`y=`/`w=`/`h=`).
    pub source_x: u32,
    pub source_y: u32,
    pub source_width: u32,
    pub source_height: u32,
    pub z: i32,
}

/// Geometry of one non-virtual kitty placement in absolute rows.
/// For frozen blocks ([`GhosttyTerminal::block_placements`]) the rows are
/// block-relative — the same row space `BlockRef::read_row_visit` reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlacementScreenPos {
    pub image_id: u32,
    pub placement_id: u32,
    /// Absolute SCREEN column/row of the placement's top-left pin.
    pub screen_col: u32,
    pub screen_row: u32,
    pub grid_cols: u32,
    pub grid_rows: u32,
    pub source_x: u32,
    pub source_y: u32,
    pub source_width: u32,
    pub source_height: u32,
    pub z: i32,
}

/// Engine scrollbar geometry (a terminal-side `Eq` mirror of the FFI
/// `TerminalScrollbar`): `total` scrollable rows, `offset` of the viewport top in
/// that area (top-anchored, `0..total-len`), `len` visible rows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScrollbarInfo {
    pub total: u64,
    pub offset: u64,
    pub len: u64,
}

/// VT mode identifiers for [`GhosttyTerminal::mode`].
///
/// Values mirror Ghostty's `ModeTag` (packed `u16`): a DEC private mode uses its
/// raw number; an ANSI mode sets bit 15. See Ghostty `src/terminal/modes.zig`.
pub mod mode {
    /// DECCKM — application cursor keys.
    pub const CURSOR_KEYS: u16 = 1;
    /// IRM — insert/replace (ANSI mode 4).
    pub const INSERT: u16 = 4 | 0x8000;
    /// DECAWM — autowrap / line wrap.
    pub const WRAPAROUND: u16 = 7;
    /// DECTCEM — cursor visible.
    pub const CURSOR_VISIBLE: u16 = 25;
    /// DECKPAM — application keypad.
    pub const KEYPAD_KEYS: u16 = 66;
    pub const MOUSE_NORMAL: u16 = 1000;
    pub const MOUSE_BUTTON: u16 = 1002;
    pub const MOUSE_ANY: u16 = 1003;
    pub const FOCUS_EVENT: u16 = 1004;
    pub const MOUSE_UTF8: u16 = 1005;
    pub const MOUSE_SGR: u16 = 1006;
    pub const MOUSE_ALTERNATE_SCROLL: u16 = 1007;
    pub const MOUSE_URXVT: u16 = 1015;
    pub const MOUSE_SGR_PIXELS: u16 = 1016;
    pub const ALT_SCREEN: u16 = 1049;
    pub const BRACKETED_PASTE: u16 = 2004;
    /// DEC synchronized output keeps a TUI frame private until its matching reset.
    pub const SYNC_OUTPUT: u16 = 2026;
}

/// State the terminal's synchronous callbacks write into during `write_vt`.
/// Owned behind a `Box` so its address is stable for the FFI userdata pointer.
#[derive(Default)]
struct Callbacks {
    /// Bytes the terminal wants written back to the PTY (DSR/DA/etc.).
    pty_writes: Vec<u8>,
    /// Number of BEL characters received since last drained.
    bell_count: u32,
    /// Owned text copied from clipboard requests before the FFI callback returns.
    clipboard_writes: Vec<(crate::clipboard::ClipboardType, String)>,
}

unsafe extern "C" fn write_pty_cb(
    _terminal: vt::Terminal,
    userdata: *mut std::os::raw::c_void,
    data: *const u8,
    len: usize,
) {
    if userdata.is_null() || data.is_null() || len == 0 {
        return;
    }
    let cb = unsafe { &mut *(userdata as *mut Callbacks) };
    cb.pty_writes
        .extend_from_slice(unsafe { std::slice::from_raw_parts(data, len) });
}

unsafe extern "C" fn bell_cb(_terminal: vt::Terminal, userdata: *mut std::os::raw::c_void) {
    if userdata.is_null() {
        return;
    }
    let cb = unsafe { &mut *(userdata as *mut Callbacks) };
    cb.bell_count = cb.bell_count.saturating_add(1);
}

unsafe fn vt_string_bytes(value: &vt::String) -> Option<&[u8]> {
    if value.len == 0 {
        return Some(&[]);
    }
    if value.ptr.is_null() {
        return None;
    }
    Some(unsafe { std::slice::from_raw_parts(value.ptr, value.len) })
}

unsafe extern "C" fn clipboard_write_cb(
    _terminal: vt::Terminal,
    userdata: *mut std::os::raw::c_void,
    write: *const vt::ClipboardWrite,
) -> vt::ClipboardWriteResult::Type {
    use crate::clipboard::ClipboardType;

    if userdata.is_null() || write.is_null() {
        return vt::ClipboardWriteResult::INVALID_DATA;
    }
    let size = unsafe { write.cast::<usize>().read() };
    if size < std::mem::size_of::<vt::ClipboardWrite>() {
        return vt::ClipboardWriteResult::INVALID_DATA;
    }
    let write = unsafe { &*write };
    let ty = match write.location {
        vt::ClipboardLocation::STANDARD => ClipboardType::Clipboard,
        vt::ClipboardLocation::SELECTION | vt::ClipboardLocation::PRIMARY => {
            ClipboardType::Selection
        }
        _ => return vt::ClipboardWriteResult::UNSUPPORTED,
    };
    let cb = unsafe { &mut *(userdata as *mut Callbacks) };
    if write.contents_len == 0 {
        cb.clipboard_writes.push((ty, String::new()));
        return vt::ClipboardWriteResult::SUCCESS;
    }
    if write.contents.is_null() {
        return vt::ClipboardWriteResult::INVALID_DATA;
    }
    let contents = unsafe { std::slice::from_raw_parts(write.contents, write.contents_len) };
    for content in contents {
        let Some(mime) = (unsafe { vt_string_bytes(&content.mime) }) else {
            return vt::ClipboardWriteResult::INVALID_DATA;
        };
        if mime != b"text/plain" && !mime.starts_with(b"text/plain;") {
            continue;
        }
        let Some(data) = (unsafe { vt_string_bytes(&content.data) }) else {
            return vt::ClipboardWriteResult::INVALID_DATA;
        };
        let Ok(text) = std::str::from_utf8(data) else {
            return vt::ClipboardWriteResult::INVALID_DATA;
        };
        cb.clipboard_writes.push((ty, text.to_owned()));
        return vt::ClipboardWriteResult::SUCCESS;
    }
    vt::ClipboardWriteResult::UNSUPPORTED
}

/// PNG decode hook for the engine's kitty graphics protocol. The
/// `.lib` artifact ships no PNG decoder, so without this `f=100` transmissions are
/// rejected. Decodes via `image_rs` to RGBA and returns the buffer allocated with
/// the engine's own allocator (so the engine frees it).
unsafe extern "C" fn decode_png_cb(
    _userdata: *mut std::os::raw::c_void,
    allocator: *const vt::Allocator,
    data: *const u8,
    data_len: usize,
    out: *mut vt::SysImage,
) -> bool {
    if data.is_null() || out.is_null() {
        return false;
    }
    let bytes = unsafe { std::slice::from_raw_parts(data, data_len) };
    let img = match image_rs::load_from_memory(bytes) {
        Ok(img) => img.to_rgba8(),
        Err(_) => return false,
    };
    let (w, h) = (img.width(), img.height());
    let rgba = img.into_raw();
    let buf = unsafe { vt::ghostty_alloc(allocator, rgba.len()) };
    if buf.is_null() {
        return false;
    }
    unsafe {
        std::ptr::copy_nonoverlapping(rgba.as_ptr(), buf, rgba.len());
        (*out).width = w;
        (*out).height = h;
        (*out).data = buf;
        (*out).data_len = rgba.len();
    }
    true
}

/// Register the process-global PNG decode hook once.
fn register_png_decoder() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| unsafe {
        vt::ghostty_sys_set(
            vt::SysOption::GHOSTTY_SYS_OPT_DECODE_PNG,
            decode_png_cb as *const std::os::raw::c_void,
        );
    });
}

/// Kitty image storage limit. The `.lib` default is 10 MB — small
/// enough to evict real images; 64 MB holds typical multi-image use with a bounded
/// resident footprint (~2–3× at saturation). Future `graphics` config knob.
const KITTY_IMAGE_STORAGE_LIMIT_BYTES: u64 = 64 * 1024 * 1024;

pub struct GhosttyTerminal {
    terminal: vt::Terminal,
    render_state: vt::RenderState,
    row_iter: vt::RenderStateRowIterator,
    /// Reused each `snapshot()` to walk kitty placements. Allocated once;
    /// `ghostty_kitty_graphics_get(PLACEMENT_ITERATOR)` re-points it at the live
    /// storage with no allocation, so a no-graphics batch costs ~3 FFI calls.
    placement_iter: vt::KittyGraphicsPlacementIterator,
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
    shipped_images: rustc_hash::FxHashMap<u32, (u32, u32, usize)>,
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
        let options = vt::TerminalOptions {
            cols,
            rows,
            max_scrollback,
        };
        Error::from_code(unsafe { vt::ghostty_terminal_new(ptr::null(), &mut terminal, options) })?;

        let mut render_state = ptr::null_mut();
        if let Err(err) = Error::from_code(unsafe {
            vt::ghostty_render_state_new(ptr::null(), &mut render_state)
        }) {
            unsafe { vt::ghostty_terminal_free(terminal) };
            return Err(err);
        }

        let mut row_iter = ptr::null_mut();
        if let Err(err) = Error::from_code(unsafe {
            vt::ghostty_render_state_row_iterator_new(ptr::null(), &mut row_iter)
        }) {
            unsafe {
                vt::ghostty_render_state_free(render_state);
                vt::ghostty_terminal_free(terminal);
            }
            return Err(err);
        }

        let mut placement_iter = ptr::null_mut();
        if let Err(err) = Error::from_code(unsafe {
            vt::ghostty_kitty_graphics_placement_iterator_new(ptr::null(), &mut placement_iter)
        }) {
            unsafe {
                vt::ghostty_render_state_row_iterator_free(row_iter);
                vt::ghostty_render_state_free(render_state);
                vt::ghostty_terminal_free(terminal);
            }
            return Err(err);
        }

        // Register the process-global PNG decoder once for Kitty `f=100` payloads.
        register_png_decoder();

        // Raise the kitty image storage limit from the conservative 10 MB `.lib`
        // default; a non-zero limit also enables the protocol.
        let limit = KITTY_IMAGE_STORAGE_LIMIT_BYTES;
        unsafe {
            vt::ghostty_terminal_set(
                terminal,
                vt::TerminalOption::KITTY_IMAGE_STORAGE_LIMIT,
                (&limit as *const u64).cast(),
            );
        }

        // Register synchronous callbacks. Userdata points at the boxed
        // `Callbacks`; its heap address is stable across moves of `Self`.
        let mut callbacks = Box::new(Callbacks::default());
        let userdata = &mut *callbacks as *mut Callbacks as *mut std::os::raw::c_void;
        unsafe {
            vt::ghostty_terminal_set(terminal, vt::TerminalOption::USERDATA, userdata);
            vt::ghostty_terminal_set(
                terminal,
                vt::TerminalOption::WRITE_PTY,
                write_pty_cb as *const std::os::raw::c_void,
            );
            vt::ghostty_terminal_set(
                terminal,
                vt::TerminalOption::BELL,
                bell_cb as *const std::os::raw::c_void,
            );
            vt::ghostty_terminal_set(
                terminal,
                vt::TerminalOption::CLIPBOARD_WRITE,
                clipboard_write_cb as *const std::os::raw::c_void,
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
            vt::ghostty_terminal_vt_write(terminal, seq.as_ptr(), seq.len());
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
            shipped_images: rustc_hash::FxHashMap::default(),
            scrollbar_override: None,
        })
    }

    /// Drain bytes the terminal wants written back to the PTY (query/DSR/DA
    /// responses). Returns empty when there is nothing to send.
    pub fn take_pty_writes(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.callbacks.pty_writes)
    }

    /// Drain and reset the bell counter (number of BELs since last call).
    pub fn take_bell(&mut self) -> u32 {
        std::mem::replace(&mut self.callbacks.bell_count, 0)
    }

    /// Drain clipboard writes decoded from OSC 52 or iTerm2 OSC 1337.
    pub fn take_clipboard_writes(&mut self) -> Vec<(crate::clipboard::ClipboardType, String)> {
        std::mem::take(&mut self.callbacks.clipboard_writes)
    }

    /// Poll the terminal title; returns `Some(title)` only when it changed
    /// since the last poll.
    pub fn poll_title(&mut self) -> Option<String> {
        let title = self.get_string(vt::TerminalData::TITLE);
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
        let pwd = self.get_string(vt::TerminalData::PWD);
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
        self.get_string(vt::TerminalData::TITLE)
    }

    /// The current OSC 7 working directory (peek) as a path, or `None` when unset.
    /// Replaces the mirror's `current_directory` for the title template.
    pub fn current_directory(&self) -> Option<std::path::PathBuf> {
        let pwd = self.get_string(vt::TerminalData::PWD);
        if pwd.is_empty() {
            None
        } else {
            Some(crate::pty_pipe::pwd_to_path(&pwd))
        }
    }

    /// Set the working directory directly. OSC 7 populates the same engine state;
    /// this direct setter is
    /// kept for tests and programmatic cwd updates.
    pub fn set_pwd(&mut self, pwd: &str) {
        let s = vt::String {
            ptr: pwd.as_ptr(),
            len: pwd.len(),
        };
        unsafe {
            vt::ghostty_terminal_set(
                self.terminal,
                vt::TerminalOption::PWD,
                (&s as *const vt::String).cast(),
            );
        }
    }

    /// Read a `GhosttyString`-typed terminal datum as an owned `String`. The
    /// borrowed pointer is only valid until the next mutating call, so we copy
    /// immediately.
    fn get_string(&self, data: vt::TerminalData::Type) -> String {
        let mut s = vt::String {
            ptr: ptr::null(),
            len: 0,
        };
        let ok = unsafe {
            vt::ghostty_terminal_get(self.terminal, data, (&mut s as *mut vt::String).cast())
        };
        if ok != vt::Result::SUCCESS || s.ptr.is_null() || s.len == 0 {
            return String::new();
        }
        let bytes = unsafe { std::slice::from_raw_parts(s.ptr, s.len) };
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
            vt::ghostty_terminal_get(
                self.terminal,
                vt::TerminalData::CURSOR_Y,
                (&mut out as *mut u16).cast(),
            )
        };
        (ok == vt::Result::SUCCESS).then_some(out)
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
        let mut sb = vt::TerminalScrollbar::default();
        unsafe {
            vt::ghostty_terminal_get(
                self.terminal,
                vt::TerminalData::SCROLLBAR,
                (&mut sb as *mut vt::TerminalScrollbar).cast(),
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
        unsafe { vt::ghostty_terminal_vt_write(self.terminal, data.as_ptr(), data.len()) };
        if self.scrollbar_override.is_some() {
            self.update_scrollbar_override();
        }
    }

    /// Set the Kitty-image storage limit in bytes; `new()` applies the default. A non-zero limit
    /// also enables the protocol; 0 disables it. Exposed for tests/eviction.
    pub fn set_kitty_storage_limit(&mut self, bytes: u64) {
        unsafe {
            vt::ghostty_terminal_set(
                self.terminal,
                vt::TerminalOption::KITTY_IMAGE_STORAGE_LIMIT,
                (&bytes as *const u64).cast(),
            );
        }
    }

    /// Whether the engine currently holds a kitty image with this id —
    /// cheap id lookup, no pixel copy. Used to observe transmit/delete/eviction.
    pub fn kitty_image_exists(&self, image_id: u32) -> bool {
        let mut graphics: vt::KittyGraphics = ptr::null_mut();
        let have = unsafe {
            vt::ghostty_terminal_get(
                self.terminal,
                vt::TerminalData::KITTY_GRAPHICS,
                (&mut graphics as *mut vt::KittyGraphics).cast(),
            )
        } == vt::Result::SUCCESS
            && !graphics.is_null();
        have && !unsafe { vt::ghostty_kitty_graphics_image(graphics, image_id) }.is_null()
    }

    /// Read the current value of a VT mode (identifiers in [`mode`]). Returns
    /// `false` for unknown/unset modes.
    pub fn mode(&self, id: u16) -> bool {
        let mut value = false;
        let ok =
            unsafe { vt::ghostty_terminal_mode_get(self.terminal, id, &mut value as *mut bool) };
        ok == vt::Result::SUCCESS && value
    }

    /// The active kitty keyboard protocol flags, mapped to terminal `Mode` bits. These
    /// live in the engine's kitty-keyboard flag stack, NOT the DEC private modes, so
    /// `mode()` can't read them — the vt_modes facade folds these in separately so
    /// `session_key_flags` / the input path see kitty press+release encoding
    /// for key press and release encoding. Empty when the protocol is inactive.
    pub fn kitty_keyboard_modes(&self) -> crate::terminal::Mode {
        use crate::terminal::Mode;
        let mut flags: u8 = 0;
        let ok = unsafe {
            vt::ghostty_terminal_get(
                self.terminal,
                vt::TerminalData::KITTY_KEYBOARD_FLAGS,
                (&mut flags as *mut u8).cast(),
            )
        };
        if ok != vt::Result::SUCCESS {
            return Mode::empty();
        }
        let mut m = Mode::empty();
        m.set(
            Mode::DISAMBIGUATE_ESC_CODES,
            flags & vt::KITTY_KEY_DISAMBIGUATE != 0,
        );
        m.set(
            Mode::REPORT_EVENT_TYPES,
            flags & vt::KITTY_KEY_REPORT_EVENTS != 0,
        );
        m.set(
            Mode::REPORT_ALTERNATE_KEYS,
            flags & vt::KITTY_KEY_REPORT_ALTERNATES != 0,
        );
        m.set(
            Mode::REPORT_ALL_KEYS_AS_ESC,
            flags & vt::KITTY_KEY_REPORT_ALL != 0,
        );
        m.set(
            Mode::REPORT_ASSOCIATED_TEXT,
            flags & vt::KITTY_KEY_REPORT_ASSOCIATED != 0,
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
                vt::ghostty_terminal_resize(
                    self.terminal,
                    cols,
                    self.rows,
                    cell_width_px,
                    cell_height_px,
                )
            })?;
        }
        Error::from_code(unsafe {
            vt::ghostty_terminal_resize(self.terminal, cols, rows, cell_width_px, cell_height_px)
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
        let rgb = |c: [u8; 3]| vt::ColorRgb {
            r: c[0],
            g: c[1],
            b: c[2],
        };
        let f = rgb(fg);
        let b = rgb(bg);
        let c = rgb(cursor);
        let pal: [vt::ColorRgb; 256] = std::array::from_fn(|i| rgb(palette[i]));
        unsafe {
            vt::ghostty_terminal_set(
                self.terminal,
                vt::TerminalOption::COLOR_FOREGROUND,
                (&f as *const vt::ColorRgb).cast(),
            );
            vt::ghostty_terminal_set(
                self.terminal,
                vt::TerminalOption::COLOR_BACKGROUND,
                (&b as *const vt::ColorRgb).cast(),
            );
            vt::ghostty_terminal_set(
                self.terminal,
                vt::TerminalOption::COLOR_CURSOR,
                (&c as *const vt::ColorRgb).cast(),
            );
            vt::ghostty_terminal_set(
                self.terminal,
                vt::TerminalOption::COLOR_PALETTE,
                pal.as_ptr().cast(),
            );
        }
    }

    pub fn set_theme_colors(&mut self, colors: &nmt_config::colors::Colors) {
        use nmt_config::colors::term::List;
        use nmt_config::colors::{ColorRgb, NamedColor};

        let list = List::from(colors);
        let to_rgb = |color| {
            let color = ColorRgb::from_color_arr(color);
            [color.r, color.g, color.b]
        };
        let palette = std::array::from_fn(|index| to_rgb(list[index]));
        self.set_colors(
            to_rgb(list[NamedColor::Foreground]),
            to_rgb(list[NamedColor::Background]),
            to_rgb(list[NamedColor::Cursor]),
            &palette,
        );
    }

    /// Probe: whether any visible row carries a PROMPT semantic tag (command-blocks-
    /// rendering — mark-forwarding regression checks in pty_pipe tests).
    #[cfg(test)]
    pub(crate) fn has_prompt_tagged_row(&mut self) -> bool {
        self.row_semantic_prompts()
            .map(|tags| tags.iter().any(|&t| t == vt::RowSemanticPrompt::PROMPT))
            .unwrap_or(false)
    }

    /// Probe the engine's `SEMANTIC_PROMPT` tag per visible row. This verifies that
    /// headless parsing preserves OSC 133 metadata used by command-block rendering.
    #[cfg(test)]
    fn row_semantic_prompts(&mut self) -> Result<Vec<vt::RowSemanticPrompt::Type>> {
        Error::from_code(unsafe {
            vt::ghostty_render_state_update(self.render_state, self.terminal)
        })?;
        Error::from_code(unsafe {
            vt::ghostty_render_state_get(
                self.render_state,
                vt::RenderStateData::ROW_ITERATOR,
                (&mut self.row_iter as *mut vt::RenderStateRowIterator).cast(),
            )
        })?;
        let mut out = Vec::with_capacity(self.rows as usize);
        while unsafe { vt::ghostty_render_state_row_iterator_next(self.row_iter) } {
            let mut tag: vt::RowSemanticPrompt::Type = vt::RowSemanticPrompt::NONE;
            let mut raw_row: vt::Row = 0;
            if unsafe {
                vt::ghostty_render_state_row_get(
                    self.row_iter,
                    vt::RenderStateRowData::RAW,
                    (&mut raw_row as *mut vt::Row).cast(),
                )
            } == vt::Result::SUCCESS
            {
                let _ = unsafe {
                    vt::ghostty_row_get(
                        raw_row,
                        vt::RowData::SEMANTIC_PROMPT,
                        (&mut tag as *mut vt::RowSemanticPrompt::Type).cast(),
                    )
                };
            }
            out.push(tag);
        }
        Ok(out)
    }

    /// Populate a reusable render buffer from the full visible viewport.
    pub fn snapshot_into(&mut self, buffer: &mut RenderBuffer) -> Result<()> {
        Error::from_code(unsafe {
            vt::ghostty_render_state_update(self.render_state, self.terminal)
        })?;
        self.consume_render_damage()?;

        let cursor = self.cursor().unwrap_or(SnapshotCursor {
            x: 0,
            y: 0,
            visible: false,
            shape: crate::ansi::CursorShape::Block,
            blinking: false,
        });
        let palette = self.color_palette();
        buffer.begin_capture(self.cols as usize, self.rows as usize);
        // A transient row lookup failure blanks only that row; publishing the
        // remaining viewport is safer than withholding an otherwise valid frame.
        for y in 0..self.rows {
            let meta = self
                .grid_ref_at(vt::PointTag::VIEWPORT, 0, y as u32)
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
        let mut dirty: vt::RenderStateDirty::Type = vt::RenderStateDirty::FALSE;
        Error::from_code(unsafe {
            vt::ghostty_render_state_get(
                self.render_state,
                vt::RenderStateData::DIRTY,
                (&mut dirty as *mut vt::RenderStateDirty::Type).cast(),
            )
        })?;

        let rows = self.rows as usize;
        let dimensions_changed = self.row_versions.len() != rows;
        if dirty == vt::RenderStateDirty::FALSE && !dimensions_changed {
            return Ok(());
        }

        self.content_revision = self.content_revision.wrapping_add(1);
        let revision = self.content_revision;
        self.row_versions.resize(rows, revision);
        let full = dimensions_changed || dirty != vt::RenderStateDirty::PARTIAL;
        if full {
            self.row_versions.fill(revision);
        }

        Error::from_code(unsafe {
            vt::ghostty_render_state_get(
                self.render_state,
                vt::RenderStateData::ROW_ITERATOR,
                (&mut self.row_iter as *mut vt::RenderStateRowIterator).cast(),
            )
        })?;

        let clean = false;
        let mut row = 0usize;
        while unsafe { vt::ghostty_render_state_row_iterator_next(self.row_iter) } {
            let mut row_dirty = false;
            Error::from_code(unsafe {
                vt::ghostty_render_state_row_get(
                    self.row_iter,
                    vt::RenderStateRowData::DIRTY,
                    (&mut row_dirty as *mut bool).cast(),
                )
            })?;
            if !full && row_dirty {
                if let Some(version) = self.row_versions.get_mut(row) {
                    *version = revision;
                }
            }
            Error::from_code(unsafe {
                vt::ghostty_render_state_row_set(
                    self.row_iter,
                    vt::RenderStateRowOption::DIRTY,
                    (&clean as *const bool).cast(),
                )
            })?;
            row += 1;
        }

        let clean = vt::RenderStateDirty::FALSE;
        Error::from_code(unsafe {
            vt::ghostty_render_state_set(
                self.render_state,
                vt::RenderStateOption::DIRTY,
                (&clean as *const vt::RenderStateDirty::Type).cast(),
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
        let mut graphics: vt::KittyGraphics = ptr::null_mut();
        if unsafe {
            vt::ghostty_terminal_get(
                self.terminal,
                vt::TerminalData::KITTY_GRAPHICS,
                (&mut graphics as *mut vt::KittyGraphics).cast(),
            )
        } != vt::Result::SUCCESS
            || graphics.is_null()
        {
            return out;
        }

        // Re-point the persistent iterator at the live placement set (no alloc).
        if unsafe {
            vt::ghostty_kitty_graphics_get(
                graphics,
                vt::KittyGraphicsData::PLACEMENT_ITERATOR,
                (&mut self.placement_iter as *mut vt::KittyGraphicsPlacementIterator).cast(),
            )
        } != vt::Result::SUCCESS
        {
            return out;
        }

        while unsafe { vt::ghostty_kitty_graphics_placement_next(self.placement_iter) } {
            let iter = self.placement_iter;
            let image_id = placement_u32(iter, vt::KittyGraphicsPlacementData::IMAGE_ID);
            let placement_id = placement_u32(iter, vt::KittyGraphicsPlacementData::PLACEMENT_ID);
            let mut is_virtual = false;
            unsafe {
                vt::ghostty_kitty_graphics_placement_get(
                    iter,
                    vt::KittyGraphicsPlacementData::IS_VIRTUAL,
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
                    grid_cols: placement_u32(iter, vt::KittyGraphicsPlacementData::COLUMNS),
                    grid_rows: placement_u32(iter, vt::KittyGraphicsPlacementData::ROWS),
                    cell_x_offset: 0,
                    cell_y_offset: 0,
                    source_x: 0,
                    source_y: 0,
                    source_width: 0,
                    source_height: 0,
                    z: placement_i32(iter, vt::KittyGraphicsPlacementData::Z),
                });
                continue;
            }

            // Geometry needs the image handle.
            let image = unsafe { vt::ghostty_kitty_graphics_image(graphics, image_id) };
            if image.is_null() {
                continue;
            }

            let (mut vp_col, mut vp_row) = (0i32, 0i32);
            if unsafe {
                vt::ghostty_kitty_graphics_placement_viewport_pos(
                    iter,
                    image,
                    self.terminal,
                    &mut vp_col,
                    &mut vp_row,
                )
            } != vt::Result::SUCCESS
            {
                // Off-screen (NO_VALUE) — invisible this frame, nothing to paint.
                continue;
            }

            let (mut px_w, mut px_h) = (0u32, 0u32);
            unsafe {
                vt::ghostty_kitty_graphics_placement_pixel_size(
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
                cell_x_offset: placement_u32(iter, vt::KittyGraphicsPlacementData::X_OFFSET),
                cell_y_offset: placement_u32(iter, vt::KittyGraphicsPlacementData::Y_OFFSET),
                source_x: sx,
                source_y: sy,
                source_width: sw,
                source_height: sh,
                z: placement_i32(iter, vt::KittyGraphicsPlacementData::Z),
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
            vt::ghostty_kitty_graphics_get(
                graphics,
                vt::KittyGraphicsData::PLACEMENT_ITERATOR,
                (&mut self.placement_iter as *mut vt::KittyGraphicsPlacementIterator).cast(),
            )
        } != vt::Result::SUCCESS
        {
            return out;
        }

        while unsafe { vt::ghostty_kitty_graphics_placement_next(self.placement_iter) } {
            let iter = self.placement_iter;
            let (mut col, mut row) = (0u32, 0u32);
            if unsafe { vt::ghostty_block_ref_placement_pos(block.raw, iter, &mut col, &mut row) }
                != vt::Result::SUCCESS
            {
                // Virtual placement (unicode placeholder) — no pin to resolve.
                continue;
            }

            let image_id = placement_u32(iter, vt::KittyGraphicsPlacementData::IMAGE_ID);
            let image = unsafe { vt::ghostty_kitty_graphics_image(graphics, image_id) };
            if image.is_null() {
                continue;
            }
            let (g_cols, g_rows, [sx, sy, sw, sh]) = placement_geometry(iter, image, self.terminal);

            out.push(PlacementScreenPos {
                image_id,
                placement_id: placement_u32(iter, vt::KittyGraphicsPlacementData::PLACEMENT_ID),
                screen_col: col,
                screen_row: row,
                grid_cols: g_cols,
                grid_rows: g_rows,
                source_x: sx,
                source_y: sy,
                source_width: sw,
                source_height: sh,
                z: placement_i32(iter, vt::KittyGraphicsPlacementData::Z),
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
    ) -> Option<crate::graphics::GraphicData> {
        let graphics = block.kitty_graphics_raw()?;
        let image = unsafe { vt::ghostty_kitty_graphics_image(graphics, image_id) };
        if image.is_null() {
            return None;
        }
        let read_u32 = |data: vt::KittyGraphicsImageData::Type| -> u32 {
            let mut v: u32 = 0;
            unsafe {
                vt::ghostty_kitty_graphics_image_get(image, data, (&mut v as *mut u32).cast());
            }
            v
        };
        let width = read_u32(vt::KittyGraphicsImageData::WIDTH);
        let height = read_u32(vt::KittyGraphicsImageData::HEIGHT);
        let mut data_len: usize = 0;
        unsafe {
            vt::ghostty_kitty_graphics_image_get(
                image,
                vt::KittyGraphicsImageData::DATA_LEN,
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
    ) -> (Vec<(u32, crate::graphics::GraphicData)>, Vec<u32>) {
        let mut pending = Vec::new();
        let mut live: rustc_hash::FxHashSet<u32> = rustc_hash::FxHashSet::default();

        let mut graphics: vt::KittyGraphics = ptr::null_mut();
        let have_graphics = unsafe {
            vt::ghostty_terminal_get(
                self.terminal,
                vt::TerminalData::KITTY_GRAPHICS,
                (&mut graphics as *mut vt::KittyGraphics).cast(),
            )
        } == vt::Result::SUCCESS
            && !graphics.is_null();

        if have_graphics {
            for p in placements {
                if !live.insert(p.image_id) {
                    continue;
                }
                let image = unsafe { vt::ghostty_kitty_graphics_image(graphics, p.image_id) };
                if image.is_null() {
                    continue;
                }

                let read_u32 = |data: vt::KittyGraphicsImageData::Type| -> u32 {
                    let mut v: u32 = 0;
                    unsafe {
                        vt::ghostty_kitty_graphics_image_get(
                            image,
                            data,
                            (&mut v as *mut u32).cast(),
                        );
                    }
                    v
                };
                let width = read_u32(vt::KittyGraphicsImageData::WIDTH);
                let height = read_u32(vt::KittyGraphicsImageData::HEIGHT);
                let mut data_len: usize = 0;
                unsafe {
                    vt::ghostty_kitty_graphics_image_get(
                        image,
                        vt::KittyGraphicsImageData::DATA_LEN,
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
                !have_graphics
                    || unsafe { vt::ghostty_kitty_graphics_image(graphics, *id) }.is_null()
            })
            .collect();
        for id in &removed {
            self.shipped_images.remove(id);
        }

        (pending, removed)
    }

    /// Resolve a point (in the given coordinate system) to a `GridRef`. Fast for
    /// `VIEWPORT`/`ACTIVE`; **O(scrollback) for `SCREEN`/`HISTORY`**. The ref is
    /// valid only until the next mutating call (`write_vt`/`resize`/
    /// `scroll_viewport`) — use it within one read pass, never cache it.
    pub fn grid_ref_at(&self, tag: vt::PointTag::Type, x: u16, y: u32) -> Result<vt::GridRef> {
        let point = vt::Point {
            tag,
            value: vt::PointValue {
                coordinate: vt::PointCoordinate { x, y },
            },
        };
        let mut grid_ref = vt::GridRef::default();
        Error::from_code(unsafe {
            vt::ghostty_terminal_grid_ref(self.terminal, point, &mut grid_ref)
        })?;
        Ok(grid_ref)
    }

    /// Resolve a viewport coordinate to a `GridRef` (fast).
    pub fn viewport_grid_ref(&self, x: u16, y: u16) -> Result<vt::GridRef> {
        self.grid_ref_at(vt::PointTag::VIEWPORT, x, y as u32)
    }

    /// The SCREEN row of the top visible row (`viewport_top`) — the constant that
    /// maps between SCREEN and visible coordinates (`screen_row = viewport_top +
    /// visible_row`). One cheap viewport `grid_ref`; `None` if the viewport is
    /// empty. Selection rendering uses this to translate coordinate spaces.
    pub fn viewport_top_screen(&self) -> Option<u32> {
        let r = self.viewport_grid_ref(0, 0).ok()?;
        self.point_from_grid_ref(&r, vt::PointTag::SCREEN)
            .ok()
            .flatten()
            .map(|(_, y)| y)
    }

    /// Read one absolute `SCREEN` row into a materialized `Vec` — test-only
    /// convenience over [`Self::read_screen_row_visit`].
    #[cfg(test)]
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
        palette: &[vt::ColorRgb; 256],
        on_cell: impl FnMut(u16, CellText, CellWide, SnapshotStyle),
    ) -> Result<Option<ScreenRowMeta>> {
        let grid_ref = match self.grid_ref_at(vt::PointTag::SCREEN, 0, row) {
            Ok(r) => r,
            Err(Error::InvalidValue) => return Ok(None),
            Err(e) => return Err(e),
        };
        Ok(Some(Self::visit_row_cells(
            grid_ref, self.cols, palette, on_cell,
        )?))
    }

    /// Shared per-row cell walk over a resolved row `GridRef` — the body of
    /// [`Self::read_screen_row_visit`], also used by finished-block reads
    /// ([`Self::read_block_row_visit`]) where the ref comes from the block
    /// resolver instead of the active screen.
    fn visit_row_cells(
        mut grid_ref: vt::GridRef,
        cols: u16,
        palette: &[vt::ColorRgb; 256],
        mut on_cell: impl FnMut(u16, CellText, CellWide, SnapshotStyle),
    ) -> Result<ScreenRowMeta> {
        // Row flags from the raw row handle (same keys the snapshot path
        // reads), fetched in one multi-get FFI call.
        let mut raw_row: vt::Row = 0;
        Error::from_code(unsafe { vt::ghostty_grid_ref_row(&grid_ref, &mut raw_row) })?;
        let mut wrapped = false;
        let mut prompt_tag: vt::RowSemanticPrompt::Type = vt::RowSemanticPrompt::NONE;
        let mut has_link = false;
        let mut virtual_placeholder = false;
        {
            const ROW_KEYS: [vt::RowData::Type; 4] = [
                vt::RowData::WRAP,
                vt::RowData::SEMANTIC_PROMPT,
                vt::RowData::HYPERLINK,
                vt::RowData::KITTY_VIRTUAL_PLACEHOLDER,
            ];
            let mut values: [*mut std::os::raw::c_void; 4] = [
                (&mut wrapped as *mut bool).cast(),
                (&mut prompt_tag as *mut vt::RowSemanticPrompt::Type).cast(),
                (&mut has_link as *mut bool).cast(),
                (&mut virtual_placeholder as *mut bool).cast(),
            ];
            unsafe {
                let _ = vt::ghostty_row_get_multi(
                    raw_row,
                    ROW_KEYS.len(),
                    ROW_KEYS.as_ptr(),
                    values.as_mut_ptr(),
                    std::ptr::null_mut(),
                );
            }
        }

        let mut hyperlinks: Vec<(u16, u16, String)> = Vec::new();
        for x in 0..cols {
            // All cells of one row share the pin's node; stepping `x` in place
            // avoids re-resolving the O(scrollback) SCREEN pin per cell.
            grid_ref.x = x;

            let mut raw: vt::Cell = 0;
            if unsafe { vt::ghostty_grid_ref_cell(&grid_ref, &mut raw) } != vt::Result::SUCCESS {
                continue;
            }

            // Tag-driven per-cell reads on the raw cell handle, fetched in one
            // multi-get FFI call: the common cases (blank, single codepoint)
            // never call the grapheme reader. CODEPOINT is deliberately last —
            // multi-get stops at the first error, and a bg-color-only cell that
            // rejected it would still have tag/wide/styling written while `cp`
            // keeps its correct 0 default.
            let mut tag: vt::CellContentTag::Type = vt::CellContentTag::CODEPOINT;
            let mut wide_raw: vt::CellWide::Type = vt::CellWide::NARROW;
            let mut has_styling = false;
            let mut cp: u32 = 0;
            {
                const CELL_KEYS: [vt::CellData::Type; 4] = [
                    vt::CellData::CONTENT_TAG,
                    vt::CellData::WIDE,
                    vt::CellData::HAS_STYLING,
                    vt::CellData::CODEPOINT,
                ];
                let mut values: [*mut std::os::raw::c_void; 4] = [
                    (&mut tag as *mut i32).cast(),
                    (&mut wide_raw as *mut i32).cast(),
                    (&mut has_styling as *mut bool).cast(),
                    (&mut cp as *mut u32).cast(),
                ];
                unsafe {
                    let _ = vt::ghostty_cell_get_multi(
                        raw,
                        CELL_KEYS.len(),
                        CELL_KEYS.as_ptr(),
                        values.as_mut_ptr(),
                        std::ptr::null_mut(),
                    );
                }
            }
            let wide = CellWide::from(wide_raw);

            let text = match tag {
                vt::CellContentTag::CODEPOINT => {
                    if cp == 0 {
                        CellText::default()
                    } else {
                        CellText::from_char(
                            char::from_u32(cp).unwrap_or(char::REPLACEMENT_CHARACTER),
                        )
                    }
                }
                vt::CellContentTag::CODEPOINT_GRAPHEME => {
                    CellText::from(grid_ref_graphemes(&grid_ref))
                }
                _ => CellText::default(), // BG_COLOR_*: no text
            };

            // The style struct read + resolve only runs for cells the engine
            // flags as styled; default-styled text (the bulk of scroll floods)
            // skips it entirely.
            let mut style = SnapshotStyle::default();
            if has_styling {
                let mut raw_style = vt::sized!(vt::Style);
                if unsafe { vt::ghostty_grid_ref_style(&grid_ref, &mut raw_style) }
                    == vt::Result::SUCCESS
                {
                    style.fg = style_color_resolve(&raw_style.fg_color, palette);
                    style.bg = style_color_resolve(&raw_style.bg_color, palette);
                    style.underline_color =
                        style_color_resolve(&raw_style.underline_color, palette);
                    style.bold = raw_style.bold;
                    style.italic = raw_style.italic;
                    style.faint = raw_style.faint;
                    style.blink = raw_style.blink;
                    style.inverse = raw_style.inverse;
                    style.invisible = raw_style.invisible;
                    style.strikethrough = raw_style.strikethrough;
                    style.overline = raw_style.overline;
                    style.underline = Underline::from(raw_style.underline);
                }
            }

            // Erased-with-bg cells carry their color in the content tag, not the
            // style (mirrors the render-state BG_COLOR resolution).
            if style.bg.is_none() {
                if tag == vt::CellContentTag::BG_COLOR_PALETTE {
                    let mut idx: vt::ColorPaletteIndex = 0;
                    if unsafe {
                        vt::ghostty_cell_get(
                            raw,
                            vt::CellData::COLOR_PALETTE,
                            (&mut idx as *mut vt::ColorPaletteIndex).cast(),
                        )
                    } == vt::Result::SUCCESS
                    {
                        style.bg = Some(palette[idx as usize].into());
                    }
                } else if tag == vt::CellContentTag::BG_COLOR_RGB {
                    let mut rgb = vt::ColorRgb::default();
                    if unsafe {
                        vt::ghostty_cell_get(
                            raw,
                            vt::CellData::COLOR_RGB,
                            (&mut rgb as *mut vt::ColorRgb).cast(),
                        )
                    } == vt::Result::SUCCESS
                    {
                        style.bg = Some(rgb.into());
                    }
                }
            }

            if has_link {
                if let Some(uri) = grid_ref_hyperlink_uri(&grid_ref) {
                    match hyperlinks.last_mut() {
                        Some((_, end, last_uri)) if *end + 1 == x && *last_uri == uri => *end = x,
                        _ => hyperlinks.push((x, x, uri)),
                    }
                }
            }

            if text.is_empty() && style.bg.is_none() && wide == CellWide::Narrow {
                continue;
            }
            on_cell(x, text, wide, style);
        }

        Ok(ScreenRowMeta {
            wrapped,
            prompt_start: prompt_tag == vt::RowSemanticPrompt::PROMPT,
            virtual_placeholder,
            hyperlinks,
        })
    }

    /// Finish the current command block: freeze the primary screen into the
    /// engine's block set (O(1) ownership move) and continue on a fresh
    /// primary screen with writer state carried over. Returns `None` when
    /// the active screen has no content (no block created). Errors with
    /// `InvalidValue` if the alternate screen is active — callers gate on
    /// the primary screen because alternate-screen content should not enter history.
    pub fn finish_block(&mut self) -> Result<Option<vt::BlockHandle>> {
        let mut handle = vt::BlockHandle::default();
        match unsafe { vt::ghostty_terminal_finish_block(self.terminal, &mut handle) } {
            vt::Result::SUCCESS => Ok(Some(handle)),
            vt::Result::NO_VALUE => Ok(None),
            other => {
                Error::from_code(other)?;
                Ok(None)
            }
        }
    }

    /// Remove and destroy all finished blocks (user clear; `;K` path).
    pub fn clear_blocks(&mut self) {
        unsafe { vt::ghostty_terminal_clear_blocks(self.terminal) }
    }

    /// Remove and destroy one finished block. Returns `false` for a stale
    /// handle (already removed/evicted).
    pub fn remove_block(&mut self, handle: vt::BlockHandle) -> bool {
        (unsafe { vt::ghostty_terminal_remove_block(self.terminal, handle) }) == vt::Result::SUCCESS
    }

    pub fn block_count(&self) -> usize {
        unsafe { vt::ghostty_terminal_block_count(self.terminal) }
    }

    /// The handle of the finished block at `index`, oldest first.
    pub fn block_at(&self, index: usize) -> Option<vt::BlockHandle> {
        let mut handle = vt::BlockHandle::default();
        (unsafe { vt::ghostty_terminal_block_at(self.terminal, index, &mut handle) }
            == vt::Result::SUCCESS)
            .then_some(handle)
    }

    /// Logical row count of a finished block (trailing blanks after the
    /// finish-time cursor truncated). `None` for a stale handle.
    pub fn block_row_count(&self, handle: vt::BlockHandle) -> Option<usize> {
        let mut rows: usize = 0;
        (unsafe { vt::ghostty_terminal_block_row_count(self.terminal, handle, &mut rows) }
            == vt::Result::SUCCESS)
            .then_some(rows)
    }

    /// The column count the block was frozen at (can differ from the live
    /// terminal width after a resize). `None` for a stale handle.
    pub fn block_cols(&self, handle: vt::BlockHandle) -> Option<u16> {
        let mut cols: u16 = 0;
        (unsafe { vt::ghostty_terminal_block_cols(self.terminal, handle, &mut cols) }
            == vt::Result::SUCCESS)
            .then_some(cols)
    }

    /// The memory retained by a block's page storage in bytes (the input
    /// for enforcing the finished-block byte budget. `None` for a stale
    /// handle.
    pub fn block_bytes(&self, handle: vt::BlockHandle) -> Option<usize> {
        let mut bytes: usize = 0;
        (unsafe { vt::ghostty_terminal_block_bytes(self.terminal, handle, &mut bytes) }
            == vt::Result::SUCCESS)
            .then_some(bytes)
    }

    /// Total page-storage bytes of all finished blocks — the value the
    /// block byte budget is enforced against.
    pub fn blocks_bytes(&self) -> usize {
        unsafe { vt::ghostty_terminal_blocks_bytes(self.terminal) }
    }

    /// Set the finished-block byte budget. Oldest blocks are evicted
    /// immediately (and on every finish) while the total exceeds it; the
    /// newest block is never evicted. Zero means unlimited.
    pub fn set_block_budget_bytes(&mut self, bytes: usize) -> Result<()> {
        Error::from_code(unsafe {
            vt::ghostty_terminal_set(
                self.terminal,
                vt::TerminalOption::BLOCK_BUDGET_BYTES,
                (&bytes as *const usize).cast(),
            )
        })
    }

    /// Reflow one finished block to `cols` (the lazy-reflow driver;
    /// `resize` already reflows all blocks eagerly). Bumps the block's
    /// data generation — re-fetch via [`Self::block_at`]. Returns `false`
    /// for a stale handle.
    pub fn reflow_block(&mut self, handle: vt::BlockHandle, cols: u16) -> Result<bool> {
        match unsafe { vt::ghostty_terminal_reflow_block(self.terminal, handle, cols) } {
            vt::Result::SUCCESS => Ok(true),
            vt::Result::NO_VALUE => Ok(false),
            other => {
                Error::from_code(other)?;
                Ok(false)
            }
        }
    }

    /// Take a read reference on a finished block (engine-refcounted; any
    /// thread). `None` for a stale handle or while the engine is
    /// reflowing the block — retry next frame. The reference pins an
    /// immutable snapshot: the block cannot be freed or mutated while it
    /// is held, and reads through it take no engine lock. Keep it
    /// short-lived (one read pass) — a held reference blocks the writer's
    /// resize reflow.
    pub fn block_acquire(&self, handle: vt::BlockHandle) -> Option<BlockRef> {
        let mut raw: vt::BlockRef = ptr::null_mut();
        if unsafe { vt::ghostty_terminal_block_acquire(self.terminal, handle, &mut raw) }
            != vt::Result::SUCCESS
            || raw.is_null()
        {
            return None;
        }
        let mut cols: u16 = 0;
        unsafe {
            let _ = vt::ghostty_block_ref_cols(raw, &mut cols);
        }
        Some(BlockRef { raw, cols })
    }

    /// [`Self::block_acquire`] plus everything a frame's read pass needs
    /// from under the engine lock in one call: the palette styles resolve
    /// against and the block's Kitty placements in block-relative
    /// coordinates. Every subsequent text read through the
    /// returned reference is lock-free.
    pub fn acquire_block_snapshot(&mut self, handle: vt::BlockHandle) -> Option<AcquiredBlock> {
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
        handle: vt::BlockHandle,
        row: usize,
        palette: &[vt::ColorRgb; 256],
        on_cell: impl FnMut(u16, CellText, CellWide, SnapshotStyle),
    ) -> Result<Option<ScreenRowMeta>> {
        let mut grid_ref = vt::GridRef::default();
        match unsafe {
            vt::ghostty_terminal_block_grid_ref(self.terminal, handle, row, &mut grid_ref)
        } {
            vt::Result::SUCCESS => {}
            vt::Result::NO_VALUE | vt::Result::INVALID_VALUE => return Ok(None),
            other => {
                Error::from_code(other)?;
                return Ok(None);
            }
        }
        let cols = self.block_cols(handle).unwrap_or(self.cols);
        Ok(Some(Self::visit_row_cells(
            grid_ref, cols, palette, on_cell,
        )?))
    }

    /// Materializing convenience over [`Self::read_block_row_visit`] — test-only.
    #[cfg(test)]
    pub fn read_block_row(
        &self,
        handle: vt::BlockHandle,
        row: usize,
    ) -> Result<Option<ScreenRowRead>> {
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

    /// The engine's current 256-color palette (OSC 4 overrides applied). Used to
    /// resolve palette-tagged style colors into concrete RGB at read time — by
    /// the harvester (once per batch) and by app-side `BlockRef` readers (once
    /// per acquire).
    pub fn color_palette(&self) -> [vt::ColorRgb; 256] {
        let mut palette = [vt::ColorRgb::default(); 256];
        unsafe {
            let _ = vt::ghostty_terminal_get(
                self.terminal,
                vt::TerminalData::COLOR_PALETTE,
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
        grid_ref: &vt::GridRef,
        tag: vt::PointTag::Type,
    ) -> Result<Option<(u16, u32)>> {
        let mut out = vt::PointCoordinate::default();
        match unsafe {
            vt::ghostty_terminal_point_from_grid_ref(self.terminal, grid_ref, tag, &mut out)
        } {
            vt::Result::SUCCESS => Ok(Some((out.x, out.y))),
            vt::Result::NO_VALUE => Ok(None),
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
        selection: Option<&vt::Selection>,
        unwrap: bool,
        trim: bool,
    ) -> Result<String> {
        self.format_terminal(vt::FormatterFormat::PLAIN, selection, unwrap, trim)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    }

    /// Export the complete terminal state as a VT stream. Replaying the returned
    /// bytes reconstructs the current screen, styles, modes, palette, and cursor,
    /// which lets a newly attached client start from a consistent checkpoint.
    pub fn format_vt_state(&mut self) -> Result<Vec<u8>> {
        self.format_terminal(vt::FormatterFormat::VT, None, false, false)
    }

    fn format_terminal(
        &mut self,
        emit: vt::FormatterFormat::Type,
        selection: Option<&vt::Selection>,
        unwrap: bool,
        trim: bool,
    ) -> Result<Vec<u8>> {
        let mut opts = vt::sized!(vt::FormatterTerminalOptions);
        opts.emit = emit;
        opts.unwrap = unwrap;
        opts.trim = trim;
        opts.extra = vt::sized!(vt::FormatterTerminalExtra);
        opts.selection = selection
            .map(|s| s as *const vt::Selection)
            .unwrap_or(ptr::null());

        let mut formatter: vt::Formatter = ptr::null_mut();
        Error::from_code(unsafe {
            vt::ghostty_formatter_terminal_new(ptr::null(), &mut formatter, self.terminal, opts)
        })?;

        let mut out_ptr: *mut u8 = ptr::null_mut();
        let mut out_len: usize = 0;
        let res = Error::from_code(unsafe {
            vt::ghostty_formatter_format_alloc(formatter, ptr::null(), &mut out_ptr, &mut out_len)
        });

        let bytes = res.map(|_| {
            if out_ptr.is_null() || out_len == 0 {
                Vec::new()
            } else {
                let bytes = unsafe { std::slice::from_raw_parts(out_ptr, out_len) };
                bytes.to_vec()
            }
        });

        if !out_ptr.is_null() {
            unsafe { vt::ghostty_free(ptr::null(), out_ptr, out_len) };
        }
        unsafe { vt::ghostty_formatter_free(formatter) };
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
        let start_ref = self.grid_ref_at(vt::PointTag::SCREEN, start.0, start.1)?;
        let end_ref = self.grid_ref_at(vt::PointTag::SCREEN, end.0, end.1)?;
        let mut sel = vt::sized!(vt::Selection);
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
        let behavior = vt::TerminalScrollViewport {
            tag: vt::TerminalScrollViewportTag::DELTA,
            value: vt::TerminalScrollViewportValue { delta },
        };
        unsafe { vt::ghostty_terminal_scroll_viewport(self.terminal, behavior) };
    }

    /// Scroll the viewport to the bottom (active area).
    pub fn scroll_viewport_bottom(&mut self) {
        if self.scrollbar_override.is_some() {
            return;
        }
        let behavior = vt::TerminalScrollViewport {
            tag: vt::TerminalScrollViewportTag::BOTTOM,
            value: vt::TerminalScrollViewportValue { delta: 0 },
        };
        unsafe { vt::ghostty_terminal_scroll_viewport(self.terminal, behavior) };
    }

    /// Scroll the viewport to the top of the scrollback.
    pub fn scroll_viewport_top(&mut self) {
        if self.scrollbar_override.is_some() {
            return;
        }
        self.scroll_viewport_top_raw();
    }

    fn scroll_viewport_top_raw(&mut self) {
        let behavior = vt::TerminalScrollViewport {
            tag: vt::TerminalScrollViewportTag::TOP,
            value: vt::TerminalScrollViewportValue { delta: 0 },
        };
        unsafe { vt::ghostty_terminal_scroll_viewport(self.terminal, behavior) };
    }

    fn cursor(&self) -> Result<SnapshotCursor> {
        let mut visible = false;
        Error::from_code(unsafe {
            vt::ghostty_render_state_get(
                self.render_state,
                vt::RenderStateData::CURSOR_VISIBLE,
                (&mut visible as *mut bool).cast(),
            )
        })?;

        // DECSCUSR shape and modes-based blink come from the render state.
        let mut style: vt::RenderStateCursorVisualStyle::Type =
            vt::RenderStateCursorVisualStyle::BLOCK;
        let _ = unsafe {
            vt::ghostty_render_state_get(
                self.render_state,
                vt::RenderStateData::CURSOR_VISUAL_STYLE,
                (&mut style as *mut vt::RenderStateCursorVisualStyle::Type).cast(),
            )
        };
        let shape = match style {
            vt::RenderStateCursorVisualStyle::BAR => crate::ansi::CursorShape::Beam,
            vt::RenderStateCursorVisualStyle::UNDERLINE => crate::ansi::CursorShape::Underline,
            // BLOCK and BLOCK_HOLLOW → Block (terminal renders hollow from focus state).
            _ => crate::ansi::CursorShape::Block,
        };
        let mut blinking = false;
        let _ = unsafe {
            vt::ghostty_render_state_get(
                self.render_state,
                vt::RenderStateData::CURSOR_BLINKING,
                (&mut blinking as *mut bool).cast(),
            )
        };

        let mut has_viewport = false;
        Error::from_code(unsafe {
            vt::ghostty_render_state_get(
                self.render_state,
                vt::RenderStateData::CURSOR_VIEWPORT_HAS_VALUE,
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
            vt::ghostty_render_state_get(
                self.render_state,
                vt::RenderStateData::CURSOR_VIEWPORT_X,
                (&mut x as *mut u16).cast(),
            )
        })?;
        Error::from_code(unsafe {
            vt::ghostty_render_state_get(
                self.render_state,
                vt::RenderStateData::CURSOR_VIEWPORT_Y,
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
        let read = |data: vt::RenderStateData::Type| -> Option<ColorRgb> {
            let mut c = vt::ColorRgb::default();
            match unsafe {
                vt::ghostty_render_state_get(
                    self.render_state,
                    data,
                    (&mut c as *mut vt::ColorRgb).cast(),
                )
            } {
                vt::Result::SUCCESS => Some(ColorRgb {
                    r: c.r,
                    g: c.g,
                    b: c.b,
                }),
                _ => None,
            }
        };
        let fg = read(vt::RenderStateData::COLOR_FOREGROUND).unwrap_or_default();
        let bg = read(vt::RenderStateData::COLOR_BACKGROUND).unwrap_or_default();
        let mut has_cursor = false;
        let _ = unsafe {
            vt::ghostty_render_state_get(
                self.render_state,
                vt::RenderStateData::COLOR_CURSOR_HAS_VALUE,
                (&mut has_cursor as *mut bool).cast(),
            )
        };
        let cursor = if has_cursor {
            read(vt::RenderStateData::COLOR_CURSOR)
        } else {
            None
        };
        // Detect OSC 11 overrides by comparing the effective background
        // (override OR default) to the engine's *default* (ignoring OSC). Both come
        // from the engine, so there's no config↔u8 conversion mismatch. An override
        // is active iff they differ; `bg_override` is then `Some(effective)`.
        let read_term = |data: vt::TerminalData::Type| -> Option<ColorRgb> {
            let mut c = vt::ColorRgb::default();
            match unsafe {
                vt::ghostty_terminal_get(self.terminal, data, (&mut c as *mut vt::ColorRgb).cast())
            } {
                vt::Result::SUCCESS => Some(ColorRgb {
                    r: c.r,
                    g: c.g,
                    b: c.b,
                }),
                _ => None,
            }
        };
        let bg_effective = read_term(vt::TerminalData::COLOR_BACKGROUND);
        let bg_default = read_term(vt::TerminalData::COLOR_BACKGROUND_DEFAULT);
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

/// Read one u32-typed datum of the placement the iterator is positioned on.
fn placement_u32(
    iter: vt::KittyGraphicsPlacementIterator,
    data: vt::KittyGraphicsPlacementData::Type,
) -> u32 {
    let mut v: u32 = 0;
    unsafe {
        vt::ghostty_kitty_graphics_placement_get(iter, data, (&mut v as *mut u32).cast());
    }
    v
}

/// Read one i32-typed datum of the placement the iterator is positioned on.
fn placement_i32(
    iter: vt::KittyGraphicsPlacementIterator,
    data: vt::KittyGraphicsPlacementData::Type,
) -> i32 {
    let mut v: i32 = 0;
    unsafe {
        vt::ghostty_kitty_graphics_placement_get(iter, data, (&mut v as *mut i32).cast());
    }
    v
}

/// Grid size + resolved source rectangle of the current placement — the shared
/// tail of every placement walk. Returns `(grid_cols, grid_rows, [sx, sy, sw, sh])`.
fn placement_geometry(
    iter: vt::KittyGraphicsPlacementIterator,
    image: vt::KittyGraphicsImage,
    terminal: vt::Terminal,
) -> (u32, u32, [u32; 4]) {
    let (mut g_cols, mut g_rows) = (0u32, 0u32);
    let (mut sx, mut sy, mut sw, mut sh) = (0u32, 0u32, 0u32, 0u32);
    unsafe {
        vt::ghostty_kitty_graphics_placement_grid_size(
            iter,
            image,
            terminal,
            &mut g_cols,
            &mut g_rows,
        );
        vt::ghostty_kitty_graphics_placement_source_rect(
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
    image: vt::KittyGraphicsImage,
    image_id: u32,
    width: u32,
    height: u32,
    data_len: usize,
) -> Option<crate::graphics::GraphicData> {
    use crate::graphics::{ColorType, GraphicData, GraphicId};

    let mut format: vt::KittyImageFormat::Type = vt::KittyImageFormat::RGBA;
    unsafe {
        vt::ghostty_kitty_graphics_image_get(
            image,
            vt::KittyGraphicsImageData::FORMAT,
            (&mut format as *mut vt::KittyImageFormat::Type).cast(),
        );
    }
    let mut data_ptr: *const u8 = ptr::null();
    unsafe {
        vt::ghostty_kitty_graphics_image_get(
            image,
            vt::KittyGraphicsImageData::DATA_PTR,
            (&mut data_ptr as *mut *const u8).cast(),
        );
    }
    if data_ptr.is_null() || data_len == 0 {
        return None;
    }
    let raw = unsafe { std::slice::from_raw_parts(data_ptr, data_len) };

    let (pixels, color_type) = match format {
        vt::KittyImageFormat::RGB => (raw.to_vec(), ColorType::Rgb),
        vt::KittyImageFormat::RGBA => (raw.to_vec(), ColorType::Rgba),
        vt::KittyImageFormat::GRAY => {
            let mut px = Vec::with_capacity(raw.len() * 4);
            for &g in raw {
                px.extend_from_slice(&[g, g, g, 255]);
            }
            (px, ColorType::Rgba)
        }
        vt::KittyImageFormat::GRAY_ALPHA => {
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
        transmit_time: std::time::Instant::now(),
    })
}

impl Drop for GhosttyTerminal {
    fn drop(&mut self) {
        unsafe {
            vt::ghostty_kitty_graphics_placement_iterator_free(self.placement_iter);
            vt::ghostty_render_state_row_iterator_free(self.row_iter);
            vt::ghostty_render_state_free(self.render_state);
            vt::ghostty_terminal_free(self.terminal);
        }
    }
}

/// An acquired read reference to a finished block (engine-refcounted).
///
/// Pins an immutable snapshot of the block: while held, the block cannot
/// be freed (removal/eviction defer destruction) or mutated (reflow
/// drains readers first), and every read here takes no engine lock — the
/// render thread can read frozen blocks while the PTY thread is inside a
/// `write_vt` burst. Released on drop.
///
/// Keep references short-lived (one read pass, e.g. a frame): a held
/// reference blocks the engine's resize reflow of this block.
pub struct BlockRef {
    raw: vt::BlockRef,
    cols: u16,
}

// SAFETY: the engine's block_ref contract is explicitly any-thread —
// acquire/release and all block_ref_* readers synchronize internally
// (refcount under the block-set mutex; the pinned data is immutable).
unsafe impl Send for BlockRef {}
unsafe impl Sync for BlockRef {}

impl BlockRef {
    /// The `(id, generation)` of the pinned snapshot — the stable cache
    /// key for shaped/rendered rows.
    pub fn handle(&self) -> vt::BlockHandle {
        let mut handle = vt::BlockHandle::default();
        unsafe {
            let _ = vt::ghostty_block_ref_handle(self.raw, &mut handle);
        }
        handle
    }

    /// Logical row count of the snapshot.
    pub fn row_count(&self) -> usize {
        let mut rows: usize = 0;
        unsafe {
            let _ = vt::ghostty_block_ref_row_count(self.raw, &mut rows);
        }
        rows
    }

    /// Column count of the snapshot (the width it is currently laid out
    /// at).
    pub fn cols(&self) -> u16 {
        self.cols
    }

    /// Page-storage bytes of the snapshot.
    pub fn bytes(&self) -> usize {
        let mut bytes: usize = 0;
        unsafe {
            let _ = vt::ghostty_block_ref_bytes(self.raw, &mut bytes);
        }
        bytes
    }

    /// Walk one row of the snapshot with styles — same visitor shape as
    /// [`GhosttyTerminal::read_screen_row_visit`], but without the engine
    /// lock. `None` for a row at/beyond the logical row count.
    pub fn read_row_visit(
        &self,
        row: usize,
        palette: &[vt::ColorRgb; 256],
        on_cell: impl FnMut(u16, CellText, CellWide, SnapshotStyle),
    ) -> Result<Option<ScreenRowMeta>> {
        let mut grid_ref = vt::GridRef::default();
        match unsafe { vt::ghostty_block_ref_grid_ref(self.raw, row, &mut grid_ref) } {
            vt::Result::SUCCESS => {}
            vt::Result::INVALID_VALUE => return Ok(None),
            other => {
                Error::from_code(other)?;
                return Ok(None);
            }
        }
        Ok(Some(GhosttyTerminal::visit_row_cells(
            grid_ref, self.cols, palette, on_cell,
        )?))
    }

    /// The snapshot's Kitty graphics storage handle, for placement
    /// iteration and lazy pixel upload of frozen images. Valid while this
    /// reference is held. `None` if kitty graphics are disabled at build
    /// time.
    pub fn kitty_graphics_raw(&self) -> Option<vt::KittyGraphics> {
        let mut graphics: vt::KittyGraphics = ptr::null_mut();
        (unsafe { vt::ghostty_block_ref_kitty_graphics(self.raw, &mut graphics) }
            == vt::Result::SUCCESS
            && !graphics.is_null())
        .then_some(graphics)
    }

    /// [`Self::format_range`] with caller-friendly endpoints: `None` means
    /// the block edge, and rows/columns clamp into the snapshot's bounds —
    /// the shape a selection copy produces. `None` for an
    /// empty block.
    pub fn format_range_clamped(
        &self,
        start: Option<(usize, u32)>,
        end: Option<(usize, u32)>,
        unwrap: bool,
        trim: bool,
    ) -> Option<String> {
        let rows = self.row_count();
        if rows == 0 {
            return None;
        }
        let last_col = u32::from(self.cols().saturating_sub(1));
        let clamp = |(row, col): (usize, u32)| {
            (
                row.min(rows - 1),
                col.min(last_col).min(u16::MAX as u32) as u16,
            )
        };
        let tl = clamp(start.unwrap_or((0, 0)));
        let br = clamp(end.unwrap_or((rows - 1, last_col)));
        self.format_range(tl, br, unwrap, trim).ok()
    }

    /// Export an inclusive cell range of the snapshot as plain text — the
    /// copy/deep-search floor. Cross-block copy concatenates per-block
    /// exports so no cross-block engine lock is needed.
    pub fn format_range(
        &self,
        tl: (usize, u16),
        br: (usize, u16),
        unwrap: bool,
        trim: bool,
    ) -> Result<String> {
        let mut opts = vt::sized!(vt::BlockFormatOptions);
        opts.tl_row = tl.0;
        opts.tl_col = tl.1;
        opts.br_row = br.0;
        opts.br_col = br.1;
        opts.unwrap = unwrap;
        opts.trim = trim;

        let mut out_ptr: *mut u8 = ptr::null_mut();
        let mut out_len: usize = 0;
        Error::from_code(unsafe {
            vt::ghostty_block_ref_format_alloc(
                self.raw,
                ptr::null(),
                opts,
                &mut out_ptr,
                &mut out_len,
            )
        })?;
        let text = if out_ptr.is_null() || out_len == 0 {
            String::new()
        } else {
            let bytes = unsafe { std::slice::from_raw_parts(out_ptr, out_len) };
            String::from_utf8_lossy(bytes).into_owned()
        };
        if !out_ptr.is_null() {
            unsafe { vt::ghostty_free(ptr::null(), out_ptr, out_len) };
        }
        Ok(text)
    }
}

impl Drop for BlockRef {
    fn drop(&mut self) {
        unsafe { vt::ghostty_block_ref_release(self.raw) }
    }
}

/// One acquired frozen block, bundled with everything a read pass needs
/// from under the engine lock: the pinned reference, the palette its styles
/// resolve against, and its Kitty placements in block-relative coordinates
/// so callers can finish reading after releasing the engine lock. Produced by
/// [`GhosttyTerminal::acquire_block_snapshot`].
pub struct AcquiredBlock {
    pub block: BlockRef,
    pub palette: Palette,
    pub placements: Vec<PlacementScreenPos>,
}

/// Resolve a tagged style color against the palette. `None` for the default
/// (terminal-level) color, concrete RGB otherwise.
fn style_color_resolve(c: &vt::StyleColor, palette: &[vt::ColorRgb; 256]) -> Option<Color> {
    match c.tag {
        vt::StyleColorTag::PALETTE => {
            let idx = unsafe { c.value.palette } as usize;
            palette.get(idx).map(|&rgb| rgb.into())
        }
        vt::StyleColorTag::RGB => Some(unsafe { c.value.rgb }.into()),
        _ => None,
    }
}

/// Read the full grapheme cluster of a `GridRef` cell as a `String`. Empty for
/// blank cells. Stack buffer first; falls back to a heap read for oversized
/// clusters (same two-call pattern as `grid_ref_hyperlink_uri`).
fn grid_ref_graphemes(r: &vt::GridRef) -> String {
    fn to_string(codepoints: &[u32]) -> String {
        codepoints
            .iter()
            .map(|&cp| char::from_u32(cp).unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect()
    }

    let mut buf = [0u32; 8];
    let mut len: usize = 0;
    match unsafe { vt::ghostty_grid_ref_graphemes(r, buf.as_mut_ptr(), buf.len(), &mut len) } {
        vt::Result::SUCCESS => to_string(&buf[..len]),
        vt::Result::OUT_OF_SPACE => {
            let mut big = vec![0u32; len];
            match unsafe {
                vt::ghostty_grid_ref_graphemes(r, big.as_mut_ptr(), big.len(), &mut len)
            } {
                vt::Result::SUCCESS => to_string(&big[..len]),
                _ => String::new(),
            }
        }
        _ => String::new(),
    }
}

/// Read the OSC 8 hyperlink URI for a resolved `GridRef`, or `None` if the cell
/// has none. Two-call pattern: a NULL probe yields the required length (`out_len`
/// is 0 ⇒ no hyperlink), then a sized read.
fn grid_ref_hyperlink_uri(r: &vt::GridRef) -> Option<String> {
    let mut len: usize = 0;
    unsafe {
        vt::ghostty_grid_ref_hyperlink_uri(r, ptr::null_mut(), 0, &mut len);
    }
    if len == 0 {
        return None;
    }
    let mut buf = vec![0u8; len];
    let rc =
        unsafe { vt::ghostty_grid_ref_hyperlink_uri(r, buf.as_mut_ptr(), buf.len(), &mut len) };
    if rc != vt::Result::SUCCESS {
        return None;
    }
    buf.truncate(len);
    String::from_utf8(buf).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line_text(snapshot: &RenderBuffer, row: usize) -> String {
        let mut text = String::new();
        for x in 0..snapshot.cols() {
            let cell = snapshot.cell(x, row);
            if cell.c() == '\0'
                || matches!(
                    cell.wide(),
                    crate::terminal::square::Wide::Spacer
                        | crate::terminal::square::Wide::LeadingSpacer
                )
            {
                continue;
            }
            text.push(cell.c());
            if let Some(extras) = cell.extras_id().and_then(|id| snapshot.extras().get(&id)) {
                text.extend(&extras.zerowidth);
            }
        }
        text
    }

    /// Finishing freezes content into an engine block readable
    /// through the block row visitor; the active screen restarts empty with
    /// SGR carried over; stale handles read as absent.
    #[test]
    fn finish_block_freezes_and_reads_back() {
        let mut t = GhosttyTerminal::new(20, 5, 10_000).unwrap();

        // Empty screen: no block.
        assert!(t.finish_block().unwrap().is_none());

        // Bold "hello" + newline + "world".
        t.write_vt(b"\x1b[1mhello\r\nworld");
        let handle = t.finish_block().unwrap().expect("block created");
        assert_eq!(t.block_count(), 1);
        assert_eq!(t.block_at(0).map(|h| h.id), Some(handle.id));
        assert_eq!(t.block_row_count(handle), Some(2));
        assert_eq!(t.block_cols(handle), Some(20));

        let row0 = t.read_block_row(handle, 0).unwrap().expect("row 0");
        let text: String = row0.cells.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(text, "hello");
        assert!(row0.cells[0].style.bold, "SGR captured in frozen block");
        let row1 = t.read_block_row(handle, 1).unwrap().expect("row 1");
        let text: String = row1.cells.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(text, "world");
        assert!(
            t.read_block_row(handle, 2).unwrap().is_none(),
            "beyond logical rows"
        );

        // Active screen restarted empty; SGR continues (bold pen).
        let snap = t.snapshot().unwrap();
        assert_eq!((snap.cursor().col.0, snap.cursor().row.0), (0, 0));
        t.write_vt(b"next");
        let row = t.read_screen_row(0).unwrap().expect("active row");
        assert!(row.cells[0].style.bold, "continuation SGR applies");

        // The frozen block never changes.
        let row0 = t.read_block_row(handle, 0).unwrap().expect("row 0 again");
        let text: String = row0.cells.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(text, "hello");

        // Stale after removal; ids are never reused.
        assert!(t.remove_block(handle));
        assert!(!t.remove_block(handle));
        assert_eq!(t.block_row_count(handle), None);
        assert_eq!(t.block_count(), 0);

        t.write_vt(b"again");
        let h2 = t.finish_block().unwrap().expect("second block");
        assert_ne!(h2.id, handle.id);
        t.clear_blocks();
        assert_eq!(t.block_count(), 0);
    }

    /// A stream RIS (`ESC c`) clears only the active screen — finished
    /// blocks survive so a per-sample reset does not erase frozen history.
    #[test]
    fn finish_block_survives_stream_ris() {
        let mut t = GhosttyTerminal::new(20, 5, 10_000).unwrap();
        t.write_vt(b"hello");
        let handle = t.finish_block().unwrap().expect("block created");
        t.write_vt(b"\x1bc");
        assert_eq!(t.block_count(), 1);
        assert_eq!(t.block_row_count(handle), Some(1));
    }

    /// An acquired block reference reads rows and text without the
    /// terminal, survives removal (deferred destroy), and format-exports.
    #[test]
    fn block_ref_reads_and_survives_removal() {
        let mut t = GhosttyTerminal::new(20, 5, 10_000).unwrap();
        t.write_vt(b"\x1b[1mhello\r\nworld");
        let handle = t.finish_block().unwrap().expect("block created");

        let r = t.block_acquire(handle).expect("acquire");
        assert_eq!(r.handle().id, handle.id);
        assert_eq!(r.row_count(), 2);
        assert_eq!(r.cols(), 20);
        assert!(r.bytes() > 0);

        let palette = t.color_palette();
        let mut text = String::new();
        let meta = r
            .read_row_visit(0, &palette, |_, cell_text, _, style| {
                text.push_str(cell_text.as_str());
                assert!(style.bold);
            })
            .unwrap()
            .expect("row 0");
        assert!(!meta.wrapped);
        assert_eq!(text, "hello");
        assert!(
            r.read_row_visit(2, &palette, |_, _, _, _| {})
                .unwrap()
                .is_none()
        );

        assert_eq!(
            r.format_range((0, 0), (1, 19), true, true)
                .unwrap()
                .trim_end(),
            "hello\nworld"
        );

        // Remove while held: handle stale, snapshot still readable.
        assert!(t.remove_block(handle));
        assert!(t.block_acquire(handle).is_none());
        assert_eq!(r.row_count(), 2);
        drop(r);
    }

    /// Block references can be read and released on another thread
    /// while the writer keeps finishing, resizing (reflow drains readers),
    /// and removing blocks.
    #[test]
    fn block_ref_cross_thread_reads() {
        let mut t = GhosttyTerminal::new(20, 5, 10_000).unwrap();
        let palette = t.color_palette();

        let (tx, rx) = std::sync::mpsc::channel::<BlockRef>();
        let reader = std::thread::spawn(move || {
            let mut cells = 0usize;
            for r in rx {
                let rows = r.row_count();
                for row in 0..rows {
                    let _ = r.read_row_visit(row, &palette, |_, _, _, _| cells += 1);
                }
                if rows > 0 {
                    let _ = r.format_range((0, 0), (rows - 1, r.cols() - 1), true, true);
                }
            }
            cells
        });

        let mut cols = 20u16;
        for i in 0..60 {
            t.write_vt(b"the quick brown fox jumps over the lazy dog\r\n");
            let handle = t.finish_block().unwrap().expect("block created");
            if let Some(r) = t.block_acquire(handle) {
                tx.send(r).unwrap();
            }
            if i % 10 == 9 {
                // Reflow of every block: the engine drains reader refs
                // (including any still queued in the channel) per block.
                cols = if cols == 20 { 26 } else { 20 };
                t.resize(cols, 5, 10, 20).unwrap();
            }
            if i % 15 == 14 {
                // Deferred destroy while the reader may hold the ref.
                t.remove_block(handle);
            }
        }
        drop(tx);
        let cells = reader.join().unwrap();
        assert!(cells > 0, "reader observed content");
    }

    /// Shrinking the block budget evicts oldest-first immediately,
    /// keeping the newest; blocks_bytes reports the enforced total.
    #[test]
    fn block_budget_evicts_oldest() {
        let mut t = GhosttyTerminal::new(20, 5, 10_000).unwrap();
        let mut last = None;
        for _ in 0..3 {
            t.write_vt(b"hello");
            last = t.finish_block().unwrap();
        }
        assert_eq!(t.block_count(), 3);
        assert!(t.blocks_bytes() > 0);

        t.set_block_budget_bytes(1).unwrap();
        assert_eq!(t.block_count(), 1);
        assert_eq!(t.block_at(0).map(|h| h.id), last.map(|h| h.id));
    }

    /// Resize rewraps finished blocks to the new width and
    /// bumps their data generation; block reads follow the new layout.
    #[test]
    fn block_reflows_on_resize() {
        let mut t = GhosttyTerminal::new(10, 5, 10_000).unwrap();
        t.write_vt(b"0123456789ABC"); // wraps into 2 rows at 10 cols
        let handle = t.finish_block().unwrap().expect("block created");
        assert_eq!(t.block_row_count(handle), Some(2));

        t.resize(5, 5, 10, 20).unwrap();
        assert_eq!(t.block_row_count(handle), Some(3));
        assert_eq!(t.block_cols(handle), Some(5));
        let generation = t.block_at(0).map(|h| h.generation);
        assert_eq!(generation, Some(handle.generation + 1));

        let row = t.read_block_row(handle, 1).unwrap().expect("row 1");
        let text: String = row.cells.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(text, "56789");
    }

    /// Transmitting and placing a Kitty image surfaces a non-virtual placement
    /// in the snapshot at the cursor's viewport position.
    #[test]
    fn kitty_image_placement() {
        let mut t = GhosttyTerminal::new(20, 5, 100).unwrap();
        // Give the engine a cell geometry so placement pixel/grid size resolves.
        t.resize(20, 5, 10, 20).unwrap();

        // Transmit + display (a=T) a 1×1 RGBA image (f=32), id=1, placement p=9.
        // Payload is one opaque-red pixel (FF 00 00 FF) base64-encoded.
        t.write_vt(b"\x1b_Ga=T,f=32,s=1,v=1,i=1,p=9;/wAA/w==\x1b\\");

        let snap = t.snapshot().unwrap();
        let visible: Vec<_> = snap.placements().iter().filter(|p| !p.is_virtual).collect();
        assert_eq!(visible.len(), 1, "one non-virtual placement");
        let p = visible[0];
        assert_eq!(p.image_id, 1);
        assert_eq!(
            p.placement_id, 9,
            "ordinary placement carries its placement id"
        );
        assert_eq!((p.viewport_col, p.viewport_row), (0, 0), "placed at cursor");
        assert!(p.grid_cols >= 1 && p.grid_rows >= 1, "spans >=1 cell");
        assert!(
            p.pixel_width >= 1 && p.pixel_height >= 1,
            "has rendered pixels"
        );
        // Ordinary geometry unchanged: full 1×1 source rectangle, no sub-cell offset.
        assert_eq!(
            (p.source_x, p.source_y, p.source_width, p.source_height),
            (0, 0, 1, 1),
            "full-image source rectangle"
        );
        assert_eq!(
            (p.cell_x_offset, p.cell_y_offset),
            (0, 0),
            "no sub-cell offset"
        );

        // The delta reader ships each pixel generation exactly once.
        let (first, removed) = t.take_image_deltas(snap.placements());
        assert!(removed.is_empty(), "nothing removed on first ship");
        assert!(
            first.iter().any(|(id, _)| *id == 1),
            "first batch ships image 1's pixels"
        );
        // A second call with no intervening write must yield nothing — neither a
        // re-ship nor a removal (idempotent steady state).
        let snap2 = t.snapshot().unwrap();
        let (second, removed2) = t.take_image_deltas(snap2.placements());
        assert!(
            second.is_empty() && removed2.is_empty(),
            "unchanged batch: {} pending / {} removed (want 0/0)",
            second.len(),
            removed2.len()
        );
    }

    /// The backend delta key is `(id, width, height,
    /// data_len)` because the pinned FFI exposes no image generation counter. A
    /// same-ID retransmission whose width, height, and byte length are unchanged
    /// is therefore NOT observed as a delta and is not re-shipped, even if the
    /// pixel bytes differ. This known limitation needs a future Ghostty generation
    /// field to distinguish same-sized retransmissions.
    #[test]
    fn kitty_same_size_retransmit_not_reshipped() {
        let mut t = GhosttyTerminal::new(20, 5, 100).unwrap();
        t.resize(20, 5, 10, 20).unwrap();

        // Transmit + place a 1×1 opaque-red RGBA image, id=1.
        t.write_vt(b"\x1b_Ga=T,f=32,s=1,v=1,i=1;/wAA/w==\x1b\\");
        let snap = t.snapshot().unwrap();
        let (first, _) = t.take_image_deltas(snap.placements());
        assert!(first.iter().any(|(id, _)| *id == 1), "first ship");

        // Retransmit the SAME id with the SAME 1×1 RGBA dimensions/length but
        // different pixels (opaque-blue). Same (id,w,h,len) key ⇒ not re-shipped.
        t.write_vt(b"\x1b_Ga=T,f=32,s=1,v=1,i=1;AAD/fw==\x1b\\");
        let snap = t.snapshot().unwrap();
        let (second, removed) = t.take_image_deltas(snap.placements());
        assert!(
            second.iter().all(|(id, _)| *id != 1) && !removed.contains(&1),
            "same-size same-id retransmission is not re-shipped (known residual)"
        );
    }

    /// With the registered PNG decode hook, an `f=100` transmission is
    /// decoded by the engine to RGBA and shipped by `take_image_deltas`.
    #[test]
    fn kitty_png_decode() {
        use base64::Engine as _;

        let mut t = GhosttyTerminal::new(20, 5, 100).unwrap();
        t.resize(20, 5, 10, 20).unwrap();

        // A 1×1 opaque-red PNG, generated so the bytes are unquestionably valid.
        let img = image_rs::RgbaImage::from_pixel(1, 1, image_rs::Rgba([255, 0, 0, 255]));
        let mut png = Vec::new();
        image_rs::DynamicImage::ImageRgba8(img)
            .write_to(
                &mut std::io::Cursor::new(&mut png),
                image_rs::ImageFormat::Png,
            )
            .unwrap();
        let b64 = base64::engine::general_purpose::STANDARD.encode(&png);

        t.write_vt(format!("\x1b_Ga=T,f=100,i=2;{b64}\x1b\\").as_bytes());

        let snap = t.snapshot().unwrap();
        let (pending, _) = t.take_image_deltas(snap.placements());
        let img = pending
            .iter()
            .find(|(id, _)| *id == 2)
            .map(|(_, d)| d)
            .expect("PNG image decoded + shipped");
        assert_eq!((img.width, img.height), (1, 1));
        assert_eq!(img.color_type, crate::graphics::ColorType::Rgba);
        assert_eq!(img.pixels.len(), 4, "1×1 RGBA = 4 bytes");
    }

    /// A Kitty Unicode-placeholder cell (U+10EEEE, image id in the foreground)
    /// sets the per-row `KITTY_VIRTUAL_PLACEHOLDER` flag, the snapshot reports a
    /// virtual placement carrying the id, and its pixels still ship via the
    /// delta path (virtual placements).
    #[test]
    fn virtual_placeholder_row_flag() {
        let mut t = GhosttyTerminal::new(20, 5, 100).unwrap();
        t.resize(20, 5, 10, 20).unwrap();

        // Transmit a 1×2 RGBA image (id=7) as a *virtual* placement (U=1) with an
        // explicit placement id (p=3), grid size (c=2,r=1), and z (z=5): it has no
        // engine grid position; placeholders position it, but its identity, grid
        // size, and z must be exposed so the frame path can match runs.
        t.write_vt(b"\x1b_Ga=T,U=1,f=32,s=1,v=2,i=7,p=3,c=2,r=1,z=5;/wAA//8AAP8=\x1b\\");
        // Print one placeholder cell on row 0 with the image id (7) in the fg.
        let cell = format!("\x1b[38;2;0;0;7m{}", '\u{10EEEE}');
        t.write_vt(cell.as_bytes());

        let snap = t.snapshot().unwrap();
        assert!(
            snap.row_has_virtual_placeholder(0),
            "row 0 carries the virtual-placeholder flag"
        );
        assert!(
            (1..snap.rows()).all(|y| !snap.row_has_virtual_placeholder(y)),
            "no other row is flagged"
        );

        let virt: Vec<_> = snap.placements().iter().filter(|p| p.is_virtual).collect();
        assert_eq!(virt.len(), 1, "one virtual placement");
        let v = virt[0];
        assert_eq!(v.image_id, 7);
        assert_eq!(v.placement_id, 3, "virtual placement id exposed");
        assert_eq!(
            (v.grid_cols, v.grid_rows),
            (2, 1),
            "virtual grid size exposed"
        );
        assert_eq!(v.z, 5, "virtual z-index exposed");

        // Virtual placements ship pixels through the same delta path.
        let (pending, _) = t.take_image_deltas(snap.placements());
        assert!(
            pending.iter().any(|(id, _)| *id == 7),
            "virtual placement's image pixels ship by id"
        );
    }

    /// Scrolling moves a placement's viewport row by the scroll delta. An image
    /// scrolled fully off-screen is not reported removed by `take_image_deltas`
    /// because it remains live in the engine; scrolling must not emit graphics churn.
    #[test]
    fn kitty_image_scroll() {
        let mut t = GhosttyTerminal::new(20, 5, 100).unwrap();
        t.resize(20, 5, 10, 20).unwrap();

        // Lay down `a b c <image> d e f g h` so the image lands at absolute row 3
        // and is pushed into scrollback (9 rows, 5-row viewport).
        t.write_vt(b"a\r\nb\r\nc\r\n");
        t.write_vt(b"\x1b_Ga=T,f=32,s=1,v=1,i=1;/wAA/w==\x1b\\");
        t.write_vt(b"\r\nd\r\ne\r\nf\r\ng\r\nh");

        let find_row = |t: &mut GhosttyTerminal| -> Option<i32> {
            let snap = t.snapshot().unwrap();
            snap.placements()
                .iter()
                .find(|p| p.image_id == 1 && !p.is_virtual)
                .map(|p| p.viewport_row)
        };

        // Scroll up so the image is visible; record its viewport row, ship it.
        t.scroll_viewport_bottom();
        t.scroll_viewport_delta(-2);
        let r0 = find_row(&mut t).expect("image visible after scrolling up 2");
        let snap = t.snapshot().unwrap();
        let (shipped, _) = t.take_image_deltas(snap.placements());
        assert!(
            shipped.iter().any(|(id, _)| *id == 1),
            "shipped while visible"
        );

        // One more row up moves a fixed placement down by exactly one.
        t.scroll_viewport_delta(-1);
        let r1 = find_row(&mut t).expect("image still visible after one more row");
        assert_eq!((r1 - r0).abs(), 1, "viewport row moves by the scroll delta");

        // Scroll back to the bottom so the image is fully off-screen, then run a
        // delta pass: the image is still in the engine, so it must NOT be removed
        // (and not re-shipped).
        t.scroll_viewport_bottom();
        let snap = t.snapshot().unwrap();
        assert!(
            !snap
                .placements()
                .iter()
                .any(|p| p.image_id == 1 && !p.is_virtual),
            "off-screen: no visible placement"
        );
        let (pending, removed) = t.take_image_deltas(snap.placements());
        assert!(
            !removed.contains(&1),
            "off-screen image must not be removed"
        );
        assert!(
            !pending.iter().any(|(id, _)| *id == 1),
            "off-screen image must not be re-shipped"
        );
    }

    /// Deleting an image with `d=I` frees its data, removes its placement from
    /// the snapshot and reports the id in `take_image_deltas`' remove queue.
    #[test]
    fn kitty_image_delete() {
        let mut t = GhosttyTerminal::new(20, 5, 100).unwrap();
        t.resize(20, 5, 10, 20).unwrap();

        t.write_vt(b"\x1b_Ga=T,f=32,s=1,v=1,i=1;/wAA/w==\x1b\\");
        let snap = t.snapshot().unwrap();
        let (shipped, _) = t.take_image_deltas(snap.placements());
        assert!(
            shipped.iter().any(|(id, _)| *id == 1),
            "shipped before delete"
        );

        // Delete image id=1 and free its data (uppercase d=I).
        t.write_vt(b"\x1b_Ga=d,d=I,i=1\x1b\\");
        let snap = t.snapshot().unwrap();
        assert!(
            !snap.placements().iter().any(|p| p.image_id == 1),
            "no placement remains after delete"
        );
        let (_, removed) = t.take_image_deltas(snap.placements());
        assert!(removed.contains(&1), "deleted image reported for removal");
    }

    /// DECSCUSR shape and DECTCEM visibility land in the snapshot cursor.
    #[test]
    fn snapshot_captures_cursor_style() {
        use crate::ansi::CursorShape;
        let mut t = GhosttyTerminal::new(20, 5, 100).unwrap();

        t.write_vt(b"\x1b[2 q"); // steady block
        assert_eq!(t.snapshot().unwrap().cursor_shape(), CursorShape::Block);
        t.write_vt(b"\x1b[5 q"); // steady bar
        assert_eq!(t.snapshot().unwrap().cursor_shape(), CursorShape::Beam);
        t.write_vt(b"\x1b[3 q"); // blinking underline
        assert_eq!(t.snapshot().unwrap().cursor_shape(), CursorShape::Underline);

        assert!(t.snapshot().unwrap().cursor_visible(), "visible by default");
        t.write_vt(b"\x1b[?25l"); // DECTCEM hide
        assert!(
            !t.snapshot().unwrap().cursor_visible(),
            "hidden after DECTCEM"
        );
        t.write_vt(b"\x1b[?25h");
        assert!(t.snapshot().unwrap().cursor_visible(), "shown again");
    }

    /// OSC 10/11 dynamic foreground and background land in the snapshot colors.
    /// The 256-entry palette enters through `set_colors`; snapshots capture only
    /// foreground, background, and the background override.
    #[test]
    fn snapshot_captures_colors() {
        use nmt_config::colors::{ColorRgb, NamedColor};
        let mut t = GhosttyTerminal::new(8, 3, 100).unwrap();
        t.set_colors(
            [205, 214, 244],
            [15, 13, 14],
            [180, 190, 254],
            &[[0u8; 3]; 256],
        );

        t.write_vt(b"\x1b]10;#112233\x07"); // OSC 10 set foreground
        assert_eq!(
            t.snapshot().unwrap().colors()[NamedColor::Foreground],
            Some(
                ColorRgb {
                    r: 0x11,
                    g: 0x22,
                    b: 0x33
                }
                .to_arr()
            ),
            "OSC 10 sets the effective foreground"
        );

        t.write_vt(b"\x1b]11;#445566\x07"); // OSC 11 set background
        assert_eq!(
            t.snapshot().unwrap().window_bg_override(),
            Some(ColorRgb {
                r: 0x44,
                g: 0x55,
                b: 0x66
            }),
            "OSC 11 sets the background override"
        );
    }

    #[test]
    fn theme_colors_update_engine_defaults() {
        use nmt_config::colors::{ColorRgb, Colors, NamedColor};

        let mut terminal = GhosttyTerminal::new(8, 3, 100).unwrap();
        let colors = Colors::default();
        terminal.set_theme_colors(&colors);

        let snapshot = terminal.snapshot().unwrap();
        assert_eq!(
            snapshot.colors()[NamedColor::Foreground],
            Some(ColorRgb::from_color_arr(colors.foreground).to_arr())
        );
        assert_eq!(
            snapshot.colors()[NamedColor::Background],
            Some(ColorRgb::from_color_arr(colors.background.0).to_arr())
        );
    }

    /// A VT mode set/reset round-trips through the engine `mode()` reader
    /// and feeds the lock-free per-panel atomic consumed by the input path.
    #[test]
    fn vt_mode_get_roundtrip() {
        let mut t = GhosttyTerminal::new(8, 3, 100).unwrap();

        assert!(!t.mode(mode::CURSOR_KEYS), "app-cursor off by default");
        t.write_vt(b"\x1b[?1h"); // DECCKM on
        assert!(t.mode(mode::CURSOR_KEYS), "DECCKM on after ?1h");
        t.write_vt(b"\x1b[?1l"); // DECCKM off
        assert!(!t.mode(mode::CURSOR_KEYS), "DECCKM off after ?1l");

        // Alt screen toggles independently.
        t.write_vt(b"\x1b[?1049h");
        assert!(t.mode(mode::ALT_SCREEN), "alt-screen on");
        t.write_vt(b"\x1b[?1049l");
        assert!(!t.mode(mode::ALT_SCREEN), "alt-screen off");
    }

    /// A small storage limit evicts older images once exceeded; only the
    /// retained image is still in the engine store.
    #[test]
    fn kitty_storage_limit() {
        let mut t = GhosttyTerminal::new(20, 5, 100).unwrap();
        t.resize(20, 5, 10, 20).unwrap();
        // Room for ~one small image: a 2×2 RGBA is 16 bytes.
        t.set_kitty_storage_limit(24);

        // Two distinct 2×2 RGBA images (ids 1, 2). Base64 of 16 bytes = 24 chars.
        use base64::Engine as _;
        let px = base64::engine::general_purpose::STANDARD.encode([0u8; 16]);
        t.write_vt(format!("\x1b_Ga=t,f=32,s=2,v=2,i=1;{px}\x1b\\").as_bytes());
        t.write_vt(format!("\x1b_Ga=t,f=32,s=2,v=2,i=2;{px}\x1b\\").as_bytes());

        // The newest image survives; the oldest was evicted to honour the limit.
        assert!(t.kitty_image_exists(2), "newest image retained");
        assert!(
            !t.kitty_image_exists(1),
            "oldest image evicted by the limit"
        );
    }

    /// a sixel sequence is ignored (terminal drops sixel) without panicking and
    /// leaves a valid, image-free snapshot.
    #[test]
    fn sixel_ignored_no_crash() {
        let mut t = GhosttyTerminal::new(20, 5, 100).unwrap();
        t.resize(20, 5, 10, 20).unwrap();
        t.write_vt(b"\x1bPq#0;2;100;0;0#0~~~~~\x1b\\");
        let snap = t.snapshot().unwrap();
        assert!(
            snap.placements().is_empty(),
            "no kitty placements from sixel"
        );
    }

    /// an iTerm2 inline-image (OSC 1337) is ignored without panicking and
    /// leaves a valid, image-free snapshot.
    #[test]
    fn iterm2_ignored_no_crash() {
        let mut t = GhosttyTerminal::new(20, 5, 100).unwrap();
        t.resize(20, 5, 10, 20).unwrap();
        t.write_vt(b"\x1b]1337;File=inline=1:AAAA\x07");
        let snap = t.snapshot().unwrap();
        assert!(
            snap.placements().is_empty(),
            "no kitty placements from iTerm2"
        );
    }

    /// OSC 133 shell-integration marks are an unknown OSC to the engine and must
    /// be ignored. The PTY sniffer forwards those marks unchanged, so they must
    /// leave only the visible text, no garbage cells.
    #[test]
    fn osc133_marks_ignored_no_crash() {
        let mut t = GhosttyTerminal::new(20, 5, 100).unwrap();
        t.resize(20, 5, 10, 20).unwrap();
        // ESC]133;A BEL  P>  ESC]133;B BEL  ESC]133;C BEL  hi
        t.write_vt(b"\x1b]133;A\x07P>\x1b]133;B\x07\x1b]133;C\x07hi");
        let snap = t.snapshot().unwrap();
        assert_eq!(line_text(&snap, 0).trim_end(), "P>hi");
    }

    /// OSC 11 sets the window background as an override; OSC 111 resets it.
    /// Exercises the exact FFI path the renderer reads (`snapshot().colors`).
    #[test]
    fn osc_11_set_and_111_reset_background() {
        let palette = [[0u8, 0, 0]; 256];
        let default_bg = [15u8, 13, 14];

        let run = |reset_seq: &[u8]| {
            let mut t = GhosttyTerminal::new(8, 3, 100).unwrap();
            t.set_colors([205, 214, 244], default_bg, [180, 190, 254], &palette);

            assert_eq!(
                t.snapshot().unwrap().window_bg_override(),
                None,
                "no override before any OSC"
            );

            t.write_vt(b"\x1b]11;#330000\x07");
            assert_eq!(
                t.snapshot().unwrap().window_bg_override(),
                Some(nmt_config::colors::ColorRgb { r: 51, g: 0, b: 0 }),
                "OSC 11 sets the override"
            );

            t.write_vt(reset_seq);
            assert_eq!(
                t.snapshot().unwrap().window_bg_override(),
                None,
                "OSC 111 ({reset_seq:?}) resets the override",
            );
        };

        run(b"\x1b]111\x07"); // BEL-terminated
        run(b"\x1b]111\x1b\\"); // ST-terminated
    }

    #[test]
    fn extracts_basic_vt_snapshot() {
        let mut terminal = GhosttyTerminal::new(8, 3, 100).unwrap();
        terminal.write_vt(b"hi \x1b[31mred\x1b[0m");

        let snapshot = terminal.snapshot().unwrap();

        assert_eq!(snapshot.cols(), 8);
        assert_eq!(snapshot.rows(), 3);
        assert_eq!(snapshot.cell(0, 0).c(), 'h');
        assert_eq!(snapshot.cell(3, 0).c(), 'r');
    }

    /// Verifies the selection-anchoring assumption: a SCREEN
    /// coordinate stays pinned to the same content as new output scrolls that
    /// content into scrollback. If this holds, selection anchors need no
    /// rotate-on-output.
    #[test]
    fn screen_coords_stable_across_output() {
        let mut t = GhosttyTerminal::new(20, 3, 1000).unwrap();
        t.write_vt(b"AAAA\r\nBBBB\r\nCCCC");

        // Anchor row "AAAA" (viewport row 0) to a SCREEN coordinate.
        let r = t.viewport_grid_ref(0, 0).unwrap();
        let (_, screen_y) = t
            .point_from_grid_ref(&r, vt::PointTag::SCREEN)
            .unwrap()
            .expect("viewport cell has a screen coord");

        // Output scrolls AAAA/BBBB/CCCC into history; viewport now DDDD/EEEE/FFFF.
        t.write_vt(b"\r\nDDDD\r\nEEEE\r\nFFFF");
        assert_eq!(line_text(&t.snapshot().unwrap(), 2), "FFFF");

        // The SAME screen coordinate still resolves to "AAAA".
        let start = t.grid_ref_at(vt::PointTag::SCREEN, 0, screen_y).unwrap();
        let end = t.grid_ref_at(vt::PointTag::SCREEN, 3, screen_y).unwrap();
        let mut sel = vt::sized!(vt::Selection);
        sel.start = start;
        sel.end = end;
        let text = t.format_text(Some(&sel), false, true).unwrap();
        assert_eq!(text.trim_end(), "AAAA", "screen coord drifted: {text:?}");
    }

    /// Verifies the cheap screen↔viewport mapping for selection rendering: the
    /// SCREEN coord of viewport row y is `viewport_top + y` for a single
    /// `viewport_top` (so one cheap viewport grid_ref gives the whole mapping —
    /// no expensive scrollbar read). Holds at the bottom and when scrolled.
    #[test]
    fn viewport_top_maps_screen_to_visible() {
        let mut t = GhosttyTerminal::new(20, 3, 1000).unwrap();
        t.write_vt(b"l0\r\nl1\r\nl2\r\nl3\r\nl4\r\nl5");

        let screen_of = |t: &GhosttyTerminal, y: u16| -> u32 {
            let r = t.viewport_grid_ref(0, y).unwrap();
            t.point_from_grid_ref(&r, vt::PointTag::SCREEN)
                .unwrap()
                .unwrap()
                .1
        };

        // At bottom: each viewport row's screen coord differs by exactly 1, i.e.
        // a single viewport_top + y mapping.
        let top = screen_of(&t, 0);
        assert_eq!(screen_of(&t, 1), top + 1);
        assert_eq!(screen_of(&t, 2), top + 2);

        // Scrolled up: the mapping still holds, viewport_top just decreased.
        t.scroll_viewport_delta(-2);
        let top2 = screen_of(&t, 0);
        assert!(top2 < top, "viewport_top decreased on scroll up");
        assert_eq!(screen_of(&t, 1), top2 + 1);
        assert_eq!(screen_of(&t, 2), top2 + 2);
    }

    #[test]
    fn resize_drag_does_not_accumulate_scrollback() {
        // DIAG (remove-crosswords resize-reflow bug). Simulate the user repro at
        // the engine layer: write content, shrink to the minimum, write more, then
        // "drag" the window by oscillating dimensions many times WITHOUT writing
        // any new content. The decisive question: does repeated resize alone grow
        // the engine's scrollback (sb_total) or visible rows? If it does, the
        // engine reflow is accumulating and the bug is upstream (libghostty-vt).
        let mut t = GhosttyTerminal::new(80, 24, 1000).unwrap();
        // Two `ls` runs worth of output.
        for i in 0..40 {
            t.write_vt(format!("file_{i:02}\r\n").as_bytes());
        }
        // Shrink to the minimum.
        t.resize(2, 1, 10, 20).unwrap();
        // Third `ls`.
        for i in 0..40 {
            t.write_vt(format!("f{i}\r\n").as_bytes());
        }
        let baseline = t.snapshot().unwrap().scrollbar().total;
        eprintln!("[resize-diag] baseline sb_total={baseline}");

        // Drag: oscillate the geometry, no new writes.
        let mut trace = Vec::new();
        for step in 0..30 {
            let cols = 2 + (step % 20) as u16 * 4;
            let rows = 1 + (step % 10) as u16 * 3;
            t.resize(cols.max(2), rows.max(1), 10, 20).unwrap();
            let snap = t.snapshot().unwrap();
            trace.push((cols, rows, snap.rows(), snap.scrollbar().total));
        }
        for (cols, rows, srows, total) in &trace {
            eprintln!(
                "[resize-diag] req cols={cols} rows={rows} -> snap.rows={srows} sb_total={total}"
            );
        }
        let final_total = trace.last().unwrap().3;
        // No new content was written during the drag, so total must not grow.
        // Reflow can legitimately re-wrap (total varies a little with width), but
        // it must not MONOTONICALLY accumulate. Flag gross growth.
        assert!(
            final_total <= baseline + 5,
            "engine scrollback grew under pure resize: baseline={baseline} final={final_total}"
        );
    }

    #[test]
    fn resize_reflow_does_not_duplicate_viewport_content() {
        // DIAG (remove-crosswords resize-reflow DUP). The bug: after resize drags
        // the viewport shows the SAME content twice. Write uniquely-tagged long
        // lines (wide `ls`-like rows that wrap when the window narrows), push some
        // into scrollback, then oscillate the geometry. Every visible tag (ROWnnn /
        // DIRnnn) must appear AT MOST once in the viewport — twice means the engine
        // reflow duplicated content into the visible region.
        let mut t = GhosttyTerminal::new(120, 40, 2000).unwrap();
        t.resize(120, 40, 10, 20).unwrap();
        for i in 0..60 {
            // ~106 cols — wraps at narrow widths.
            t.write_vt(format!("ROW{i:03} {}\r\n", "x".repeat(100)).as_bytes());
        }
        t.resize(40, 11, 10, 20).unwrap();
        for i in 0..40 {
            t.write_vt(format!("DIR{i:03}\r\n").as_bytes());
        }

        for step in 0..12 {
            let (cols, rows) = if step % 2 == 0 {
                (40u16, 11u16)
            } else {
                (110, 38)
            };
            t.resize(cols, rows, 10, 20).unwrap();
            let snap = t.snapshot().unwrap();
            let mut counts: std::collections::HashMap<String, usize> = Default::default();
            for y in 0..snap.rows() {
                for tok in line_text(&snap, y).split_whitespace() {
                    if (tok.starts_with("ROW") || tok.starts_with("DIR")) && tok.len() >= 6 {
                        *counts.entry(tok.to_string()).or_default() += 1;
                    }
                }
            }
            let mut dups: Vec<_> = counts
                .iter()
                .filter(|&(_, &n)| n > 1)
                .map(|(k, n)| format!("{k}×{n}"))
                .collect();
            dups.sort();
            eprintln!(
                "[reflow-dup] step {step} {cols}x{rows}: {} tags, dups={dups:?}",
                counts.len()
            );
            assert!(
                dups.is_empty(),
                "viewport duplicated content after resize to {cols}x{rows}: {dups:?}"
            );
        }
    }

    #[test]
    fn resize_shrink_does_not_double_full_width_padded_lines() {
        // Regression (remove-crosswords resize double-spacing / 错位). ConPTY pads
        // every line with trailing spaces out to the full console width. Without the
        // reflow trailing-space trim (vendored ghostty patch in
        // `libghostty-vt-sys/build.rs`), a column shrink wrapped that padding onto a
        // new row — each line became line+blank, ~doubling sb.total, which desynced
        // ConPTY's absolute cursor rows from the grid (input landed on history rows).
        // With the patch, padded lines must stay flat across a shrink, like plain
        // unpadded lines.
        let cols = 80u16;

        // Control: short lines, no trailing padding.
        let mut unpadded = GhosttyTerminal::new(cols, 24, 4000).unwrap();
        for i in 0..40 {
            unpadded.write_vt(format!("line{i:02}\r\n").as_bytes());
        }
        let unpadded_before = unpadded.snapshot().unwrap().scrollbar().total;
        unpadded.resize(cols - 2, 24, 10, 20).unwrap();
        let unpadded_after = unpadded.snapshot().unwrap().scrollbar().total;

        // Repro: every line padded with trailing spaces to the full width, then CRLF
        // (exactly what `Get-ChildItem`/`dir` output looks like through ConPTY).
        let mut padded = GhosttyTerminal::new(cols, 24, 4000).unwrap();
        for i in 0..40 {
            let body = format!("line{i:02}");
            let pad = cols as usize - body.len();
            padded.write_vt(format!("{body}{}\r\n", " ".repeat(pad)).as_bytes());
        }
        let padded_before = padded.snapshot().unwrap().scrollbar().total;
        padded.resize(cols - 2, 24, 10, 20).unwrap();
        let padded_after = padded.snapshot().unwrap().scrollbar().total;

        eprintln!(
            "[double-diag] unpadded {unpadded_before}->{unpadded_after} \
             padded {padded_before}->{padded_after}"
        );

        // Control must stay flat across the shrink.
        assert!(
            unpadded_after <= unpadded_before + 2,
            "unpadded lines should not grow on shrink: {unpadded_before}->{unpadded_after}"
        );
        // With the reflow trailing-space trim, the padded variant must also stay flat
        // (no line+blank doubling). Before the fix this was ~2× (41->81).
        assert!(
            padded_after <= padded_before + 2,
            "full-width padded lines must not bloat on shrink (reflow trailing-space \
             trim regressed?): {padded_before}->{padded_after}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn grapheme_cluster_2027_enabled_matches_conhost() {
        // The terminal enables mode 2027 (grapheme clustering) by default on Windows in `new()`
        // to match ConPTY's permanent Graphemes mode. A ZWJ family emoji must then
        // measure 2 cols (clustered), not 6 (per-codepoint) — otherwise the cursor
        // misaligns against ConPTY on any line with such a cluster (resize or not).
        let mut t = GhosttyTerminal::new(80, 24, 1000).unwrap();
        for _ in 0..26 {
            t.write_vt("\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}".as_bytes());
        }
        let snap = t.snapshot().unwrap();
        // 26 families × 2 cols = 52 → cursor stays on row 0 at col 52. Without
        // clustering it would be 26 × 6 = 156 cols and wrap to row 1.
        assert_eq!(
            snap.cursor().row.0,
            0,
            "clustered families (52 cols) fit one 80-col row"
        );
        assert_eq!(
            snap.cursor().col.0,
            52,
            "each ZWJ family advances 2 cols (mode 2027 on)"
        );
    }

    #[cfg(windows)]
    #[test]
    fn reflow_styled_trailing_matches_conhost() {
        // Difference 1 (conhost reflow alignment): conhost's MeasureRight trims trailing
        // spaces STYLE-BLIND; the 0001 patch now does too on Windows (dropped the
        // hasStyling guard). A line padded with bg-colored trailing spaces must stay
        // FLAT on a column shrink (like default padding), not wrap into blank rows.
        //
        // Repro of the bug for later work: this patch is Windows-gated, so on macOS/Linux
        // the styled variant still doubles; or `git revert` the styled-trim commit; or
        // run `reflow_styled_trailing_probe -- --ignored --nocapture` and compare
        // `styled` across platforms (41->41 on Windows, 41->81 elsewhere).
        let cols = 80u16;
        let build = |styled: bool| {
            let mut t = GhosttyTerminal::new(cols, 24, 8000).unwrap();
            for i in 0..40 {
                let body = format!("line{i:02}");
                let pad = " ".repeat(cols as usize - body.len());
                let line = if styled {
                    format!("{body}\x1b[41m{pad}\x1b[0m\r\n")
                } else {
                    format!("{body}{pad}\r\n")
                };
                t.write_vt(line.as_bytes());
            }
            let before = t.snapshot().unwrap().scrollbar().total;
            t.resize(40, 24, 10, 20).unwrap();
            (before, t.snapshot().unwrap().scrollbar().total)
        };
        let (_, default_after) = build(false);
        let (styled_before, styled_after) = build(true);
        assert!(
            default_after <= 43,
            "sanity: default-padded lines stay flat on shrink, got ->{default_after}"
        );
        assert!(
            styled_after <= styled_before + 2,
            "bg-colored trailing spaces must stay flat on shrink (0001 style-blind trim), \
             got {styled_before}->{styled_after} (81 means the hasStyling guard regressed)"
        );
    }

    #[cfg(windows)]
    #[test]
    fn resize_grow_preserves_cursor_row_on_windows() {
        // Regression for the scroll-after-resize 错位. conhost's ConPTY producer
        // (`SCREEN_INFORMATION::ResizeWithReflow`) preserves the cursor's offset
        // within the viewport on grow: the new rows appear as blanks BELOW the
        // prompt and the prompt stays "high". ghostty's default for a bottom cursor
        // is to "pull down" scrollback (the cursor pins to the new bottom), which
        // puts history where ConPTY expects blanks and smears ConPTY's viewport-
        // relative resize echoes onto a history row. The vendored Windows engine
        // preserves cursor y so Ghostty matches ConHost's cursor placement.
        let rows = 6u16;
        let mut t = GhosttyTerminal::new(40, rows, 4000).unwrap();
        // Fill past the viewport so there IS scrollback to (wrongly) pull down,
        // leaving the cursor on the bottom active row (no trailing newline → the
        // prompt sits at the bottom, like a shell after a command).
        for i in 0..12 {
            t.write_vt(format!("line{i:02}\r\n").as_bytes());
        }
        t.write_vt(b"PROMPT> ");

        let before = t.active_cursor_row().unwrap();
        assert_eq!(
            before,
            rows - 1,
            "cursor should start on the bottom active row"
        );

        // Grow the viewport (6 → 12 rows). With the patch the cursor row is
        // preserved (blanks below); without it ghostty pulls scrollback down and
        // pins the cursor to the new bottom (row 11).
        t.resize(40, 12, 10, 20).unwrap();
        let after = t.active_cursor_row().unwrap();
        assert_eq!(
            after, before,
            "grow must preserve the cursor's active row (conhost top-anchor); \
             got {after}, pull-down would be 11"
        );
    }

    #[test]
    fn snapshot_scrollbar_reflects_scrollback() {
        // 3-row viewport, write 6 lines → 3 rows in scrollback.
        let mut t = GhosttyTerminal::new(20, 3, 100).unwrap();
        t.write_vt(b"l0\r\nl1\r\nl2\r\nl3\r\nl4\r\nl5");
        let sb = t.snapshot().unwrap().scrollbar();
        assert_eq!(sb.len, 3, "len = visible rows");
        assert!(sb.total >= 6, "total includes scrollback, got {}", sb.total);
        // At the bottom the viewport sits at the end: offset = total - len.
        assert_eq!(sb.offset, sb.total - sb.len, "at bottom offset = total-len");
        // Scrolled to the top, the offset is 0 (top-anchored).
        t.scroll_viewport_top();
        assert_eq!(t.scrollbar().offset, 0, "at top offset = 0");
    }

    #[test]
    fn resize_grow_clamps_scroll_when_content_fits() {
        let mut t = GhosttyTerminal::new(20, 6, 100).unwrap();
        t.write_vt(b"l0\r\nl1\r\nl2\r\nl3\r\nl4\r\nl5");

        t.resize(20, 3, 10, 20).unwrap();
        let small = t.snapshot().unwrap().scrollbar();
        assert!(
            small.total > small.len,
            "precondition: small viewport scrolls"
        );

        t.resize(20, 8, 10, 20).unwrap();
        let snap = t.snapshot().unwrap();
        let grown = snap.scrollbar();
        assert!(
            grown.total <= grown.len,
            "grown viewport should not scroll when content fits: {grown:?}"
        );
        assert_eq!(line_text(&snap, 0), "l0");
        assert_eq!(line_text(&snap, 5), "l5");

        t.scroll_viewport_delta(1);
        assert_eq!(
            t.snapshot().unwrap().scrollbar(),
            grown,
            "scroll delta must no-op when content fits"
        );

        t.write_vt(b"\r\nl6\r\nl7\r\nl8");
        let overflow = t.snapshot().unwrap().scrollbar();
        assert!(
            overflow.total > overflow.len,
            "scrolling must return once content exceeds the viewport"
        );
    }

    #[test]
    fn scroll_viewport_shows_scrollback() {
        // 3-row viewport, write 6 lines so 3 scroll into history.
        let mut t = GhosttyTerminal::new(20, 3, 100).unwrap();
        t.write_vt(b"l0\r\nl1\r\nl2\r\nl3\r\nl4\r\nl5");
        // At the bottom: newest lines visible.
        let bottom = line_text(&t.snapshot().unwrap(), 0);
        assert!(
            !bottom.starts_with("l0"),
            "bottom shows newest, got {bottom:?}"
        );

        // Scroll up: older lines come into view.
        t.scroll_viewport_delta(-3);
        let scrolled = line_text(&t.snapshot().unwrap(), 0);
        assert!(
            scrolled.starts_with("l0"),
            "scrolled top shows l0, got {scrolled:?}"
        );

        // Back to bottom.
        t.scroll_viewport_bottom();
        let back = line_text(&t.snapshot().unwrap(), 0);
        assert_eq!(back, bottom, "scroll-to-bottom restores the view");
    }

    #[test]
    fn format_whole_screen_text() {
        let mut terminal = GhosttyTerminal::new(20, 3, 100).unwrap();
        terminal.write_vt(b"hello\r\nworld");
        let text = terminal.format_text(None, false, true).unwrap();
        assert!(text.contains("hello"), "got {text:?}");
        assert!(text.contains("world"), "got {text:?}");
    }

    #[test]
    fn format_screen_range_reaches_scrollback() {
        // 2 visible rows, scrollback. Push the first line into history, then
        // extract it by SCREEN coordinate (0,0)..(4,0) → "first".
        let mut terminal = GhosttyTerminal::new(20, 2, 100).unwrap();
        terminal.write_vt(b"first\r\nsecond\r\nthird");
        // "first" is now in scrollback (SCREEN row 0); the viewport shows
        // "second"/"third". A SCREEN-coord range still extracts it.
        let text = terminal
            .format_screen_range((0, 0), (4, 0), false, false, true)
            .unwrap();
        assert_eq!(text.trim_end(), "first", "got {text:?}");
    }

    #[test]
    fn viewport_grid_ref_resolves() {
        let mut terminal = GhosttyTerminal::new(20, 2, 100).unwrap();
        terminal.write_vt(b"x");
        // A valid viewport cell resolves to a non-null grid ref node.
        let r = terminal.viewport_grid_ref(0, 0).unwrap();
        assert!(!r.node.is_null());
    }

    #[test]
    fn red_sgr_sets_foreground() {
        let mut terminal = GhosttyTerminal::new(8, 1, 100).unwrap();
        terminal.write_vt(b"\x1b[31mR");

        let snapshot = terminal.snapshot().unwrap();
        let style = snapshot.style(snapshot.cell(0, 0).style_id());
        // Ghostty's default palette red (SGR 31), flattened through the palette.
        assert_eq!(
            style.fg,
            crate::render_buffer::to_ansi(Color {
                r: 204,
                g: 102,
                b: 102
            })
        );
    }

    #[test]
    fn rejects_zero_dimensions() {
        assert!(matches!(
            GhosttyTerminal::new(0, 24, 100),
            Err(Error::InvalidValue)
        ));
    }

    #[test]
    fn wide_cjk_char_occupies_two_columns() {
        let mut terminal = GhosttyTerminal::new(8, 1, 100).unwrap();
        terminal.write_vt("中A".as_bytes());

        let snapshot = terminal.snapshot().unwrap();
        // Wide ideograph in column 0, spacer (no text) in column 1, narrow in 2.
        assert_eq!(snapshot.cell(0, 0).c(), '中');
        assert_eq!(snapshot.cell(2, 0).c(), 'A');
    }

    #[test]
    fn mode_alt_screen_and_bracketed_paste() {
        let mut t = GhosttyTerminal::new(8, 3, 100).unwrap();
        assert!(!t.mode(mode::ALT_SCREEN));
        assert!(!t.mode(mode::BRACKETED_PASTE));
        t.write_vt(b"\x1b[?1049h\x1b[?2004h");
        assert!(t.mode(mode::ALT_SCREEN));
        assert!(t.mode(mode::BRACKETED_PASTE));
    }

    #[test]
    fn mode_sgr_mouse() {
        let mut t = GhosttyTerminal::new(8, 1, 100).unwrap();
        t.write_vt(b"\x1b[?1000h\x1b[?1006h");
        assert!(t.mode(mode::MOUSE_NORMAL));
        assert!(t.mode(mode::MOUSE_SGR));
    }

    #[test]
    fn shrink_resize_does_not_panic() {
        let mut t = GhosttyTerminal::new(80, 24, 1000).unwrap();
        for i in 0..200u32 {
            let line = format!("line {i} with some text that is fairly long to wrap\r\n");
            t.write_vt(line.as_bytes());
        }
        for (c, r) in [(60u16, 20u16), (40, 15), (20, 10), (5, 3), (1, 1), (80, 24)] {
            t.resize(c, r, 8, 16).unwrap();
            let _ = t.snapshot().unwrap();
        }
    }

    #[test]
    fn custom_palette_applied() {
        let mut terminal = GhosttyTerminal::new(8, 1, 100).unwrap();
        let mut palette = [[0u8; 3]; 256];
        palette[1] = [10, 20, 30]; // SGR 31 resolves to palette index 1.
        terminal.set_colors([255, 255, 255], [0, 0, 0], [255, 255, 255], &palette);
        terminal.write_vt(b"\x1b[31mR");
        let snapshot = terminal.snapshot().unwrap();
        let style = snapshot.style(snapshot.cell(0, 0).style_id());
        assert_eq!(
            style.fg,
            crate::render_buffer::to_ansi(Color {
                r: 10,
                g: 20,
                b: 30
            })
        );
    }

    #[test]
    fn write_pty_dsr_cursor_report() {
        let mut terminal = GhosttyTerminal::new(20, 5, 100).unwrap();
        // Move to row 3 col 4 (1-based 4;5) then request cursor position (DSR 6).
        terminal.write_vt(b"\x1b[4;5H\x1b[6n");
        let resp = terminal.take_pty_writes();
        assert_eq!(resp, b"\x1b[4;5R");
        // Draining is one-shot.
        assert!(terminal.take_pty_writes().is_empty());
    }

    #[test]
    fn write_pty_primary_da() {
        let mut terminal = GhosttyTerminal::new(20, 5, 100).unwrap();
        terminal.write_vt(b"\x1b[c");
        assert!(!terminal.take_pty_writes().is_empty());
    }

    #[test]
    fn bell_callback_counts() {
        let mut terminal = GhosttyTerminal::new(8, 1, 100).unwrap();
        terminal.write_vt(b"a\x07b\x07");
        assert_eq!(terminal.take_bell(), 2);
        assert_eq!(terminal.take_bell(), 0);
    }

    #[test]
    fn title_poll_reports_change_once() {
        let mut terminal = GhosttyTerminal::new(8, 1, 100).unwrap();
        terminal.write_vt(b"\x1b]2;hello\x07");
        assert_eq!(terminal.poll_title().as_deref(), Some("hello"));
        // No further change → None.
        assert_eq!(terminal.poll_title(), None);
    }

    /// The `PWD` getter is populated by both the manual `PWD` setter and
    /// by **OSC 7**. Upstream libghostty-vt dropped OSC 7 (an apprt action with no
    /// apprt in headless builds); the vendored patch
    /// `0003-pwd-store-osc7-headless.patch` routes `report_pwd` to
    /// `Terminal.setPwd`, mirroring `.window_title` so direct setters and OSC 7
    /// share the same observable state.
    #[test]
    fn pwd_set_via_setter_and_osc7() {
        // Setter → getter roundtrip works (the getter itself is fine).
        let t = GhosttyTerminal::new(8, 1, 100).unwrap();
        let p = b"/tmp/set";
        let s = vt::String {
            ptr: p.as_ptr(),
            len: p.len(),
        };
        let rc = unsafe {
            vt::ghostty_terminal_set(
                t.terminal,
                vt::TerminalOption::PWD,
                (&s as *const vt::String).cast(),
            )
        };
        assert_eq!(rc, vt::Result::SUCCESS);
        assert_eq!(
            t.get_string(vt::TerminalData::PWD),
            "/tmp/set",
            "the PWD setter populates the getter"
        );

        // OSC 7 populates the getter through report_pwd → setPwd.
        let mut t = GhosttyTerminal::new(8, 1, 100).unwrap();
        t.write_vt(b"\x1b]7;file:///home/u\x07");
        assert_eq!(
            t.get_string(vt::TerminalData::PWD),
            "file:///home/u",
            "OSC 7 populates PWD"
        );
    }

    /// The headless build must process OSC 133 marks written via `write_vt` into per-row
    /// SEMANTIC_PROMPT tags (OSC 7 needed a vendored patch for analogous plumbing).
    #[test]
    fn osc133_marks_tag_prompt_rows_headless() {
        let mut t = GhosttyTerminal::new(40, 4, 10_000).unwrap();
        // Row 0: prompt + echoed command. Row 1: command output. BEL-terminated,
        // matching the shipped pwsh integration.
        t.write_vt(
            b"\x1b]133;A\x07PS> \x1b]133;B\x07echo hi\r\n\x1b]133;C\x07hi\r\n\x1b]133;D;0\x07",
        );
        let tags = t.row_semantic_prompts().unwrap();
        assert_eq!(
            tags[0],
            vt::RowSemanticPrompt::PROMPT,
            "row 0 (prompt+command) must be tagged PROMPT; got {tags:?}"
        );
        assert_eq!(
            tags[1],
            vt::RowSemanticPrompt::NONE,
            "row 1 (output) must be untagged; got {tags:?}"
        );
    }

    /// Forwarded OSC 133 marks are zero-width state changes — they must not move the
    /// cursor or add lines.
    #[test]
    fn osc133_marks_do_not_move_the_cursor() {
        let mut t = GhosttyTerminal::new(40, 5, 10_000).unwrap();
        t.write_vt(b"out\r\n");
        let row_before = t.active_cursor_row();
        let cursor_before = t.snapshot().unwrap().cursor();
        // A full prompt-render mark burst as forwarding emits it (;D always,
        // plus ;A/;B/;C in waterfall).
        t.write_vt(b"\x1b]133;D;0\x07\x1b]133;A\x07\x1b]133;B\x07\x1b]133;C\x07");
        assert_eq!(t.active_cursor_row(), row_before, "no line added");
        let cursor_after = t.snapshot().unwrap().cursor();
        assert_eq!(
            (cursor_after.col.0, cursor_after.row.0),
            (cursor_before.col.0, cursor_before.row.0),
            "marks are zero-width"
        );
    }

    /// OSC 7 updates the tracked working directory in headless mode.
    #[test]
    fn pwd_poll_reports_change() {
        let mut terminal = GhosttyTerminal::new(8, 1, 100).unwrap();
        // Canonical OSC 7 with empty authority (`file:///path`).
        terminal.write_vt(b"\x1b]7;file:///home/u\x07");
        let pwd = terminal.poll_pwd().expect("pwd reported");
        assert!(pwd.contains("/home/u"), "unexpected pwd: {pwd:?}");
        // No further change → None.
        assert_eq!(terminal.poll_pwd(), None);
    }

    #[test]
    fn kitty_keyboard_flags_map_to_modes() {
        // The kitty keyboard protocol push (`CSI > flags u`) must surface in the
        // `Mode` facade so `session_key_flags` / the input path enable kitty press +
        // key-release encoding. This covers the gap where the flags lived
        // only in the engine's kitty stack and never reached vt_modes.
        use crate::terminal::Mode;
        let mut t = GhosttyTerminal::new(8, 1, 100).unwrap();
        assert!(
            t.kitty_keyboard_modes().is_empty(),
            "kitty protocol is inactive by default"
        );
        // Push disambiguate (1) + report-event-types (2).
        t.write_vt(b"\x1b[>3u");
        let m = t.kitty_keyboard_modes();
        assert!(m.contains(Mode::DISAMBIGUATE_ESC_CODES));
        assert!(m.contains(Mode::REPORT_EVENT_TYPES));
        assert!(!m.contains(Mode::REPORT_ALL_KEYS_AS_ESC));
        // Pop the flags → inactive again.
        t.write_vt(b"\x1b[<u");
        assert!(t.kitty_keyboard_modes().is_empty());
    }

    #[test]
    fn crlf_output_appears_on_successive_rows() {
        let mut terminal = GhosttyTerminal::new(48, 4, 100).unwrap();
        terminal.write_vt(
            b"C:\\Workspace\\NiumaTerm>echo NiumaTerm\r\nNiumaTerm\r\nC:\\Workspace\\NiumaTerm>",
        );

        let snapshot = terminal.snapshot().unwrap();
        assert_eq!(
            line_text(&snapshot, 0),
            "C:\\Workspace\\NiumaTerm>echo NiumaTerm"
        );
        assert_eq!(line_text(&snapshot, 1), "NiumaTerm");
        assert_eq!(line_text(&snapshot, 2), "C:\\Workspace\\NiumaTerm>");
    }

    // ---- block-split harvest primitives (read_screen_row / track_screen_row) ----

    fn row_read_text(row: &ScreenRowRead) -> String {
        row.cells.iter().map(|c| c.text.as_str()).collect()
    }

    /// Scrollback rows are readable without moving the viewport, and the soft-
    /// wrap flag marks the logical-line join point.
    #[test]
    fn read_screen_row_reaches_scrollback_with_wrap_flag() {
        let mut t = GhosttyTerminal::new(10, 3, 100).unwrap();
        t.write_vt(b"0123456789ABC\r\n"); // soft-wraps: "0123456789" + "ABC"
        for i in 0..6 {
            t.write_vt(format!("line{i}\r\n").as_bytes());
        }

        // Rows 0-1 are now in scrollback; the viewport must not move to read them.
        let offset_before = t.scrollbar().offset;
        let row0 = t.read_screen_row(0).unwrap().expect("scrollback row 0");
        let row1 = t.read_screen_row(1).unwrap().expect("scrollback row 1");
        assert_eq!(t.scrollbar().offset, offset_before, "viewport untouched");

        assert_eq!(row_read_text(&row0), "0123456789");
        assert!(row0.wrapped, "row 0 soft-wraps into row 1");
        assert_eq!(row_read_text(&row1), "ABC");
        assert!(!row1.wrapped, "row 1 ends the logical line");
    }

    /// OSC 133 `;A` surfaces as `prompt_start`; OSC 8 spans surface with URIs.
    #[test]
    fn read_screen_row_prompt_tag_and_hyperlinks() {
        let mut t = GhosttyTerminal::new(30, 4, 100).unwrap();
        t.write_vt(b"\x1b]133;A\x07PS> \r\n");
        t.write_vt(b"\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\ text");

        let prompt_row = t.read_screen_row(0).unwrap().expect("prompt row");
        assert!(prompt_row.prompt_start, "OSC 133;A row tagged");

        let link_row = t.read_screen_row(1).unwrap().expect("link row");
        assert!(!link_row.prompt_start);
        assert_eq!(
            link_row.hyperlinks,
            vec![(0u16, 3u16, "https://example.com".to_string())]
        );
    }

    #[test]
    fn read_screen_row_out_of_range_is_none() {
        let mut t = GhosttyTerminal::new(10, 3, 100).unwrap();
        t.write_vt(b"x");
        assert!(t.read_screen_row(9999).unwrap().is_none());
    }
}
