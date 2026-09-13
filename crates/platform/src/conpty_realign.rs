use std::str;

/// Scan `bytes` for CSI sequences with digit/semicolon params — the one CSI
/// parse every ConPTY resize helper shares. Calls `f(start, final_idx)`
/// per sequence: `params = &bytes[start + 2..final_idx]`, final byte =
/// `bytes[final_idx]`. Cold path (resize windows only).
fn for_each_csi(bytes: &[u8], mut f: impl FnMut(usize, usize)) {
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == 0x1b && bytes.get(i + 1) == Some(&b'[') {
            let mut fin = i + 2;

            while fin < bytes.len() && (bytes[fin].is_ascii_digit() || bytes[fin] == b';') {
                fin += 1;
            }

            if fin < bytes.len() {
                f(i, fin);
            }

            i = fin.saturating_add(1);
        } else {
            i += 1;
        }
    }
}

/// Parse `row;col` CUP params; either half is `None` when absent/unparsable.
fn cup_row_col(params: &[u8]) -> (Option<u16>, Option<u16>) {
    let parse = |s: &[u8]| str::from_utf8(s).ok().and_then(|s| s.parse().ok());

    match params.iter().position(|b| *b == b';') {
        Some(i) => (parse(&params[..i]), parse(&params[i + 1..])),
        None => (parse(params), None),
    }
}

fn is_cup(fin: u8) -> bool {
    fin == b'H' || fin == b'f'
}

/// The row of the LAST CUP (`CSI row;col H/f`) in `bytes` — ConPTY's resulting
/// cursor row after a redraw. `None` if there is no CUP with an explicit row.
fn last_cup_row(bytes: &[u8]) -> Option<u16> {
    let mut last = None;

    for_each_csi(bytes, |start, fin| {
        if is_cup(bytes[fin])
            && let (Some(r), _) = cup_row_col(&bytes[start + 2..fin])
        {
            last = Some(r);
        }
    });

    last
}

/// The maximum row and column (both 1-based) addressed by any CUP in `bytes`. Used to
/// sanity-check that a ConPTY repaint fits the latched resize size before
/// realignment. `(0, 0)` if there is no CUP.
pub fn max_cup_row_col(bytes: &[u8]) -> (u16, u16) {
    let (mut max_row, mut max_col) = (0u16, 0u16);

    for_each_csi(bytes, |start, fin| {
        if is_cup(bytes[fin]) {
            let (row, col) = cup_row_col(&bytes[start + 2..fin]);

            max_row = max_row.max(row.unwrap_or(0));
            max_col = max_col.max(col.unwrap_or(0));
        }
    });

    (max_row, max_col)
}

/// `true` when `bytes` carries an ED (`CSI … J`). ConPTY's post-resize repaint
/// erases from its first repainted cell to the end of the screen before
/// re-emitting the viewport, so an ED is what separates that repaint from the
/// other CUP-addressed output a shell produces in the same window: PSReadLine
/// redraws (EL per row, never ED) and keystroke echo (no erase at all). Acting
/// only on ED-bearing bursts keeps ordinary shell output from being mistaken
/// for a repaint.
pub fn contains_csi_erase_display(bytes: &[u8]) -> bool {
    let mut found = false;

    for_each_csi(bytes, |_, fin| found |= bytes[fin] == b'J');

    found
}

/// Rows to scroll so ghostty's prompt lands on the row ConPTY is repainting it
/// at, plus that row (`R_conpty`, 0-based). Positive means scroll up (SU),
/// negative means scroll down (SD). `None` when the repaint has no CUP, fails
/// the resize correspondence pre-check, or the rows already agree.
///
/// Only content moves: a scroll leaves the cursor where ConPTY's own model has
/// it, so ConPTY's later column-relative moves still land on the right row. Any
/// rewrite of the CUP rows themselves would desynchronise the two cursor models,
/// and ConPTY would then keep writing on the wrong row until its next absolute
/// reposition.
///
/// `r_ghostty` is the latched `active_cursor_row()` (0-based); `R_conpty` is the
/// repaint's last CUP row (0-based) — both the cursor row, kept anchor-matched.
pub fn realign_scroll_rows(
    repaint: &[u8],
    r_ghostty: u16,
    latched_cols: u16,
    latched_rows: u16,
    engine_cols: u16,
    engine_rows: u16,
) -> Option<(i32, u16)> {
    let r_conpty = last_cup_row(repaint)?.saturating_sub(1);

    let (max_row, max_col) = max_cup_row_col(repaint);

    let fits = engine_cols == latched_cols
        && engine_rows == latched_rows
        && max_row <= latched_rows
        && max_col <= latched_cols;

    if !fits {
        return None;
    }

    let delta = i32::from(r_ghostty) - i32::from(r_conpty);

    (delta != 0).then_some((delta, r_conpty))
}
