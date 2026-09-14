# Code governance audit resolution

Date: 2026-09-14

This records the review of [the audit](code-governance-audit.md) against the
current implementation. All remote-related findings are excluded for this
round. The original audit and unrelated readability-tool changes are preserved.

## Confirmed issues addressed

| Area | Result |
| --- | --- |
| Session event tests | Removed the pane's duplicate event, ready-default, and replay handlers. Session, question, branch, input-history, and restore tests deliver events through `AgentSession`, including its subscriptions. Remaining pane fixtures live in the tests module. |
| Child transcripts | Removed the unused transcript accumulator and its separate update fold. Tests now exercise the `ChildTranscript` used by the session controller, including completion merging, retention, replacement, and unavailable reads. |
| Team summaries | Oversized public context now pauses with an explicit error before reserving budget or starting an attempt. Removed the unreachable summary preparation/completion path. Persisted summary and attempt types remain readable. Automatic summary execution is still unavailable. |
| Terminal gutter | Removed selection state that could only be set behind a constant-false branch, its copy/rerun actions and keybindings, and its highlight data. Text selection, command headers, and block navigation remain available. |
| Dead APIs | Removed the retired selection rotation/dimension chain, old visible-row helper, duplicate shutdown/title aliases, unused workflow/child revision counters, unused registry accessors, and redundant history/settings helpers. Removed test-only style-table, hex-conversion, preferred-shell, and version-channel APIs. |
| Test quality | Removed repeated assertions and test-local comparisons. Attachment fixtures now contain distinguishable images. Prompt-trust tests first establish trust, then send an invalid transition. Question tests exercise actual editor changes and verify that expired answers do not populate a new editor. Close-confirmation tests cover every setting and unknown process counts. |
| Startup configuration | Only a missing file selects defaults. Read failures, invalid UTF-8, and parse failures return an error. A repeated global initialization fails before changing the active palette. |
| Attachments | Directory creation and every image write must succeed before a Codex message can be accepted. A failed image write no longer sends an incomplete attachment list. |
| Git and update state | Git branch and change-count failures remain errors. Branch watchers retain the error separately from a missing repository. Update-cache writes report failures and replace the file through the platform implementation. |
| Hook and model state | Invalid hook settings appear unavailable and disable the unsafe toggle. Failed model-catalog refreshes produce a refusal and diagnostic. Invalid historical timestamps remain unknown. Hook CLI failures emit bounded diagnostics without the payload. |
| Terminal reads | Row, cell, color, cursor, formatter, and screen-page errors propagate instead of producing a partial successful result. Failed captures retain the previous frame and log once per consecutive failure run. The documented absence of an optional terminal color remains an allowed state. |
| Native process management | Local close decisions retain process-query errors and request generic confirmation when the setting requires it. Child cleanup reports failures without waiting indefinitely after a failed kill. macOS enumeration reads actual group members and respects the native count units. |
| Windows integration | Registry deletion ignores missing keys but reports other failures. Explorer command invocation reports lookup and launch failures while preserving the default-directory behavior for an absent selection. |
| Update rollback | Replacement errors include restoration failures. Incomplete recovery keeps incoming and previous copies; startup cleanup preserves backups when incoming files remain or a target is missing. |
| Native notifications | Linux delivery/removal errors propagate and native IDs are tracked. macOS waits for authorization, reports asynchronous failures, and uses unique IDs plus cancellation state to prevent dismissed requests from reappearing. |
| Platform boundaries | PTY hangup rules, configured shell defaults, interactive shell arguments, durable replacement, history path spelling, and native window-activity lookup reside in the platform crate. Dynamic-library names use Rust's platform constants. macOS key normalization occurs in the terminal adapter. |

## Findings retained or corrected after review

- The room attachment writer is used by storage fixtures. A missing upload UI
  does not justify deleting the persisted attachment reader.
- `SessionController::apply_replay` already records the age of restored replies.
  The duplicate pane implementation was unnecessary; timestamp handling did not
  need another implementation.
- The release-channel trait has a real provider-update test double outside the
  file cited by the audit. The trait remains; the test that only checked a
  double's constant return was removed.
- A valid acquired engine block makes its handle/count getters infallible under
  the bundled native API. Those getters remain; formatter failures propagate.
- The stream-JSON writer already reports write failures and stops its process.
  Its stale comment was corrected.
- Default-value and legacy-deserialization tests detect compatibility changes.
  Registry-root lists and distinct resize-handle IDs protect specific ownership
  and interaction rules. These checks remain with accurate names.
- Alpha-composition checks, frozen-header offsets, parser field mapping, large
  queued payloads, and sibling-room isolation retain useful regression coverage.
- Test constructors, read-only inspectors, deterministic subprocess controls,
  and disabled-profiling build checks are not defects solely because production
  does not call them. Useful fixtures remain; duplicated behavior and counters
  maintained only for assertions were removed.
- GPUI's thread-priority and drag controls belong to its UI backend integration.
  OS-specific scripts in integration tests are valid fixtures.
- `ProcessTree::other_process_count` retains its existing numeric interface for
  remote consumers. Local terminal closing uses the fallible query instead.
  `live_frame_text` is retained for existing remote tests; normal app builds
  currently report it as unused after the gutter removal.

The macOS count handling was checked against Apple's
[libproc implementation](https://github.com/apple-oss-distributions/xnu/blob/main/libsyscall/wrappers/libproc/libproc.c)
and [process enumeration](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/kern/proc_info.c).
The Explorer result handling follows the documented
[Invoke return behavior](https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nf-shobjidl_core-iexplorercommand-invoke).

## Verification

- Full tests passed for agent, configuration, input, platform, terminal, version,
  profiling, hook CLI, and grammar bundle crates, including their integration
  tests. Existing ignored tests remained ignored.
- Application tests passed: 340 library tests, 244 binary tests, and one
  integration test; two existing library tests remained ignored.
- All four real PowerShell/ConPTY vtebench regression tests passed separately.
  Their startup path now fails explicitly if required prerequisites are absent.
- Regression coverage includes failed attachment writes and successful retries,
  invalid configuration reads, child transcript updates, missing Git working
  directories, incomplete update rollback, optional terminal colors, and team
  context rejection without budget reservations.
- Strict Clippy checks passed for the affected workspace packages with
  `absolute_paths`, `complexity`, `perf`, and `style` denied. The retained
  remote-test helper warning described above is still present.
- The configuration crate also passed a check without default features.
- Direct hook-CLI checks confirmed that malformed versions and JSON produce
  diagnostics without including the token or input; an unrouted hook stays quiet.
- Modified Rust files passed formatting and the diff passed whitespace checks.

Validation ran on Windows. macOS and Linux native behavior was reviewed against
the implementation and available API definitions, but was not executed on those
operating systems. Native notification permission prompts and Explorer menu
invocation were not manually exercised.
