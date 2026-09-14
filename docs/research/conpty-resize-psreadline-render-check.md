# PSReadLine resize checks and persistent input displacement

Date: 2026-09-14.

Follow-up: [the complete-fix investigation](conpty-resize-complete-fix-research.md)
adds a 240-case producer/module comparison and editing-state probes. ConPTY 1.25
plus the public same-offset refresh passes the tested wrapped-input cases, but
an extra refresh key clears selection. Official PSReadLine 2.3.6 also fails a
separate rapid-input test. These results limit the earlier correction candidates;
none is a validated complete solution.

Status: a controlled experiment identifies elapsed-time suppression of resize
checks in PSReadLine 2.4.5 as a sufficient cause of the reproduced wrong-row
input. The branch now includes the host fixes through `f3562681` and bundles
ConPTY `1.25.260710002-preview`; PSReadLine remains unmodified. The native
resize/input reproducers remain unresolved. This result concerns those
reproducers; the original intermittent invisible-input incident remains
uncaptured.

Implementation constraint: the NiumaTerm solution must work with the user's
unmodified PSReadLine. Maintaining or shipping a modified PSReadLine module is
not an accepted solution for this project. The upstream source discussion
below explains the observed behavior; it is not a dependency adoption plan.

## Observed sequence

With 40 history lines, shrink an 80 by 24 session to 60 by 20, then grow it to
100 by 30 and immediately type `echo NMT_VISIBLE`. The prompt moves from row
24 to row 20, while the input can remain on row 24. These rows are one-based.

The original timing-sensitive run failed with the then-bundled ConPTY
`1.24.2607.10001`. An isolated copy of the identical test executable also
failed with NuGet package `1.25.260710002-preview`, whose DLL and OpenConsole
file versions are `1.25.2607.10002`. The latter package is now bundled by the
branch; the version distinction below describes the controlled comparison.

The newer producer does request a cursor position after resize. A captured
failure contains this sequence after both engine resizes:

```text
ConPTY output: ESC[6n       engine active row: 20
ConPTY output: ESC[20;6H   engine active row: 20
ConPTY output: ESC[?25l
ConPTY output: ESC[24;6H   engine active row: 24
ConPTY output: styled input text
```

