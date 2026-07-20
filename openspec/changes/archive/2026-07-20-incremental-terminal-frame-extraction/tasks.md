## 1. Establish Baseline and Row Versions

- [x] 1.1 Record five-run release medians for the existing forced full-frame extraction profile before implementation. Baseline: extraction 34.295 µs; capture 30.262 µs.
- [x] 1.2 Add persistent monotonic visible-row versions to `GhosttyTerminal` and copy the complete version vector into every `RenderBuffer` capture.
- [x] 1.3 Map Ghostty false, partial, and full render damage to row-version updates, then clear both global and per-row damage after transfer.
- [x] 1.4 Add terminal tests for initial full damage, clean captures, partial row changes, global changes, resize, and cleared render-state damage.
- [x] 1.5 Add a skipped-publication regression test proving versions retain changes from multiple captures.

## 2. Reuse Immutable Line Data

- [x] 2.1 Move the existing immutable `TerminalLine` payload behind one `Arc` so cloning a reused line does not copy boxed cells or style runs.
- [x] 2.2 Retain row versions and per-row selection intervals in `TerminalFrame` for comparison with the next live frame.
- [x] 2.3 Extend frame extraction to accept an optional reusable frame and rebuild only rows whose dimensions, version, selection interval, or cursor input differ.
- [x] 2.4 Continue rebuilding current cursor, scrollbar, Kitty image, and other non-line frame metadata on every frame rebuild.
- [x] 2.5 Add pointer-identity tests for clean-row reuse and dirty-row replacement.
- [x] 2.6 Add regression tests for cursor-only movement and selection movement or removal when terminal row versions stay unchanged.

## 3. Integrate Cache Invalidation

- [x] 3.1 Pass the retained frame and reuse permission from `TerminalFrameCache` through `TerminalSurface` into extraction without changing render-buffer lock ordering.
- [x] 3.2 Add one-shot full visual invalidation that retains the displayed frame for mapping but forbids line reuse on the next rebuild.
- [x] 3.3 Route application theme and visual-setting changes to full invalidation while keeping terminal damage and selection on ordinary invalidation.
- [x] 3.4 Add cache tests for ordinary reuse, full rebuild, displayed-frame retention, and consumption of the full-invalidation flag.

## 4. Verify Correctness and Performance

- [x] 4.1 Run the terminal and app test suites covering Unicode graphemes, wide cells, styles, selection, cursor, scrollbar, and Kitty images. `nmt_terminal`: 299 passed; `app`: 204 passed, 2 ignored.
- [x] 4.2 Extend the release profiling harness with a representative one-row incremental-update scenario while retaining forced full extraction.
- [x] 4.3 Run each release profile five times and confirm full extraction stays within 10 percent of its pre-change median while incremental extraction is faster. Medians: full 34.512 µs (+0.63%); one-row 2.257 µs (-93.5% vs full).
- [x] 4.4 Launch `target\debug\NiumaTerm.exe --testing` and manually verify typing, cursor movement, selection, scrolling, resize, theme changes, and a full-screen terminal application. Automated smoke launch remained stable for 3 seconds; user interaction testing passed.
