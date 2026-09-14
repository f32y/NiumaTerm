# Review of ConPTY resize recovery and invisible input

Date: 2026-09-14

This report reviews `488c329d1`, not the current implementation. Source paths,
line numbers, and review recommendations below refer to that revision. Commit
`e50b1473` subsequently removed scroll injection and recovery state. Current
behavior and unresolved resize/input reproducers are recorded in
[the updated investigation](conpty-realign-input-desync.md).

The earlier recovery decisions have been removed from the document set. The
[current output-preservation decision](../adr/0005-conpty-output-preservation.md)
records the adopted behavior; historical source and decision text remain in Git.

Reviewed branch: `fix/conpty-realign-no-rewrite` at `488c329d1`.
Implementation change: `4179a802`.

Removing CUP row rewriting is justified: a replay using the old rewrite
function and the real Ghostty engine demonstrates a persistent row mismatch
under subsequent column-only output. This does not establish that the
PowerShell profile caused the reported intermittent failure through this path.
The original failing session has not been reproduced or captured during this
review.

The retained scrolling is not generally safe. The implementation adds SD,
keeps erase detection across reads, and retains the resize anchor until a
nonzero adjustment occurs. Replays expose content loss and incorrect screen
updates. The reviewed revision was not a complete, regression-free fix.

## Confirmed implementation findings

### P1: A later read can scroll away content already repainted

Locations: `crates/terminal/src/pty_pipe/conpty_resize.rs:95`,
`:109`; `crates/platform/src/conpty_realign.rs:45`.

The new `erase_seen` flag survives between reads, but each read is immediately
applied to the engine. This allows the scroll to occur in the middle of a
repaint, after some of the replacement screen is already visible.

Reproduction, with an 80 by 24 engine and the resize anchor on row 18:

```text
Read 1: ESC[H ESC[2J HEADER
Read 2: ESC[2;1H >
```

Spaces above separate tokens; they are not additional bytes.

`ESC[H` is a valid home operation, but `last_cup_row` only accepts explicit
row numbers. Read 1 therefore draws `HEADER` on row 1 without consuming the
anchor. Read 2 injects `ESC[16S` before drawing the prompt on row 2. The header
disappears from the active screen. Reading the identical bytes in one chunk
preserves the header, because the scroll precedes the clear and all repaint
text. Both cases end with the same cursor row, so cursor checks cannot detect
this screen corruption.

This is a new exposure from carrying both erase detection and the anchor into
later reads. The original implementation consumed the scroll anchor on the
first read. Adding support for omitted CUP parameters alone is insufficient:
read boundaries can also fall between the erase and the first CUP, or inside
a control sequence. A read is not a complete render operation.

Recommendation: do not prepend a content adjustment after part of the related
repaint has already been applied. Any retained recovery needs a defensible
boundary before mutation, incremental control-sequence parsing, and a safe
pass-through result when the boundary cannot be determined. Arbitrary buffering
until a CUP is seen would not establish where the repaint ends.

### P1: The newly added downward scroll can discard text

Locations: `crates/terminal/src/pty_pipe/conpty_resize.rs:102`;
`crates/platform/src/conpty_realign.rs:84`, `:126`.

The detector accepts any ED parameter. ED does not identify a resize, and ED 1
does not clear the entire screen. SD shifts content toward the bottom and can
discard rows; it does not preserve them in top scrollback.

Reproduction:

```text
Initial content: BOTTOM on row 22; prompt and anchor on row 2.
Read: ESC[1J ESC[5;1H new
```

With unchanged input, `BOTTOM` remains. With recovery, the read triggers
`ESC[3T`; `BOTTOM` is absent even from the engine's complete screen-plus-history
text. The ordinary partial erase did not authorize removing that lower row.

The original implementation did not inject SD. The then-current research
proposal described SU only, while the reviewed implementation added both
directions. Its decision text described misclassification as merely moving
rows into scrollback, which is incorrect for this demonstrated case. Keeping the cursor fixed does not keep
the screen contents synchronized with ConPTY's incremental renderer.

Recommendation: remove the SD extension from this fix unless preservation of
all displaced content can be established. Restricting ED parameters would
reduce some false matches, but cannot by itself prove that output is a resize
repaint or that a content shift is safe.

### P2: A successful zero-adjustment repaint leaves a stale anchor

Locations: `crates/platform/src/conpty_realign.rs:128`;
`crates/terminal/src/pty_pipe/conpty_resize.rs:97`, `:113`.

`realign_scroll_rows` returns `None` both when there is no usable target and
when the target already agrees with the anchor. Only a nonzero adjustment
consumes the anchor.

Reproduction:

```text
Anchor: row 18.
Read 1: ESC[18;1H ESC[J >
Read 2: ESC[19;1H result
```

The first read already agrees with the anchor. The second read contains no ED,
but inherits `erase_seen` and injects `ESC[1T`. A normal move to the next output
row is mistaken for outstanding resize recovery.

This follows from the new anchor lifetime. The existing EL-only ListView test
starts with `erase_seen = false`, so it does not cover ordinary output after an
ED-bearing read.

Recommendation: distinguish an unusable observation from a completed
zero-adjustment observation. Consume a successfully matched resize even when
no scroll is needed. This still depends on solving repaint identification;
simply consuming the first CUP would introduce different failures on split
repaints.

### P2: The 150 ms value is not a deadline

Location: `crates/terminal/src/pty_pipe/conpty_resize.rs:118`.

