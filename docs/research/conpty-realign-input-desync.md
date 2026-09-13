# ConPTY resize realignment vs. a PowerShell profile: the "cannot type" bug

Status: hypothesis with a confirmation plan; fix implemented. The mechanism
below explains every observation so far, but the decisive trace (a CUP rewrite
or an injected scroll-up immediately before the freeze) has not yet been
captured. The fix described at the end is implemented on branch
`fix/conpty-realign-no-rewrite` and recorded in `docs/adr/0004`.

Date: 2026-09-13

## Symptom

In a NiumaTerm PowerShell tab, typing intermittently stops producing visible
text. Keys are not rejected and the shell does not hang; the typed characters
simply never appear. The session stays in that state until something forces a
full repaint, for example `cls` typed blind.

## What is established

| Observation | How it was established |
| --- | --- |
| Only happens in NiumaTerm | Same shell and profile in other terminals never show it |
| Removing `$PROFILE` makes it go away | Manual A/B over several days |
| The profile has no blocking statement | Read in full, every line evaluated in isolation |
| PSReadLine 2.4.5, pwsh 7.6.6, console code page 936 | Inspected on the machine |
| NiumaTerm's shell integration runs after the profile | `-NoExit -EncodedCommand` injection in `crates/platform/src/windows/powershell.rs` |

The profile (`Microsoft.PowerShell_profile.ps1`):

```powershell
try {
    Set-PSReadLineOption -PredictionSource History
    Set-PSReadLineOption -PredictionViewStyle ListView
    Set-PSReadLineKeyHandler -Key Tab -Function MenuComplete
    Set-PSReadLineKeyHandler -Key RightArrow -Function AcceptSuggestion
} catch {}
$env:POSH_TRANSIENT = $true
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
. "$HOME\Documents\PowerShell\OpenSpecCompletion.ps1"
```

Two lines change what ConPTY emits during the first second of a session:

- `PredictionViewStyle ListView` turns every keystroke redraw from a
  single-line rewrite into a multi-row block addressed with several absolute
  cursor positions (CUP), erased row by row with EL.
- `[Console]::OutputEncoding = UTF8` changes the console output code page.
  conhost responds with a full-viewport repaint, which under ConPTY arrives
  as a burst of ED plus absolute CUPs that looks like a resize repaint.

Neither is a bug in the profile. They are ordinary outputs that NiumaTerm's
ConPTY recovery code was never written to expect.

## The NiumaTerm side

`crates/terminal/src/pty_pipe/conpty_resize.rs` and
`crates/platform/src/conpty_realign.rs` implement a recovery window that opens
on every PTY resize. Its purpose: after a resize, ConPTY reflows its own
buffer and repaints the viewport with absolute coordinates, while the ghostty
engine reflows differently, so the prompt row in the two models can differ.

Inside the window the code does two things to the bytes coming from ConPTY:

1. **Scroll-up injection.** On the first read that contains a CUP and fits
   the latched size, if ghostty's cursor row is above ConPTY's last CUP row by
   `n`, write `ESC[nS` into the engine so the content lines up
   (`su_realign_count`).
2. **CUP row rewriting.** For up to eight reads, or within 150 ms if the user
   typed a short printable string, shift every CUP row in the read by the
   delta between ConPTY's last CUP row and ghostty's cursor row
   (`rewrite_conpty_resize_echo_cup_rows`). The rewritten bytes, not the
   originals, are fed to ghostty.

Both decisions are heuristics over the byte stream. The window cannot tell a
resize repaint from any other burst of CUP-addressed output.

## Why the two together produce invisible input

The decisive detail is how ConPTY's renderer positions the cursor. It keeps
its own model of where the terminal's cursor is and emits the smallest move
that gets from there to the new position: column moves (CUF, CHA, CR) when
the row is unchanged, an absolute CUP only when the row changes.

Now trace one bad session:

1. NiumaTerm restores the last session. Panes lay out, the PTY is resized,
   the recovery window arms. pwsh is running the profile at the same moment.
2. Within the window a read arrives that is not a resize repaint: the code
   page repaint, or a ListView redraw if the user has already started typing.
   It contains CUPs and fits the size, so a rule fires.
3. If the CUP rewrite fires, ghostty is told the cursor is on row `r + delta`
   while ConPTY believes it is on row `r`. ghostty obeys the bytes it was
   given, so ghostty's cursor model is now different from ConPTY's.
4. From here on PSReadLine redraws the input line on every keystroke. Those
   redraws stay on one row, so ConPTY emits column-only moves. ghostty
   applies them on its own, wrong row. The echo lands on a row the user is
   not looking at, or on a row that the next list redraw erases.
