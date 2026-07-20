## Why

Terminal frames are currently materialized twice: Ghostty first builds a sparse `TerminalSnapshot`, then `RenderBuffer::update` converts it into the dense representation consumed by selection and rendering. This duplicates frame models, allocations, and traversal while making correctness depend on keeping two representations synchronized.

## What Changes

- Make `RenderBuffer` the single owned, renderable snapshot of the visible terminal viewport.
- Add a reusable direct-capture path that lets Ghostty populate a `RenderBuffer` without constructing `TerminalSnapshot` or `SnapshotCell` values.
- Publish completed frames by swapping a private back buffer into the shared render buffer under a short lock.
- Preserve cursor, colors, row metadata, scrollbar state, Kitty graphics metadata, grapheme clusters, wide cells, styles, selection, and frame extraction behavior.
- Migrate trace utilities and tests to inspect `RenderBuffer` through its existing or minimal query interface.
- **BREAKING**: Remove the public `TerminalSnapshot` and `SnapshotCell` types and the `RenderBuffer::update(&TerminalSnapshot)` conversion path; `GhosttyTerminal::snapshot()` will return a `RenderBuffer`.

## Capabilities

### New Capabilities

- `terminal-render-publication`: Captures Ghostty viewport state directly into a complete `RenderBuffer` and atomically publishes it for rendering and selection.

### Modified Capabilities

None.

## Impact

- Affects `crates/terminal` snapshot extraction, render-buffer storage, PTY publication, resize/scroll refresh paths, trace utilities, and terminal tests.
- Affects `crates/app` terminal surface and frame tests that currently construct or update render buffers from terminal snapshots.
- Removes an internal workspace API but adds no dependency and changes no user-facing terminal behavior.
- Changes frame production performance characteristics by eliminating the intermediate cell vector and conversion pass while retaining dense frame extraction.