The window closes only when the read budget is exhausted AND elapsed time is
at least 150 ms. The check also happens after the possible injection. A replay
with a resize timestamp 60 seconds in the past still injects `ESC[16S` on an
ED/CUP read. Many reads in less than 150 ms also continue to be considered.

The combined count/time lifetime has an antecedent in the old implementation;
it is not wholly introduced by this change. Retaining an unused anchor and ED
across reads makes its effect on scroll injection more extensive. The new
comment saying an idle shell cannot keep the window open is misleading.

Recommendation: define separate time and work limits and check expiration
before inspecting output for a possible mutation. If a repaint arrives after
the deadline, pass it through. Choose limits using actual traces; 150 ms is a
policy choice, not evidence of repaint ownership.

## Assessment of the proposed cause

The basic cursor argument holds. The old helper changes a CUP target from row
18 to row 2 in the replay. Three later column-only echoes remain on row 2. An
unmodified absolute CUP to row 18 restores that row. Deleting rewriting
eliminates this particular source of mismatch.

The explanation of ConPTY movement is too absolute. Microsoft's historical
`XtermEngine::_MoveCursor` tracks the last position and uses CR, LF, CRLF,
backspace, forward-column movement, home, and absolute positioning depending
on the case. A changed row does not always cause CUP; CUP is also possible
without a row change. This supports the general mismatch mechanism but not a
guarantee of recovery on the next keystroke.
[Microsoft renderer source](https://github.com/microsoft/terminal/blob/ad362fc8663f43f9a63aac76a05dd1f227fe2246/src/renderer/vt/XtermEngine.cpp#L230)

That historical source is not treated as an exact match for the bundled
binary. The reviewed revision used OpenConsole and ConPTY file version
`1.24.2607.10001`; the causal claim needed output captured with that pair.
The later upgrade and its results are covered by the updated investigation.

PSReadLine 2.4.5 has multi-line prediction rendering and a list-clear path that
clears multiple lines. Its renderer also changes cursor positions explicitly.
These support investigating multi-row output, but do not establish the exact
ConPTY byte stream for the reported session.
[Prediction views](https://github.com/PowerShell/PSReadLine/blob/v2.4.5/PSReadLine/Prediction.Views.cs#L1085),
[PSReadLine rendering](https://github.com/PowerShell/PSReadLine/blob/v2.4.5/PSReadLine/Render.cs)

The research document first describes ListView output as multi-row CUP output,
then assumes that subsequent keystrokes stay on one row indefinitely. It needs
trace evidence for that transition. A wrong cursor row also does not alone
explain complete invisibility: the affected content must be outside the visible
area, erased, hidden by presentation logic, or otherwise not displayed.

The output-code-page repaint claim remains unverified here. Likewise, clearing
the screen successfully after blind typing supports a working input path but
does not uniquely identify the rendering defect. A day without recurrence is
supporting A/B evidence, not confirmation of a specific mechanism.

## Corrections identified in the reviewed research document

- Lines 65 and 161 reverse the SU direction. SU applies when Ghostty's row
  number is larger, placing its content lower than ConPTY's target.
- Lines 153-157 describe same-read ED detection with CR/LF rejection. The
  implementation removes that helper, accepts CR/LF, and retains ED across
  reads. Neither version proves that a resize repaint is being observed.
- The implemented SD extension and longer-lived anchor need explicit risk
  analysis. Content movement can damage active content and history even when
  cursor coordinates are unchanged.
- Lines 164-167 should promise no one-keystroke recovery bound. A future
  suitable absolute position or repaint is required; its timing is unknown.
- A scroll injection alone cannot confirm the CUP-rewrite explanation. The
  decision text needed to preserve uncertainty about the original
  profile-triggered event.
- The reproduction launch must include `--testing`.
- The reviewed implementation no longer logged original-versus-rewritten bytes.
  Its PTY trace records only the first 240 input bytes per read and only during
  the window. A decisive capture needs full input, any injected prefix, engine
  state before/after, and subsequent output after the window closes. Long
  redraws may put the final CUP beyond that trace limit.

## Validation and next steps

The existing targeted run passed all 10 tests:

```powershell
cargo test --locked -p nmt_terminal --lib conpty --target-dir C:\Workspace\NiumaTerm\target
```

Six additional review probes exercised the reviewed recovery source and
Ghostty engine, plus the old CUP rewrite function. One confirmed the old
mismatch mechanism; five checks of desired behavior failed, covering four
issue groups above. Two probes cover the same split-read failure from different
angles. The disposable harness uses its own dependency lock; the existing
10-test run used the repository lock.

The historical harness and captured output are in the ignored directory
`target/conpty-review/`, including `probes.rs` and `probe-results.txt`. The
command below requires the reviewed source revision; it no longer applies
directly after the recovery modules have been deleted. Permanent replacements
now live in `crates/terminal/src/pty_pipe/ghostty_mirror_tests.rs`.

```powershell
cargo test --offline --manifest-path target/conpty-review/Cargo.toml --target-dir C:\Workspace\NiumaTerm\target probes:: -- --nocapture
```

These are synthetic byte-stream replays, not a capture of the user's original
invisible-input incident. No application source was changed by this review.

The adopted correction removed the scrolling behavior rather than extending
its repaint heuristic. Permanent regressions cover the demonstrated corruption.
That correction establishes output preservation; the separate native
resize/input failures remain unresolved as described in the updated
investigation.
