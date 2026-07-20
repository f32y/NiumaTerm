## 1. Baseline

- [x] 1.1 Run the current `nmt_terminal` and `app` test suites, including the scrolled `pipelines.universal` word-selection regression.
- [x] 1.2 Record five-run median timings for snapshot creation, `RenderBuffer::update`, their combined publication work, and full `TerminalFrame` extraction.

## 2. Direct Render-Buffer Capture

- [x] 2.1 Add the minimal crate-private reset and cell/metadata writing operations needed to reuse a `RenderBuffer` without exposing a second frame interface.
- [x] 2.2 Implement `GhosttyTerminal::snapshot_into(&mut RenderBuffer)` as a single viewport traversal that preserves cells, styles, grapheme extras, row flags, cursor, colors, scrollbar, and Kitty placements.
- [x] 2.3 Change the allocating `GhosttyTerminal::snapshot()` convenience method to return a populated `RenderBuffer` and update focused capture tests for reuse, resize, Unicode, style, and metadata behavior.

## 3. Complete-Frame Publication

- [x] 3.1 Add one reusable back `RenderBuffer` to the PTY state and swap it into the shared front buffer only after successful direct capture.
- [x] 3.2 Extract Kitty image deltas from the completed back buffer, preserve the existing lock order, and emit `TerminalDamaged` only after publication.
- [x] 3.3 Convert resize and synchronous scroll refresh paths to publish complete render buffers, retaining the scroll path's cursor-visibility behavior.
- [x] 3.4 Add or adapt one runnable publication check proving failed capture cannot replace the front buffer or emit damage for an incomplete frame.

## 4. Legacy Model Removal

- [x] 4.1 Migrate trace utilities, selection/search/vi-motion tests, ConPTY tests, and app frame tests to query `RenderBuffer` directly.
- [x] 4.2 Remove `TerminalSnapshot`, `SnapshotCell`, `RenderBuffer::update(&TerminalSnapshot)`, and helpers used only by the intermediate conversion path.
- [x] 4.3 Verify repository searches contain no remaining legacy snapshot type or update-path references.

## 5. Verification

- [x] 5.1 Run `cargo fmt --check`, `cargo test -p nmt_terminal`, and `cargo test -p app`.
- [x] 5.2 Repeat the five-run performance measurements and confirm full-frame extraction remains within 10 percent of baseline while comparing total capture/publication cost.
- [x] 5.3 Launch `target\debug\NiumaTerm.exe --testing` and verify normal shell output, scrolling, resize, Codex full-screen rendering, Unicode/style rendering, and double-click copy of `pipelines.universal`.
- [x] 5.4 Review the final diff for unrelated abstractions or cleanup, then create one focused `refactor(terminal): publish render buffers directly` commit with verification details and the Codex co-author trailer.
