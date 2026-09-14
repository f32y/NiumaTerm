# Preserve ConPTY output during resize

Status: accepted, 2026-09-14. Implemented in `e50b1473`.

This is the current resize-output decision. It replaces the earlier CUP
rewriting and synthetic-scroll strategies; their implementations remain
available in Git history. The replay evidence is recorded in the
[historical recovery review](../research/conpty-realign-input-desync-review.md).

## Context

ConPTY emits incremental output based on its own cursor and screen state.
Rewriting CUP rows makes later relative output land on a row the producer did
not select. Removing only coordinate rewriting is insufficient: inserting SU
or SD changes content that the producer may not repaint, even though the
cursor stays in place.

Tests against the real Ghostty parser demonstrate three failures in the
previous scrolling implementation. A split erase/repaint scrolls away text
already written. Partial erase followed by SD removes bottom content. A
zero-adjustment repaint leaves state that misclassifies later output. PTY
read boundaries and an ED/CUP pattern cannot reliably identify a complete
resize repaint or establish that a content adjustment is safe.

## Decision

Pass PTY output unchanged through the existing prompt processing and into the
terminal engine. Output observers receive the original bytes. Delete the
resize recovery state and its platform helpers. Do not rewrite cursor
addresses or insert content movement based on a guessed repaint.

Keep engine and PTY resizing and native reflow. Related host changes restore
saved grid dimensions before shell startup (`cd6ff1c4`), wait for native input
write completion (`f473a100`), and preserve input/resize submission order
(`f3562681`). Adjacent unexecuted resizes may coalesce; intervening input must
retain its place. Output, cursor replies, and shutdown remain active while
writes are pending.

Native write completion and a published engine grid do not acknowledge that
ConPTY processed the resize or that PSReadLine refreshed its input origin.
The bundled ConPTY upgrade in `b133ae17` does not remove that distinction.

Retain opt-in diagnostics for full PTY output bytes and resulting active
cursor rows, with route and size information, across the whole session. Full
viewport/history captures remain explicit events such as resize, avoiding
history formatting on every read.

## Consequences

Output no longer depends on a read-count/time window, split erase detection,
or a stale cursor anchor. Tests assert visible content and preserved history
as well as final cursor addresses. Existing tests for wrapped input, active
screen coordinates, native resize behavior, and terminal responses remain.

This change removes the demonstrated host-induced corruption. It does not
prove the cause of the original intermittent invisible-input incident, and
does not promise that every resize/input race is fixed. Both the immediate
shrink/grow/input case and the history-viewport case remain opt-in native
reproducers with their assertions intact. The history case also failed before
the input-ordering change; the observed samples do not establish whether that
change affects its frequency. The [current regression results](../research/conpty-realign-input-desync.md#current-regression-results)
separate passing coverage from these unresolved cases.

PSReadLine remains unmodified. Automatic redraw keys remain isolated
experiments. A subsequent compatibility setting, enabled by default, holds
user input for 80 ms after a changed native grid in local Windows PowerShell
sessions. Terminal output and generated replies continue during the pause.
The [compatibility implementation](../research/conpty-resize-complete-fix-research.md#application-compatibility-setting)
preserves submission order and bounds artificial waiting; it does not provide
producer synchronization or change this output-preservation decision.

The trace is diagnostic and performs synchronous file I/O when enabled. Its
timing and scope must be considered when interpreting a capture; it is not a
complete record of user input and presentation events.
