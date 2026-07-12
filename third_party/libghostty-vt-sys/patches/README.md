# Vendored ghostty source patches

`build.rs` clones ghostty at the pinned `GHOSTTY_COMMIT` into `OUT_DIR` and applies
every `*.patch` in this directory (sorted by filename) with `git apply` before the
zig build. Bump `GHOSTTY_PATCH_VERSION` in `build.rs` whenever a patch changes so a
cached clone is re-fetched and re-patched.

Patches 0001–0002 are gated `comptime builtin.os.tag == .windows` (Windows/ConPTY-only
quirks; macOS/Linux stay byte-for-byte upstream for those).

Former patch 0003 (headless OSC 7 pwd storage) was retired at Ghostty `53bd14fe`:
upstream `StreamTerminal.Handler.reportPwd` now calls `Terminal.setPwd` itself.
`ghostty.rs::pwd_set_via_setter_and_osc7` and `pwd_poll_reports_change` guard the
behavior, so a future pin bump must keep those tests green rather than restore the patch.

When bumping `GHOSTTY_COMMIT`: re-apply each patch against the new checkout, resolve
any conflicts, regenerate the `.patch` (`git -C <ghostty-src> diff > ...`), and bump
`GHOSTTY_PATCH_VERSION`. A patch that no longer applies fails the build with a clear
message.

## 0001-win-reflow-trim-trailing-default-spaces.patch

`PageList.reflowRow` trims trailing cells only when `Cell.isEmpty()` (codepoint 0,
i.e. never written). ConPTY pads every line with **explicit** U+0020 space glyphs out
to the console width, which count as written content — so a column shrink wraps that
padding onto a new row and every line becomes line+blank (~doubling scrollback and
desyncing ConPTY's cursor rows from the grid). The patch extends the trailing trim to
also drop **all** trailing narrow U+0020 spaces on Windows — **style-blind**, matching
conhost's `ROW::MeasureRight` (`GetLastNonSpaceColumn`). (It originally kept bg-colored
trailing spaces via a `hasStyling()` guard, but that diverged from conhost: a
styled-padded line wrapped in ghostty while conhost trimmed it flat, re-opening the
resize residual. The guard was dropped; the tradeoff is that a colored trailing
background is trimmed on reflow — exactly as conhost/WT do.) The existing
cursor/tracked-pin handling already widens `cols_len` to cover a pin in the padding, so
a prompt cursor in the trailing run is preserved; only non-wrapped rows are trimmed, so
soft-wrapped content is untouched. Regression:
`ghostty.rs::reflow_styled_trailing_matches_conhost`. See `CONTEXT.md` → ConPTY resize.

## 0002-win-grow-preserve-cursor-y.patch

`PageList.resizeWithoutReflow`'s grow-rows branch, when the cursor is on the last row
(`cursor.y >= self.rows - 1`), "pulls down" scrollback — history fills the new rows
above and the cursor stays pinned to the new bottom. conhost's ConPTY producer does
the opposite: `SCREEN_INFORMATION::ResizeWithReflow`
(`microsoft/terminal` `src/host/screenInfo.cpp`) preserves the cursor's **offset
within the viewport**, so on grow the new rows appear as **blanks below** the prompt
and the prompt stays "high". Because rio reads ConPTY's viewport-relative resize
repaint/echoes, ghostty's pull-down puts history where ConPTY expects blanks and the
echo lands on a history row (the scroll-after-resize 错位). The patch forces the
already-present "preserve cursor y" path on Windows (`and builtin.os.tag != .windows`
on the bottom-cursor early-break), matching conhost. macOS/Linux keep upstream
pull-down behavior. See `docs/adr/0005` and `.scratch/remove-crosswords/
FINDINGS-conhost-anchor-2026-06-15.md`.

The patch also marks two upstream `Screen.zig` tests as skipped on Windows
(`resize (no reflow) more rows with scrollback cursor end` and `resize (no
reflow) more rows with soft wrapping`): both assert exactly the pull-down
behavior this patch disables, so they can never pass on a patched Windows
build. With the skips, `zig build test-lib-vt` on Windows is expected to
report 0 failures.

## 0005-per-block-grid-blockset.patch

Per-block grid P1 (`docs/per-block-grid-design.md` §2/§6): `BlockSet` (ordered
finished-screen registry with never-reused `(id, generation)` handles),
`Terminal.finishBlock` (O(1) primary-screen ownership move with
`ContinuationState` carry-over: SGR/DECSCUSR/charset/protected/kitty
keyboard/semantic prompt/saved cursor/in-flight kitty transmit), and the block
C ABI (`ghostty_terminal_finish_block` / `clear_blocks` / `remove_block` /
`block_count` / `block_at` / `block_row_count` / `block_cols` / `block_bytes` /
`block_grid_ref`). Readback rides the existing grid-ref readers. Not
OS-gated — pure additive engine feature. Developed on the `per-block-grid`
branch of a local fork checkout (`.worktrees/ghostty`); regenerate with
`git diff <patched-baseline>..per-block-grid` there.