NiumaTerm has already synchronized the producer's cursor before the later
wrong-row positioning. Updating ConPTY alone therefore does not resolve this
reproducer. The upstream cursor-query change and its response-handling follow-up
are [Microsoft #19535](https://github.com/microsoft/terminal/pull/19535) and
[#19620](https://github.com/microsoft/terminal/pull/19620).

## The PSReadLine condition

In `Render(bool force = false)`, PSReadLine disables
`_handlePotentialResizing` when fewer than 50 ms have elapsed since the previous
render. `RecomputeInitialCoords` then returns without updating the input
origin. This assumes a resize cannot matter within that interval.
[Render.cs at v2.4.5](https://github.com/PowerShell/PSReadLine/blob/v2.4.5/PSReadLine/Render.cs#L219)

The following render can combine new buffer dimensions with the old
`_initialY`, draw at the old row, and save those dimensions and the resulting
cursor position into `_previousRender`. A later check can find matching
dimensions and cursor coordinates and retain the displaced input origin.
Waiting for another key is consequently not a reliable recovery mechanism.
The same elapsed-time condition was also present in upstream `master` inspected
on the investigation date.

## Controlled experiment

The disposable harness first types and deletes one character, verifying that
PSReadLine is initialized and the input origin is established before resizing.
It then runs the same shrink/grow/input sequence. Both variants install the
same isolated handler for the first `e` key and call the normal `SelfInsert`.
The only changed value is PSReadLine's render stopwatch: the handler stops it
and sets elapsed ticks to either zero or one second before inserting the key.
This controls the existing branch without delaying input or changing VT bytes.

| Producer | Resize check suppressed | Resize check enabled |
| --- | --- | --- |
| ConPTY 1.24.2607.10001 | 10/10 displaced | 0/10 displaced |
| ConPTY 1.25.2607.10002 | 10/10 displaced | 0/10 displaced |

Every enabled-check run showed `NMT> echo NMT_VISIBLE` together on row 20 with
the cursor immediately after the input. Both producer versions used the same
test executable, Ghostty engine, shell, and PSReadLine 2.4.5 module. The only
binary difference between the producer directories was the ConPTY pair.

The reflection-based stopwatch control is an experiment, not a deployment
technique. Its source and logs are retained under the ignored directory
`target/conpty-cpr-probe/`: `initialized-render-check-matrix.rs`,
`v124-initialized-render-check.txt`, and
`v125-initialized-render-check.txt`. The newer producer's VT capture is in
`v125-trace/`. Temporary test code has been removed from the normal test file.

## Upstream correction candidate

The first source correction belongs in PSReadLine: remove the branch that
turns off resize handling solely because `elapsedMs < 50`. Preserve the
separate optimization that postpones an entire render while many keys are
queued. When a render actually proceeds, its input origin must be validated
before emitting cursor positioning and before recording the new render state.

Do not replace the time condition with a check of only the final width and
height. A shrink/grow sequence can return to the original dimensions while
changing the cursor's row. Any retained caching needs a reliable indication
that the geometry and cursor state remain valid.

Review initialization and render-state capture for consistency as part of the
source change. `_initialX`, `_initialY`, dimensions, and the previous render's
cursor should describe one consistent observation. A rendering pass that has
not checked its origin must not record new dimensions as if the old origin
had been validated. Reusing already-read state can reduce repeated console
queries, but the latency of those queries must be measured before optimizing.

This identifies an upstream correction candidate. It does not establish that
modifying PSReadLine is the only way NiumaTerm can address its observed behavior.

## NiumaTerm implementation boundary

The user requires compatibility with an unmodified PSReadLine module. A
private replacement module, a machine-wide replacement, or reflection changes
to PSReadLine internals are outside the accepted implementation scope.

The initial session size now follows the saved terminal grid. Previously,
`TerminalFrameSource::for_gpui` forced 100 columns by 30 rows before the first
layout submitted the actual content dimensions. Each terminal now saves its
accepted columns and rows in `grid_size`, including each split leaf. Restore
passes that grid to both the PTY and the engine before starting the shell.
Grid changes update the in-memory session even on an idle prompt; normal quit
persistence writes that session to disk.

When the restored layout has the same grid, the first layout does not submit
a resize. New panes and older snapshots without a saved grid still start at
100 by 30. Zero dimensions or dimensions beyond ConPTY's signed coordinate
range also use that fallback. A changed font, display, sidebar, or split layout
can still require a resize. This change removes an avoidable startup resize;
it does not resolve the already-active shrink/grow/input scenario above.

Startup validation covers saved single-pane and split grids through TOML
save/load, missing fields in older snapshots, and the GPUI resize notification
that keeps idle-pane dimensions current. A real PowerShell launched through
`TerminalPane::spawn` reported 132 columns by 43 rows before any layout ran;
the matching first layout submitted no resize. Missing, zero, and oversized
saved dimensions used 100 by 30. The five terminal-view tests and seven local
state tests passed. A full application quit/reopen UI check has not been run.

The PTY loop now retains pending commands in order and coalesces only adjacent,
unexecuted resize requests. A later resize waits for preceding input to leave
both the loop's write queue and the Windows writer's background buffer. The
Windows writer reports `WouldBlock` until its native pipe writes complete and
wakes the loop on completion or failure. While waiting, PTY output and generated
cursor replies continue to flow. Shutdown cancels the wait immediately. Input
released after a resize also wakes an otherwise idle loop.

The new regressions failed before this scheduling change: resize overtook
partially written input, separated resize requests overtook intervening input,
and the Windows writer reported a completed flush while its native pipe write
was still blocked. These regressions now pass, along with checks for cursor
replies during a pending resize, idle-loop progress, and native failure wakeups.

This orders submission to the native APIs. It does not establish that ConPTY
processed a request or PSReadLine invalidated its saved input origin. The
unmodified-shell shrink/grow/input reproducer passed one run after the change
but failed the first run of a subsequent repetition batch, again separating
the prompt and input. The batch stopped at that failure; it was not a completed
30-run success. Its log is `target/resize-scheduling-repro.txt`.

A fixed wait based on the observed 50 ms value cannot be treated as proof of
correct synchronization. Further work must compare the same scenario and
dependency versions with another terminal and preserve the failing native
reproducer as the acceptance check.

ConPTY's cursor-query improvements are included in the separately committed
1.25 preview upgrade. NiumaTerm continues answering actual cursor queries
through the engine and forwarding output unchanged. A host input delay, a resized UI frame, or
ConPTY's correct cursor position does not invalidate PSReadLine's cached input
origin by itself.

## Early public redraw API experiment

These initial public redraw probes used ConPTY 1.24. The later
[producer/module comparison](conpty-resize-complete-fix-research.md#native-version-matrix)
found different wrapped-input results under 1.25, while also demonstrating
that an extra refresh key clears selection. The table below records the
initial experiment, not a current cross-version guarantee.

An isolated F12 handler called the public `PSConsoleReadLine.InvokePrompt`
method immediately after shrink/grow and before the next input. No installed
module, user profile, or private PSReadLine field was changed. Both variants
were tested with empty input and with 90 existing characters that had wrapped.

| Invocation | Empty input | Existing wrapped input |
| --- | --- | --- |
| `InvokePrompt()` | Left two prompts on different rows | Duplicated prompt and input |
| `InvokePrompt($null, [Console]::CursorTop)` | Prompt and input aligned | Overwrote `HISTORY40` and left old input text |
| `GetBufferState` followed by `SetCursorPosition` at the same input offset | Prompt and input aligned | Overwrote part of `HISTORY40`, lost the prompt, and left old input text |

The explicit-row variant assumes that the cursor row is the prompt's starting row,
which is invalid for wrapped or multi-line editing. The default variant uses
the stale cached prompt origin. Neither is suitable as an automatic resize
recovery action. A separate same-offset cursor experiment reached PSReadLine's
coordinate recomputation through its public API but also corrupted the wrapped
case. Enabling a coordinate check is therefore insufficient evidence for all
resize scenarios. These are diagnostic observations, not passing regression
assertions or proof that every use of the public API is unsuitable.

Validation at `f3562681` passed 41 platform tests, 181 terminal unit tests,
three regular native typing tests, and the explicit local-profile prediction
test. First-party static checks passed. Repeated history-viewport testing
exposed intermittent displacement both before and after the input-ordering
change. That case is now opt-in alongside the immediate shrink/grow/input
reproducer, with all assertions retained. See the
[current regression results](conpty-realign-input-desync.md#current-regression-results)
for the observed counts and their limits.

The temporary test was removed from the normal suite. Its source and output
remain in `target/conpty-cpr-probe/public-prompt-redraw-probe.rs` and
`target/conpty-cpr-probe/public-prompt-redraw-probe.txt`. No automatic redraw
handler is installed by NiumaTerm. The same-offset variant is retained as
`public-cursor-offset-probe.rs` and `public-cursor-offset-probe.txt` in that
directory.

## Required validation before declaring a complete fix

- Run the original native resize/input reproducer with the user's unmodified
  PSReadLine, without the experimental stopwatch handler.
- Compare actual initial sizes, resize requests, input writes, output, and
  cursor positions with a reference terminal under the same scenario.
- Exercise startup, already-active editing, resizing back to the same size,
  wrapped input, prediction lists, and rapid repeated input/resize sequences.
- Verify history preservation, cursor placement, input completeness, and the
  behavior of replies while resize operations are still arriving.
- Measure interactive latency and console-query cost, including remote hosts.

The controlled results identify a contributing cache assumption. They do not
establish that a complete host-side fix has been found, or that every possible
mid-render resize is handled.
