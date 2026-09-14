# ConPTY resize recovery and invisible PowerShell input

Updated: 2026-09-14.

Status: the unsafe recovery code has been removed. Its cursor and content
corruption mechanisms are demonstrated by replay tests. The original
intermittent profile-dependent incident has not been captured, so its precise
cause remains a hypothesis. Resize/input displacement remains reproducible
after the host fixes and the bundled ConPTY 1.25 upgrade. The checks below
refer to `f3562681` with PSReadLine 2.4.5; they do not establish a complete fix.

Decision: [ADR 0005](../adr/0005-conpty-output-preservation.md).
Review of the previous implementation:
[recovery review](conpty-realign-input-desync-review.md).

## Reported symptom and environment

Typing intermittently stops producing visible text in a NiumaTerm PowerShell
tab. Blindly entering `cls` can restore the display. Removing the PowerShell
profile reportedly prevents recurrence; the same profile reportedly works in
other terminals. Those observations motivate the investigation but do not
isolate a particular rendering operation.

The inspected environment used PSReadLine 2.4.5, PowerShell 7.6.6, and console
code page 936. The profile enabled history prediction with ListView, configured
Tab completion and RightArrow suggestion acceptance, set output encoding to
UTF-8, and loaded OpenSpec completion. NiumaTerm's injected shell integration
runs after the profile.

List prediction performs multi-row rendering. The exact ConPTY byte stream for
that rendering, and whether changing the output code page triggers the proposed
repaint in the original incident, require a capture. ED or CUP alone does not
identify a resize operation.

## Demonstrated failure mechanisms

The original recovery rewrote CUP rows during a resize window. ConPTY keeps
its own cursor and screen state and can emit incremental updates. If the
consumer changes an absolute cursor address, subsequent column-only output
continues on the consumer's changed row. A replay with the old rewrite helper
and the real Ghostty engine demonstrates this persistent mismatch. Recovery
requires a suitable later positioning operation or repaint; there is no
guarantee that the next key supplies one.

Commit `4179a802` removed CUP rewriting but retained synthetic SU/SD scrolls.
Keeping the cursor position unchanged did not make those scrolls safe:

- Splitting `ESC[H ESC[2J HEADER ESC[2;1H >` after `HEADER` let a later
  read scroll away content already repainted. Combining the reads produced
  different screen contents for the same stream.
- ED 1 clears only part of the screen. Treating it as a resize repaint and
  inserting SD discarded bottom text that the producer had not erased.
- A repaint needing zero adjustment left its anchor active. A later ordinary
  CUP then triggered a scroll using the earlier erase indication.
- The read-count/time condition did not impose a 150 ms deadline. An idle
  session could retain the adjustment opportunity much longer.

SU moves content upward; SD moves it downward and can discard bottom rows.
Neither guarantees preservation of active content or history. A repaint
classification based on ED, the last CUP, and read boundaries cannot establish
that an extra scroll is safe.

## Implemented correction

The PTY read path now passes the original bytes through the existing prompt
sniffer into Ghostty, and forwards the same bytes to output observers.
`ConptyResize` and the platform realignment helpers have been deleted. There
is no resize window, cursor-address rewrite, or synthetic SU/SD adjustment.

The implemented changes are:

| Commit | Behavior |
| --- | --- |
| `b133ae17` | Bundle ConPTY `1.25.260710002-preview`. |
| `e50b1473` | Preserve original output and retain opt-in raw VT diagnostics. |
| `cd6ff1c4` | Save tab and split-pane grids and restore a valid grid before shell startup. |
| `f473a100` | Report a pending flush until native input writes finish; wake on completion or failure. |
| `f3562681` | Preserve input/resize submission order, coalesce only adjacent unexecuted sizes, and keep replies and shutdown responsive. |

Existing native reflow patches remain in place. Missing or invalid saved grids
use the startup default, and a changed layout can still require a resize.
Ordering native submissions does not acknowledge producer processing or editor
readiness. PSReadLine is unmodified; the public redraw and 80 ms input-delay
experiments have not been added to the application.

## Current regression results

Validation on September 14, 2026 used bundled ConPTY
`1.25.260710002-preview` and unchanged PSReadLine 2.4.5:

| Coverage | Observed result |
| --- | --- |
| Terminal unit tests | 181 passed, including output preservation and ordered submission regressions. |
| Windows platform tests | 41 passed; three unrelated cases remained ignored. |
| Startup and persistence | Five terminal-view tests, seven local-state tests, and 247 application binary tests passed. |
| Regular native typing tests | Three passed: wrapped input, per-keystroke input across resizes, and startup ListView prediction. |
| Local-profile ListView case | Passed when selected explicitly. |
| History-viewport resize/input case | Intermittent displacement; retained as an opt-in reproducer. |
| Immediate shrink/grow/input case | Unresolved displacement; retained as an opt-in reproducer. |

Three output-preservation tests exercise every two-read split of an
erase/repaint stream, bottom text after partial erase, and later output after
a completed zero-adjustment repaint. All three failed before scroll recovery
was removed and pass afterward. Assertions cover screen content, history,
cursor rows, and unchanged observer output.

