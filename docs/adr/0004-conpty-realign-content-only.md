# ConPTY resize recovery moves content, never cursor coordinates

Status: accepted, 2026-09-13. Supersedes 0001.

After a resize, ConPTY repaints its viewport with absolute cursor positions
while ghostty reflows on its own, so the prompt can sit on different rows in
the two models. The recovery window from 0001 closed that gap in two ways:
scrolling the engine so the content rows met ConPTY's, and rewriting the row
in every CUP of a read so the bytes matched ghostty's cursor row.

The rewrite is unsound with ConPTY's renderer. conhost keeps its own model of
where the terminal cursor is and emits the smallest move that reaches the
next position: column moves while the row is unchanged, an absolute CUP only
when the row changes. A rewritten CUP puts ghostty's cursor on a row conhost
does not know about, and every later column-only move is applied on that
wrong row until the next absolute reposition. PSReadLine redraws the input
line in place on each keystroke, so a shell session can stay in that state
indefinitely: typed text lands on a row the user does not see. The trigger
was a PowerShell profile whose ListView prediction and code page change put
multi-CUP output inside the window, where the heuristics took it for a resize
repaint.

The recovery now has one action. Inside the window, once an ED has been seen
(ConPTY erases to the end of screen before repainting) and a read carries
CUPs that fit the latched size, the engine scrolls by the row difference
between ghostty's latched prompt row and the repaint's last CUP row: SU when
ghostty's prompt is lower, SD when it is higher. A scroll moves content and
leaves the cursor where conhost's model has it. The bytes from the PTY reach
the engine unchanged in every case. The anchor that holds the latched row
survives non-repaint reads, so a keystroke echo arriving before the repaint
cannot consume it, and it is spent by the one scroll a window may inject.

Accepted costs: typing during a drag resize may echo on ConPTY's stale row
until the repaint lands, and a clear or a primary-screen redraw carrying an ED
inside the window may trigger one scroll whose only effect is rows moving
into scrollback instead of being erased. The window's expected cursor is
traced, not asserted, because that second case makes a mismatch legitimate.

Implementation: `crates/terminal/src/pty_pipe/conpty_resize.rs`,
`crates/platform/src/conpty_realign.rs`. Background and diagnosis:
`docs/research/conpty-realign-input-desync.md`.
