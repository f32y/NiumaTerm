use std::str;

pub(super) fn is_conpty_resize_echo_input(bytes: &[u8]) -> bool {
    !bytes.is_empty()
        && bytes.len() <= 32
        && bytes.iter().all(|b| *b >= 0x20 && *b != 0x7f && *b != 0x1b)
}

/// Scan `bytes` for CSI sequences with digit/semicolon params — the one CSI
/// parse every ConPTY resize-echo helper shares. Calls `f(start, final_idx)`
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

pub(super) fn rewrite_conpty_resize_echo_cup_rows(
    bytes: &[u8],
    target_row_1based: u16,
) -> Option<Vec<u8>> {
    if target_row_1based == 0 {
        return None;
    }

    // ConPTY positions the cursor with the LAST CUP in the redraw. Align *that* row
    // to ghostty's cursor row and shift every CUP by the same delta, preserving the
    // multi-row structure of a wrapped input redraw. (Collapsing every CUP onto a
    // single row corrupts wrapped input: at a narrow width PSReadLine redraws the
    // input across several rows — e.g. `[10;22H…xxx…[11;6H` — and forcing the first
    // CUP to the cursor row makes the content re-wrap/scroll and accumulate.) When
    // ConPTY's cursor row already matches (delta 0), there is nothing to fix.
    let conpty_cursor_row = last_cup_row(bytes)?;

    let delta = target_row_1based as i32 - conpty_cursor_row as i32;

    if delta == 0 {
        return None;
    }

    let mut out = Vec::with_capacity(bytes.len().saturating_add(8));
    let mut copied = 0usize;

    for_each_csi(bytes, |start, fin| {
        if !is_cup(bytes[fin]) {
            return;
        }

        let params = &bytes[start + 2..fin];

        let (Some(row), _) = cup_row_col(params) else {
            return;
        };

        let new_row = (row as i32 + delta).max(1) as u16;

        if new_row == row {
            return;
        }

        let row_end = params
            .iter()
            .position(|b| *b == b';')
            .unwrap_or(params.len());

        out.extend_from_slice(&bytes[copied..start]);
        out.extend_from_slice(b"\x1b[");
        out.extend_from_slice(new_row.to_string().as_bytes());
        out.extend_from_slice(&params[row_end..]);

        out.push(bytes[fin]);

        copied = fin + 1;
    });

    if copied == 0 {
        return None;
    }

    out.extend_from_slice(&bytes[copied..]);

    Some(out)
}

/// The row of the LAST CUP (`CSI row;col H/f`) in `bytes` — ConPTY's resulting
/// cursor row after a redraw. `None` if there is no CUP with an explicit row.
fn last_cup_row(bytes: &[u8]) -> Option<u16> {
    let mut last = None;

    for_each_csi(bytes, |start, fin| {
        if is_cup(bytes[fin]) {
            if let (Some(r), _) = cup_row_col(&bytes[start + 2..fin]) {
                last = Some(r);
            }
        }
    });
    last
}

/// The maximum row and column (both 1-based) addressed by any CUP in `bytes`. Used to
/// sanity-check that a ConPTY repaint fits the latched resize size before SU
/// realignment. `(0, 0)` if there is no CUP.
pub(super) fn max_cup_row_col(bytes: &[u8]) -> (u16, u16) {
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

/// Rows to Scroll-Up to realign ghostty's prompt with ConPTY's, plus the
/// resulting prompt row (`R_conpty`, 0-based) for the post-SU assertion. `None` when the
/// repaint has no CUP, fails the resize correspondence pre-check, or `N <= 0`.
///
/// `r_ghostty` is the latched `active_cursor_row()` (0-based); `R_conpty` is the
/// repaint's last CUP row (0-based) — both the cursor row, kept anchor-matched.
pub(super) fn su_realign_count(
    repaint: &[u8],
    r_ghostty: u16,
    latched_cols: u16,
    latched_rows: u16,
    engine_cols: u16,
    engine_rows: u16,
) -> Option<(u16, u16)> {
    let r_conpty = last_cup_row(repaint)?.saturating_sub(1);

    let (max_row, max_col) = max_cup_row_col(repaint);

    let fits = engine_cols == latched_cols
        && engine_rows == latched_rows
        && max_row <= latched_rows
        && max_col <= latched_cols;

    if !fits || r_ghostty <= r_conpty {
        return None;
    }

    let n = (r_ghostty - r_conpty).min(r_ghostty);

    (n > 0).then_some((n, r_conpty))
}

fn first_cup_row(bytes: &[u8]) -> Option<u16> {
    let mut first = None;

    for_each_csi(bytes, |start, fin| {
        if first.is_none() && is_cup(bytes[fin]) {
            first = cup_row_col(&bytes[start + 2..fin]).0;
        }
    });

    first
}

fn contains_csi_erase_display(bytes: &[u8]) -> bool {
    let mut found = false;

    for_each_csi(bytes, |_, fin| found |= bytes[fin] == b'J');

    found
}

pub(super) fn is_conpty_resize_repaint(bytes: &[u8], target_row_1based: u16) -> bool {
    if target_row_1based == 0 || bytes.is_empty() || bytes.len() > 512 {
        return false;
    }

    if bytes.iter().any(|b| *b == b'\r' || *b == b'\n') {
        return false;
    }

    if !contains_csi_erase_display(bytes) {
        return false;
    }

    matches!(first_cup_row(bytes), Some(row) if row != target_row_1based)
}
