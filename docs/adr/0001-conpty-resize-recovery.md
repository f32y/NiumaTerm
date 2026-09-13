# ConPTY resize recovery

Status: superseded by 0004, 2026-09-13. The echo correction described below
rewrote CUP rows and is gone; only the one-time scroll remains.

ConPTY can repaint at an old cursor position after a resize. The PTY loop
previously maintained the cursor anchor, echo timing, and repaint budget as
separate fields, making repeated scroll adjustments possible.

`ConptyResize` now owns an idle, armed, or repainting phase. An armed phase
contains the original cursor and dimensions. The first read consumes that
anchor, so later reads cannot repeat its scroll-up adjustment. Echo correction
accepts typing only within 150 ms of the resize; repaint detection examines at
most eight reads. Alternate-screen output ends repaint correction. Cursor
addresses use active-screen coordinates, regardless of the displayed viewport.

The helper returns rewritten input, any synthetic prefix, and the expected
cursor to the PTY loop. The loop retains responsibility for parsing and
publishing terminal output. Regression tests cover one-time realignment, late
typing, wrapped rows, and active-screen coordinates.

Implementation: `crates/terminal/src/pty_pipe/conpty_resize.rs`.