The history-viewport test waits for each requested engine size to be published
before continuing. That frame is not a ConPTY resize acknowledgment. The test
failed twice in five isolated runs with ordered submission, and once in 19
runs against `f473a100`, before that change. These small, timing-sensitive
samples show the earlier implementation also exhibits the symptom; they do
not determine whether the queue changes its frequency. Earlier passing runs
are not evidence that this scenario is reliable.

Both unresolved tests retain their alignment assertions. The history test also
checks that all 40 history markers remain exactly once in a full checkpoint
when execution reaches that check. A failure at the earlier alignment check
does not establish the outcome of the history assertions. The profile test is
opt-in because it loads machine-specific configuration. A full application
quit/reopen UI check and a reference-terminal UI comparison remain outstanding.

The repeat logs are local, ignored evidence under
`target/commit-split-20260914/`: `history-repeat.txt`,
`history-before-ordering.txt`, and `history-before-ordering-extended.txt`.

```powershell
cargo test --locked -p nmt_terminal --lib
cargo test --locked -p nmt_terminal --test conpty_typing -- --nocapture --test-threads=1
cargo test --locked -p nmt_terminal --test conpty_typing local_profile_list_prediction_survives_startup_resize -- --ignored --nocapture
cargo test --locked -p nmt_terminal --test conpty_typing resize_while_scrolled_preserves_history_and_live_input -- --ignored --nocapture
```

## Remaining resize/input displacement

Follow-up: a controlled experiment now identifies PSReadLine 2.4.5's 50 ms
suppression of resize checks as a sufficient cause of this reproduced
displacement. Both ConPTY 1.24 and 1.25 fail with the check suppressed and pass
with the check enabled. The accepted NiumaTerm solution must retain an
unmodified PSReadLine; the experiment does not establish that changing the
module is the only possible correction. See
[the follow-up investigation](conpty-resize-psreadline-render-check.md).
The observations below record how the remaining failure was first isolated.

A real session with 40 history lines starts at 80 columns by 24 rows, with its
prompt on row 24. Scroll to history, shrink to 60 by 20, wait for the engine's
new frame, then send a 100 by 30 resize immediately followed by input. The
prompt remains on row 20, but the echoed input appears on row 24. Row numbers
here are one-based.

The raw stream includes `ESC[24;6H` after both engine resizes. In the captured
run, the subsequent input redraws contain no ED and no cursor-position query.
The same test fails on the unmodified `488c329d1` implementation, with the
same screen and cursor. Removing synthetic scrolling did not introduce it.
Waiting for the final engine frame before sending input passed that early
comparison, as did deliberately separating resize operations during diagnosis.
The later history-viewport failures above show that a published frame is not a
reliable readiness signal.

The Microsoft resize implementation sends a packet through a separate signal
pipe and returns after writing it. This supports investigating ordering with
input processing, but does not by itself identify where the observed stale
row originated in the bundled implementation.
[Microsoft resize source](https://github.com/microsoft/terminal/blob/main/src/winconpty/winconpty.cpp)

That initial capture used ConPTY and OpenConsole file versions
`1.24.2607.10001`. The branch now bundles package `1.25.260710002-preview`,
whose pair reports `1.25.2607.10002`. Upstream `main` is supporting source
information, not an assertion that every detail matches either binary pair.

This is timing-sensitive: individual runs can pass. The executable reproducer
is deliberately ignored in normal runs because it can fail on the baseline.
Run it explicitly, repeating when needed, to investigate the outstanding
problem:

```powershell
cargo test --locked -p nmt_terminal --test conpty_typing immediate_input_after_shrink_and_grow_stays_with_prompt -- --ignored --nocapture
```

The follow-up records a successful ConPTY cursor synchronization followed by
PSReadLine positioning input back on the old row. An arbitrary host sleep, CUP
rewrite, or injected scroll would not correct the invalid cache assumption.

## Capturing the original incident

The VT trace now records complete bytes for every PTY output chunk, including
output after any resize, rather than a 240-byte prefix inside a recovery
window. Each entry includes sequence, timestamp, route, dimensions, byte count,
and resulting active cursor row. Resize trace points retain full viewport and
history snapshots. Backslashes are doubled; non-printable and non-ASCII bytes
are represented as `\xNN`, preserving split UTF-8 and control sequences.

For an isolated application instance:

```powershell
$env:NMT_VT_TRACE = '1'
$env:NMT_PROMPT_TRACE = '1'
$env:NMT_VT_TRACE_DIR = Join-Path $PWD 'target/resize-diagnosis'
$env:RUST_LOG = 'debug'
& .\target\debug\NiumaTerm.exe --testing
```

These diagnostics use synchronous file writes and can affect timing. They
record terminal output and resize state, not the complete keyboard, PTY-write,
terminal-response, and UI-publication timeline. A decisive capture may need
those additional observations around a controlled, non-sensitive test input.

If the original symptom recurs, record the operation and time and preserve the
logs before clearing the screen. Establish whether input was written, whether
ConPTY emitted its echo, where Ghostty placed it, and whether the published
viewport displayed that position. A successful blind `cls` supports a working
input path but does not uniquely establish the corruption mechanism. Likewise,
an interval without recurrence is supporting evidence rather than proof.
