## Why

Every terminal damage notification currently rebuilds every visible `TerminalLine`, even when typing or moving the cursor changes only one or two rows. Reusing unchanged rows can reduce frame-extraction work and shorten the time the UI holds the published render-buffer lock without changing terminal behavior.

## What Changes

- Track a monotonic content version for each visible Ghostty row and transfer those versions into every complete `RenderBuffer` capture.
- Consume and clear Ghostty render-state damage after transferring it into persistent row versions.
- Reuse immutable line data when a row's content version, selection interval, and cursor rendering inputs are unchanged.
- Force complete line reconstruction after resize, global Ghostty damage, or UI-side visual configuration changes such as a theme update.
- Preserve complete render-buffer capture, two-buffer publication, existing damage events, and per-frame cursor, image, scrollbar, and selection metadata.
- Add focused correctness and performance coverage for incremental and worst-case full-frame extraction.

## Capabilities

### New Capabilities

- `incremental-terminal-frame-extraction`: Reuses unchanged terminal line data while reliably rebuilding rows affected by terminal content, cursor, selection, dimensions, or visual configuration.

### Modified Capabilities

None.

## Impact

- Affects Ghostty render-state capture and damage reset in `crates/terminal`, plus `RenderBuffer` row metadata.
- Affects terminal frame representation, extraction, cache invalidation, tests, and profiling in `crates/app`.
- Builds on direct complete `RenderBuffer` publication; it does not change event payloads, lock ordering, external APIs, or add dependencies.
