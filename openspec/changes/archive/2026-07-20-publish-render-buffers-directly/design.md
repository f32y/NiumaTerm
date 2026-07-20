## Context

The terminal crate currently exposes two owned representations of the visible viewport. `GhosttyTerminal::snapshot()` builds a sparse `TerminalSnapshot` containing allocated `SnapshotCell` strings and repeated snapshot styles. `RenderBuffer::update()` then clears a dense grid, interns those styles, splits grapheme extras, and copies frame metadata before the app can render or perform selection.

The PTY reader deliberately releases the Ghostty lock before taking the shared render-buffer lock, so rendering never waits behind VT parsing. Any replacement must retain this lock separation, publish only complete frames, and keep the dense `Square` grid used by `TerminalFrame` and selection.

## Goals / Non-Goals

**Goals:**

- Use `RenderBuffer` as the sole owned representation of a captured visible frame.
- Populate a reusable render buffer directly during Ghostty viewport traversal.
- Keep the shared-buffer lock limited to publication rather than frame construction.
- Preserve all rendered and selectable state, including resize and scroll refresh behavior.
- Remove the legacy snapshot model and conversion pass after all consumers migrate.
- Avoid a measurable regression in dense `TerminalFrame` extraction.

**Non-Goals:**

- Replacing the dense `Square` grid with a sparse representation.
- Rewriting selection, search, frozen-history rendering, or Ghostty's native VT behavior.
- Adding a trait, adapter layer, third buffer, dependency, or generalized frame abstraction.
- Changing terminal appearance, input behavior, or supported graphics protocols.

## Decisions

### `RenderBuffer` is the single published frame type

`RenderBuffer` will absorb the complete-frame role currently held by `TerminalSnapshot`. It already owns the representation consumed by rendering and selection, so retaining it preserves constant-time cell access, compact `Square` storage, style interning, and grapheme side storage.

The alternative was to delete `RenderBuffer` and teach all consumers to interpret sparse snapshot cells. That would move gap filling, style conversion, and grapheme handling into frame extraction and selection, reducing locality and risking slower dense terminal workloads.

### Ghostty fills an existing buffer directly

The hot-path interface will be:

```rust
GhosttyTerminal::snapshot_into(&mut RenderBuffer) -> Result<()>
```

After the Ghostty render state is updated successfully, this operation resets or resizes the target, walks viewport rows once, and writes cells and row metadata directly. Cell text is split into the base character and grapheme extras during that visit, and styles are interned immediately. No `Vec<SnapshotCell>` or per-cell snapshot `String` survives the visit.

`GhosttyTerminal::snapshot() -> Result<RenderBuffer>` remains as an allocating convenience for tests and diagnostic code. Production publication uses `snapshot_into()` with reusable storage.

The alternative was a generic visitor or sink trait. There is one producer and one representation, so that seam would be hypothetical and would expose nearly the same interface as the implementation.

### Complete frames are published with a two-buffer swap

The PTY state will retain one private back `RenderBuffer`. Ghostty fills it while holding only the engine lock. Image deltas are obtained from its completed Kitty placements, then the engine lock is released. The PTY thread briefly locks the shared front buffer and swaps it with the back buffer using `std::mem::swap`.

The previous front buffer becomes reusable storage for the next capture. A failed capture is never swapped or followed by `TerminalDamaged`. On success, publication completes before the damage event is sent.

The alternative was filling the shared render buffer directly. That would hold its lock across the full Ghostty traversal and make UI frame extraction wait on terminal readback.

### Every publication path uses the same complete-frame invariant

PTY output and resize use the reusable PTY back buffer. The less frequent synchronous scroll path may allocate a temporary render buffer, preserve the existing cursor-visibility rule, and then swap it into the shared buffer. Both paths publish a complete frame rather than mutating the front buffer incrementally.

### Consumers migrate before legacy types are removed

Trace utilities and tests will query `RenderBuffer` through existing getters. Only getters required by real consumers will be added. Once every caller has migrated, `TerminalSnapshot`, `SnapshotCell`, and `RenderBuffer::update(&TerminalSnapshot)` will be removed together.

Small value types used during Ghostty traversal, such as cursor, color, wide-cell, or normalized style data, may remain if they still express engine state. They do not constitute a second owned frame model.

## Risks / Trade-offs

- **A failed capture could leave partially written back-buffer data** → Never publish the back buffer unless `snapshot_into()` returns success; the next attempt resets it before filling.
- **Swapping buffers could make style identifiers alternate between two interners** → Each frame carries its matching style table, and both buffers retain their interners across alternating reuse.
- **Resize could leave stale cells or row metadata** → The reset operation resizes all dimension-dependent storage and clears cells, extras, row flags, placements, and metadata before visiting rows.
- **Scroll refresh could change cursor visibility** → Preserve the existing front-buffer cursor visibility override before publishing the refreshed frame.
- **Direct capture could accidentally diverge from old conversion semantics** → Port the existing conversion logic without redesign and retain focused tests for wide cells, graphemes, styles, row flags, cursor, colors, scrollbar, placements, and selection.
- **The refactor could regress frame extraction performance** → Keep the dense grid unchanged and compare five-run medians; investigate before submission if `TerminalFrame` extraction slows by more than 10%.

## Migration Plan

1. Record correctness and performance baselines for the current snapshot, conversion, and frame-extraction pipeline.
2. Add direct reusable capture while keeping current rendering semantics covered by tests.
3. Convert PTY, resize, and scroll publication to complete-buffer swaps.
4. Migrate trace utilities and tests to the render-buffer interface.
5. Delete the legacy snapshot types and conversion path.
6. Run full automated, performance, and isolated `--testing` validation.

The change is an internal workspace refactor and needs no persisted-data migration. Before merge, rollback is a normal commit revert because no external format or dependency changes.

## Open Questions

None. Allocation reuse, lock ordering, publication ordering, and the retained dense representation are fixed by the existing rendering constraints.
