## Context

`GhosttyTerminal::snapshot_into` currently asks Ghostty to update its render state and then captures the complete visible viewport into a reusable `RenderBuffer`. The PTY path swaps that complete buffer into a shared front buffer. `TerminalFrame::from_render_buffer_with_selection` later holds the front-buffer lock while rebuilding every visible `TerminalLine`, including its text, cell data, and style runs.

Complete capture and two-buffer publication are valuable invariants: the UI always receives a self-contained buffer, and lock ordering stays simple. The avoidable cost is the second traversal in frame extraction. Interactive input usually changes one row, while the current extractor allocates line data for every row.

Ghostty exposes `false`, `partial`, and `full` render damage plus dirty state on individual rows. Those flags describe the current update and must be cleared by the caller after consumption. A UI damage event can be coalesced, so transient booleans cannot safely describe changes relative to the UI's last extracted frame.

## Goals / Non-Goals

**Goals:**

- Make small terminal updates rebuild only affected line data.
- Preserve correctness when the UI skips or coalesces published buffers.
- Account for cursor, selection, and application theme inputs that are not fully represented by Ghostty row damage.
- Keep worst-case full extraction close to its current cost and reduce render-buffer lock hold time for small updates.

**Non-Goals:**

- Incrementally copy Ghostty cells into `RenderBuffer`.
- Change the complete-buffer swap, lock ordering, or terminal damage event payload.
- Add a third shared buffer or revive the unused `TerminalDamage` model.
- Cache scrollbar or Kitty image extraction.

## Decisions

### Persist a monotonic version per visible row

`GhosttyTerminal` will own a row-version vector and a monotonic content revision. After `ghostty_render_state_update`:

- full damage assigns a new revision to every visible row;
- partial damage assigns a new revision only to dirty rows;
- no damage leaves every revision unchanged.

The implementation then clears both the per-row dirty flags and the global render-state damage. Every complete `RenderBuffer` capture receives a complete copy of the current row versions.

Versions describe the last content change rather than damage in one publication. If row 0 changes in an intermediate buffer and row 1 changes in the latest buffer, both versions differ from a frame built before either update. This remains correct when the UI observes only the latest buffer.

Alternatives considered:

- Publishing per-capture dirty booleans is smaller but loses changes when publications are skipped.
- Accumulating dirty bits in the shared front buffer couples producer state to UI consumption and complicates the existing swap protocol.
- Comparing or hashing every captured cell repeats much of the work this change is intended to avoid.

### Keep `RenderBuffer` complete

`snapshot_into` will continue to reset and repopulate all visible cells, styles, extras, row metadata, cursor, colors, scrollbar data, and graphics placements. Row versions are additional capture metadata only.

This keeps the producer-side change narrow and retains current recovery behavior after resize, scroll, or full-screen application updates. Incremental Ghostty-to-buffer copying can be evaluated separately if profiling later shows capture, rather than extraction, is the limiting cost.

### Share immutable line payloads

`TerminalLine` will become a cheap clone around `Arc<TerminalLineData>`. The data object retains the existing `SharedString`, content hash, boxed cells, boxed style runs, and test-only cursor column. Newly extracted rows keep the current compact boxed allocations; reused rows clone only an `Arc`.

`TerminalFrame` will retain row versions and row selection intervals aligned with its lines. On rebuild, a line is reusable only when:

1. reuse has not been explicitly disabled;
2. the old and new dimensions match;
3. the row version matches;
4. the result of `row_selection_for` matches; and
5. `cursor_for_row` produces the same old and new cursor input.

The extractor still performs an O(rows) comparison pass. It performs the O(cols) cell/style traversal and allocations only for rows that fail these checks.

Keeping versions and selection intervals on `TerminalFrame` leaves `TerminalLine` usable for frozen command blocks without inventing source versions for non-live lines.

Alternatives considered:

- Cloning the current `TerminalLine` would deep-copy its boxed cells and runs, eliminating most of the benefit.
- Storing the source version inside `TerminalLineData` mixes live-buffer bookkeeping into frozen line data.
- Reusing by `text_hash` happens after extraction and cannot avoid the expensive row traversal.

### Compare cursor and selection independently from Ghostty damage

Cursor rendering is baked into line cells and style output. Ghostty intentionally permits cursor-only operations that do not dirty terminal row content, so the old and new cursor inputs must be compared independently. A cursor move rebuilds its old and new rows; a shape, color, visibility, or column change rebuilds the affected row even if its content version is unchanged.

Selection is application state. The extractor computes the current per-row selection interval with the existing `row_selection_for` helper and compares it with the previous frame. This rebuilds only rows where highlighting was added, moved, or removed.

### Add one-shot full invalidation to the frame cache

`TerminalFrameCache::invalidate()` will continue to retain the displayed frame and mark it stale, while allowing eligible row reuse during the next rebuild. A new `invalidate_full()` path will retain the same frame for pointer and IME mapping but prevent line reuse for one rebuild.

The application-settings observer will use full invalidation because config theme colors influence extracted line colors without changing Ghostty row versions. Ordinary PTY damage, input, selection, and scrolling invalidations may use row comparisons. Dimension mismatch and Ghostty full damage independently make all rows fail reuse.

The full-invalidation flag is cleared only after a replacement frame is installed.

### Rebuild non-line frame metadata every time

Cursor metadata, scrollbar state, and Kitty image descriptors will continue to be created from the current buffer and generation map on each frame rebuild. The no-images fast path remains. Line reuse therefore does not make non-line state stale and does not introduce another cache invalidation protocol.

### Measure both incremental and worst-case paths

The existing extraction profile will retain a forced full-rebuild scenario, and a one-row mutation scenario will be added for incremental extraction. Five-run release medians will be compared on the same machine. Full extraction may add an O(rows) comparison and `Arc` operations, so its median must remain within 10 percent of the pre-change baseline; the incremental scenario must be faster than forced full extraction.

## Risks / Trade-offs

- [Ghostty dirty state is read or cleared incorrectly] → Add terminal-level tests for initial, clean, partial, full, and post-clear captures against the pinned Ghostty revision.
- [A rendering input is omitted from the reuse key] → Keep the key limited and explicit: dimensions, row version, row selection, cursor input, plus one-shot full invalidation for UI-owned visual configuration.
- [UI coalescing hides an intermediate change] → Persist last-change versions per row and copy the full version vector into every published buffer.
- [Full-screen workloads gain overhead] → Keep the comparison pass allocation-free, retain boxed data for rebuilt rows, and enforce the full-rebuild regression bound.
- [`Arc` line payloads increase indirection] → Use one wrapper only around the existing immutable payload and validate both incremental and full profiles before accepting the change.

## Migration Plan

1. Add row-version tracking and damage reset to `GhosttyTerminal`, then expose the captured versions through `RenderBuffer`.
2. Make terminal lines cheaply shareable and add previous-frame-aware extraction with cursor and selection comparisons.
3. Add ordinary versus full cache invalidation and route application visual-setting changes to the full path.
4. Run correctness tests, release profiles, and an isolated manual launch with `target\debug\NiumaTerm.exe --testing`.

The change is internal and needs no persisted-data migration. It can be rolled back by restoring unconditional line extraction; complete `RenderBuffer` capture and publication remain unchanged throughout.

## Open Questions

None.