5. Nothing re-synchronises the two models until ConPTY sends an absolute CUP
   to a different row. `cls` does that. So does a real resize. Typing does
   not.

This explains each observation:

- Only NiumaTerm: no other terminal rewrites ConPTY's coordinates.
- Gone without the profile: with a plain PSReadLine the only output in the
  window is the resize repaint, which is what the heuristics were tuned on.
- Intermittent: it needs the profile's output to land inside a 150 ms / eight
  read window that opens on a resize whose timing varies with session
  restore, layout, and animation.
- "Suspected ghostty": the visible fault is indeed inside ghostty's grid, but
  ghostty did exactly what it was told; the bytes were altered before it saw
  them.

The scroll-up injection alone does not cause this class of failure. `ESC[nS`
moves content but leaves the cursor where it is, so ConPTY's model stays
correct. Its misfire costs at most some shifted rows that the next full
repaint overwrites.

## How to confirm

1. Start NiumaTerm with `NMT_VT_TRACE=1`, `NMT_PROMPT_TRACE=1` and
   `RUST_LOG=debug`. The VT tracer (`crates/terminal/src/vt_trace.rs`) logs
   every read inside a recovery window with original and rewritten bytes and
   an engine snapshot to `target/logs/nmt-vt-trace.log`.
2. Use the full profile until the symptom appears.
3. In the log, look at the last entries before the freeze. The hypothesis is
   confirmed if a read was rewritten (or `[nS` injected) and the rewritten
   read is a ListView redraw or a code page repaint, not a resize repaint.
4. At the moment of the freeze, type `cls` blind and press Enter. A cleared
   screen proves the input path works and only the grid is misaligned.

Two cheaper experiments narrow the trigger without the trace:

- Keep only the PSReadLine block for a day, then only the encoding line.
- Make `on_resize` leave the phase `Idle` under ConPTY, rebuild, and use the
  full profile for a day. No recurrence confirms the window as the cause.

## Fix

Principle: ghostty's cursor row must never differ from ConPTY's belief about
it. Any transformation that changes a row number in ConPTY's output violates
that and produces a desync that ConPTY's incremental renderer will never
repair on its own.

Concretely:

1. Delete the CUP row rewrite: `rewrite_conpty_resize_echo_cup_rows`,
   `echo_pending`, `on_input`, `is_conpty_resize_echo_input`, and the
   `repaint_pending` branch that also rewrites. ghostty follows whatever
   absolute coordinates ConPTY sends.
2. Keep only content movement, the `ESC[nS` injection, as the alignment
   tool. It never touches the cursor.
3. Gate the injection on a provable repaint: the read must contain ED and no
   CR/LF (the test `is_conpty_resize_repaint` already encodes). ListView
   erases with EL and never sends ED. The code page repaint does contain ED,
   but it overwrites the whole viewport, so an extra scroll-up before it only
   pushes a few blank rows into scrollback.

After the change `on_read` has a single decision: inside the window, not on
the alternate screen, the read is a provable repaint, and ghostty's prompt row
is `n` rows above ConPTY's, then inject `ESC[nS`; otherwise pass the bytes
through unchanged.

Trade-off: typing during a drag resize may paint one echo on the wrong row.
PSReadLine redraws the whole line on the next keystroke and ConPTY emits an
absolute CUP whenever the row changes, so the glitch lasts one keystroke. A
one-keystroke cosmetic flash replaces a persistent loss of visible input.

## Regression tests to add

- Inside an armed window, feed a multi-row, multi-CUP read shaped like a
  ListView redraw (EL per row, no ED). Assert the bytes pass through
  unchanged, no `[nS` is injected, and ghostty's cursor row equals the last
  CUP's row.
- Inside an armed window, feed a genuine resize repaint (ED, no CR/LF, last
  CUP row below ghostty's cursor). Assert exactly one `[nS` injection and no
  rewrite.
- Feed a code page style repaint (ED plus full viewport content) after the
  first read of the window. Assert no rewrite; an injection is acceptable.

## Lessons

- A heuristic that edits a protocol stream must be safe against every output
  the peer can legally produce in that window, not just the case it was
  written for. Here the window assumed "only resize repaints and user echo"
  and a stock PSReadLine option broke the assumption.
- When a terminal host and its engine each keep a cursor model, any edit to
  coordinates in transit must be applied to both or to neither. Content
  moves are safe; coordinate moves are not.
- "Works in every other terminal, fails only in ours, and a user config
  toggles it" points at code in our pipe that interprets the stream rather
  than at the engine that renders it.
