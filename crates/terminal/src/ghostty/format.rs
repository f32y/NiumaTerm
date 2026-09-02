use std::{ptr, slice};

use libghostty_vt_sys::{
    Formatter as VtFormatter, FormatterFormat as VtFormatterFormat,
    FormatterTerminalExtra as VtFormatterTerminalExtra,
    FormatterTerminalOptions as VtFormatterTerminalOptions, PointTag as VtPointTag,
    Selection as VtSelection, Terminal as VtTerminal, ghostty_formatter_format_alloc,
    ghostty_formatter_free, ghostty_formatter_terminal_new, ghostty_free, sized as vt_sized,
};

use crate::ghostty::{Error, GhosttyTerminal, Result};

impl GhosttyTerminal {
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
}

/// Run the engine formatter over one terminal and take ownership of the
/// bytes it allocates. The formatter and its output buffer are separate FFI
/// allocations that have to be released whether or not the format succeeded,
/// which is why they never escape this function.
fn format_terminal(
    terminal: VtTerminal,
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
        ghostty_formatter_terminal_new(ptr::null(), &mut formatter, terminal, opts)
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
