# Type Design Governance: Impl Locality and Redundant Types

Baseline: commit `415a4d3e`, branch `dev`, 2026-09-13.
Audience: implementation agents executing one task at a time. Read sections
1 through 3 completely before touching any file.

Line numbers below were taken from the baseline commit and will drift. Locate
code by item name (struct / enum / fn / impl), never by line number alone.

## 0. Progress as of 2026-09-14

All planned work is complete on `refactor/impl-20260913`, with the final
code and comment changes recorded through `cf4f75c8`. The detailed task
descriptions below describe the baseline code. Completed tasks must not be
applied again.

| Phase | Status | Completed scope |
|---|---|---|
| A | Complete | All listed type removals, simplifications, and renames. |
| B | Complete | Badge and controls removal, forwarding removal, and mirrored-state changes. |
| C | Complete | All 12 main types and the additional implementations covered by rule 2.1. |
| D | Complete | Baseline file and allowlist handling removed; every reported violation is rejected. |
| Sparkle note (5.7) | Complete | Configuration dependency choice documented at the channel translation. |

### 0.1 Enforcement and completed work

- [x] Add the implementation locality checker and pre-commit integration
  (`8511511f`).
- [x] Record the locality rule in `AGENTS.md` (`2b0514bc`).
- [x] Complete the TranscriptView batch, including CodeView rendering,
  ImagePreviewLayer, PickerReservation, and test-helper relocation
  (`c45eeb39`).
- [x] Migrate Shell (`43b0cab3`), then AgentPane (`32a402eb`). ShellChrome
  and AgentNotificationState own their related fields and operations.
  Pane components own palette state, history navigation, branch prompt
  preparation, question editing, and local workflow behavior. Coordination
  across components lives with the parent type.
- [x] Finish every additional implementation listed in the table below.
- [x] Remove the unused palette re-export and keep palette controls and
  handlers private to the pane (`fec07dc1`).
- [x] Add the Sparkle translation comment (`cf4f75c8`).
- [x] Extend checker coverage to `unsafe impl` headers and verify rejection
  when no baseline exists (`34ff0eef`).
- [x] Remove `scripts/impl-locality-baseline.txt` and the checker's allowlist
  handling (`34ff0eef`).

The current checker reports **no violations**. All 50 entries in the
2026-09-13 migration baseline have been removed. There is no remaining
migration allowlist.

| Additional type | Result | Commit |
|---|---|---|
| ComposerAttachments, SlashPalette, SessionHistoryUi | Definitions and implementations colocated in their component files. | `32a402eb` |
| TerminalLine | EngineRowBuilder and its conversion live in `frame/line.rs`. | `9bd0ffb9` |
| BackgroundTasksView | Detail behavior lives with the view definition. | `fa0ba580` |
| Sidebar | Row rendering lives with the sidebar definition. | `af1f39bf` |
| InputHistoryScope | Stored scope conversion is a private function in the storage module. | `05f725d2` |
| Backend | Team operations live with the backend definition. | `f11bb681` |
| SessionInput | Approval state handling lives with the input definition. | `6b7c0a4f` |
| Room | Context preparation and private coverage helpers live with Room. | `2ef45129` |
| SessionOptions | Remote request conversion lives with the host options definition. | `13dcb0a4` |
| KeyEncodeFlags | Terminal modes are translated by `key_encode_flags` in the terminal crate. | `bc99bbc0` |
| KeyInput | Terminal keys are translated by private `key_input` in the terminal crate. | `9f245fb5` |

### 0.2 Validation recorded for this implementation batch

Green application and core-crate baselines were established before editing.
Affected crate suites were rerun after each migration. Final results:

| Check | Result and exercised behavior |
|---|---|
| Windows app suites | **592 passed, 2 ignored**. Includes input-history navigation and draft restoration, palette behavior, background-task navigation, and existing shell and transcript scenarios. |
| Agent suites | **609 passed, 12 ignored**, excluding repeated child-process reports. Includes approval handling, backend team operations, stored input history, room summaries, and context coverage. |
| Terminal suites | **238 passed**. Includes application cursor sequences, named and modified keys, clipboard actions, IME handling, and session behavior. |
| Input baseline | **29 passed**. The input crate was unchanged by this batch. |
| Remote default suites | **30 passed, 9 ignored**. Includes unit checks and ConPTY reconnect behavior. |
| Remote local relay scenarios | All **9** normally ignored host and relay tests passed with a temporary local relay and test token. Includes shell reconnect, transport-loss recovery, encrypted exchange, and rejecting unpaired clients. The temporary service was stopped afterward. |
| Locality checker regressions | **7 passed**. Ordinary and unsafe implementations in the wrong file are rejected without a baseline; clean sources and documented exceptions are accepted. |
| Repository locality scan | **0 violations**, with no baseline file or allowlist support. |
| Profiling build | `./scripts/profiling.ps1 check -p nmt_terminal` passed after both terminal conversion changes. |
| Commit checks | Strict first-party Clippy, readability, formatting, locality, and whitespace checks passed. Hooks were not bypassed. |

Stale and read-only local build outputs interrupted initial validation
attempts; regenerating the affected outputs allowed the checks to complete.
No source changes or persistent cache configuration changes were needed for
that recovery.

No manual application validation or non-Windows build was performed. The
Sparkle change is a comment only; its macOS execution was not validated here.

## 1. Goal and non-goals

Two goals, in priority order:

1. **Impl locality.** Every `impl` block whose `Self` type is a first-party
   struct or enum lives in the source file that defines that type. Section 2
   states the rule; section 4 migrates the 12 types that currently violate it.
2. **No redundant types.** Section 5 removes or collapses the wrappers,
   mirrors, dead abstractions, and single-use generics found in the
   2026-09-13 audit. Each item names the file, the change, and the callers.

Non-goals:

- No behavior change. Every task is either a code move or a type collapse
  whose observable behavior is identical before and after.
- No new dependencies, no new traits, no new generic parameters.
- No renaming beyond what a task explicitly lists.
- Do not touch `crates/third_party/` or `crates/tree_sitter_bundle/`.
- File length is not a criterion. The 800-line guideline was retired on
  2026-09-13. A file is judged by whether it mixes concerns, never by size.

## 2. The impl locality rule

### 2.1 Normative text

> Every `impl` block whose `Self` type is a struct or enum defined in this
> repository is written in the file that defines that type. This covers
> inherent impls (`impl Type`), trait impls (`impl Trait for Type`), and
> conversions into the type (`impl From<X> for Type`). A method that must live
> in another file for a real reason is not a method: it is a free function in
> that file, or a method of a sub-struct that the file defines and owns.

### 2.2 Why

Splitting one type's impl across many files makes the type's full API
invisible from its definition. A reader who opens `agent_tab/mod.rs` sees 27
fields and no methods; the 5,700 lines of behavior that mutate those fields
sit in 23 other files. Invariants between fields cannot be reviewed because no
single file shows every writer. The split also hides god objects: a type with
one 6,000-line impl file is obviously too big, while the same type spread over
24 files of 200 lines looks tidy.

The rule turns that pressure into a design signal. When a type's single file
becomes unreadable, the fix is to move state into a sub-struct that owns its
own file, not to move methods away from their fields.

### 2.3 Exceptions

Only these. Anything else is a violation.

| Case | Where the impl lives |
|---|---|
| Platform pairs: the same type name defined once per `#[cfg]` platform file (`Clipboard`, `Pty`, `ProcessTree`, `KillOnCloseJob` under `platform/src/{unix,windows}/`) | Each platform file defines its own type and holds its own impls. |
| Build-mode pairs (`profiling/src/transcript/{enabled,disabled}.rs` and siblings) | Same as platform pairs. |
| `#[cfg(test)]` impls (`Default` for fixtures, test-only constructors) | The module's `tests.rs` or `*_tests.rs` file. |
| Extension traits on a foreign type (`AgentCapabilities` and `AgentKindExt` on `nmt_profile::AgentKind`) | The file that defines the trait. The orphan rule leaves no other option. |
| Blanket impls (`impl<T: Bound> Trait for T`) | The file that defines the trait. |
| `impl From<A> for B` where both are first-party | With `B`, the `Self` type. If `B` is in another crate that cannot depend on `A`'s crate, the conversion becomes a free function in `A`'s crate. |

### 2.4 Migration decision rule

For every impl block that is in the wrong file, pick the first option that
applies:

1. **The block's methods read or write only one field (or one cluster of
   fields that always change together).** Move those fields into a sub-struct
   defined in the block's current file, and turn the block into
   `impl SubStruct`. The parent keeps one field of that type. Callers change
   from `parent.method()` to `parent.field.method()` or the parent gets a
   one-line forwarding method only where an external caller needs it.
2. **The block's methods do not use `self` beyond reading arguments that the
   caller already has.** Convert them to free functions in the current file
   with explicit parameters. Delete the empty type if nothing remains.
3. **Otherwise.** Move the block into the defining file. Keep the original
   file for its free functions, constants, and sub-types; delete it if it
   becomes empty.

Option 3 is the default for `Render`, `EventEmitter`, `Focusable`, and other
GPUI trait impls: they need the whole entity and belong with its definition.

### 2.5 Enforcement

`scripts/check-impl-locality.py` runs from `.githooks/pre-commit` whenever
staged files include Rust. The check:

1. Index every `struct` / `enum` definition under `crates/` (excluding
   `third_party` and `tree_sitter_bundle`) to its file.
2. For every ordinary or `unsafe impl` block, resolve the `Self` type name.
   Skip type parameters, foreign types, and files whose path contains `/unix/`, `/windows/`,
   `/enabled.rs`, `/disabled.rs`, `tests.rs`, or `_tests.rs`.
3. Report every impl whose file differs from the defining file.

The migration used `scripts/impl-locality-baseline.txt` to allow existing
violations while rejecting new ones. Each task removed its migrated entries.
Commit `34ff0eef` removed the file and all allowlist handling after the scan
reached zero; the hook now rejects every reported violation.

### 2.6 AGENTS.md text

The paragraph below is added under "Module organization and imports" in
`AGENTS.md` as part of this plan:

> Every `impl` block for a first-party struct or enum, inherent or trait,
> lives in the file that defines the type. Behavior that belongs in another
> file is a free function there, or a method on a sub-struct that file owns.
> The only exceptions are `#[cfg]` platform pairs that each define their own
> type, test-only impls in the module's tests file, and extension or blanket
> traits on types this crate does not own, which live with the trait.

## 3. Execution protocol

1. One task per commit. A task is one row of section 4 or one item of
   section 5. Two tasks share a commit only when the second cannot compile
   without the first.
2. Confirm a green baseline before editing: `cargo test -p <crate>` for the
   crate you are about to touch.
3. Move code with cut and paste. Do not retype, reorder, or reformat moved
   code beyond what `cargo fmt` enforces.
4. Imports in files you touch are anchored at `crate::` or an external crate
   name. Never `super::` or `self::`.
5. Visibility narrows, never widens, unless a task says otherwise. A method
   that was `pub(super)` in `session/events.rs` and moves to `mod.rs` stays
   `pub(super)` or becomes private if its only callers are now in the same
   file.
6. Comments that move with code are kept verbatim. New comments explain the
   technical reason, never "moved because of the plan".
7. After each task: `cargo test -p <crate>`, then `cargo clippy --all-targets
   -p <crate>`. For tasks touching `crates/terminal` also run
   `./scripts/profiling.ps1 check -p nmt_terminal`; the profiling build
   compiles code that a default build skips.
8. Commit with a Conventional Commit subject, for example
   `refactor(agent_tab): move AgentPane impl into its defining file`. Never
   bypass the hooks.

## 4. Impl locality migration

Sizes are impl-block lines at the baseline. "Merged" is the defining file's
size if every block were moved there under option 3; the directive column
says how much of that should instead become sub-structs or free functions.

### 4.1 Inventory

| Type | Defining file | Files | Impl lines away | Merged size | Status / commit |
|---|---|---|---|---|---|
| `AgentPane` | `app/src/agent_tab/mod.rs` | 24 | 5,758 | ~6,100 | Complete: `32a402eb`. |
| `Shell` | `app/src/ui/shell/mod.rs` | 13 | 2,925 | ~3,600 | Complete: `43b0cab3`. |
| `TranscriptView` | `app/src/agent_tab/transcript/view/mod.rs` | 10 | 2,287 | ~3,000 | Complete: `c45eeb39`. |
| `AgentSession` | `app/src/agent_tab/execution/mod.rs` | 9 | 1,515 | ~1,800 | Complete: `d2b946c4`. |
| `TeamSession` | `agent/src/team/session/mod.rs` | 8 | 1,551 | ~1,900 | Complete: `5ed61edf`. |
| `SessionController` | `agent/src/session/controller/mod.rs` | 8 | 934 | ~1,100 | Complete: `5ff8d558`. |
| `TeamPane` | `app/src/agent_tab/team/view.rs` | 4 | 815 | ~1,300 | Complete: `3b980ba4`. |
| `TeamRuntime` | `app/src/agent_tab/team/mod.rs` | 5 | 676 | ~900 | Complete: `870436e5`. |
| `ClaudeTasks` | `agent/src/claude_code/tasks/mod.rs` | 3 | 458 | ~1,000 | Complete: `39f425ec`. |
| `ConversationBranch` | `agent/src/session/branch/mod.rs` | 3 | 441 | ~800 | Complete: `2d9de5f1`. |
| `ThreadControls` | `app/src/agent_tab/thread_controls/mod.rs` | 3 | 412 | see 5.1 | Dissolved: `a8912a4e`. |
| `RoomStore` | `agent/src/team/storage/mod.rs` | 3 | 209 | ~500 | Complete: `9896ef06`. |

Trait impls that are also out of place and move with the same tasks:

| Impl | Baseline location | Destination / change | Status / commit |
|---|---|---|---|
| `Render`, `EventEmitter`, `Focusable` for `AgentPane` | `agent_tab/view/mod.rs` | `agent_tab/mod.rs` | Complete: `32a402eb`. |
| `Render for CodeView` | `transcript/code/render.rs` | `transcript/code/mod.rs` | Complete: `c45eeb39`. |
| `IconNamed for EffortGaugeIcon` | `thread_controls/mod.rs` | `thread_controls/effort.rs` | Complete: `a8912a4e`. |
| `From<&TerminalSettings> for PaneSettings` | `terminal_tab/settings.rs` | removed by 5.6 | Complete: `141fb299`. |
| `From<EngineRowBuilder> for TerminalLine` | `terminal_tab/block_list/rows.rs` | `terminal_tab/frame/line.rs` | Complete: `9bd0ffb9`. |
| `From<Mode> for KeyEncodeFlags` | `terminal/src/input.rs` | `nmt_input` cannot depend on `nmt_terminal`; becomes `fn key_encode_flags(mode: Mode) -> KeyEncodeFlags` in `terminal/src/input.rs` | Complete: `bc99bbc0`. |
| `FileUserSession for RestartManagerSession` | `app/src/update/mod.rs` | revised by 5.11 | Complete: `65b8a2bc`. |

### 4.2 `AgentPane` (24 files)

Do this type last among the app types; it depends on `SessionStateBadge`
(5.1), `ThreadControls` (5.1), and the mirror-field removal (5.6) landing
first so the merged file does not carry code that is about to be deleted.

The pane already stores most of its state in sub-structs. The migration
assigns each out-of-place impl file to the sub-struct whose fields it touches.
Before moving, run the field-usage check on each file: list every
`self.<field>` the block references. If the list is one sub-struct field plus
`cx`, option 1 applies. If it spans `session`, `host`, `transcript`, and
`input`, option 3 applies.

| Current file | Lines | Fields touched (from the audit) | Directive |
|---|---|---|---|
| `composer/palette.rs` | 700 | `palette`, `input`, `session` | Methods that only read `self.palette` move onto `impl SlashPalette` in this file. The rest move to `mod.rs`. |
| `composer/slash.rs` | 503 | `palette`, `input`, `session`, `host` | Option 3. |
| `composer/branch/fork.rs`, `rewind.rs`, `mod.rs` | 644 | `branch`, `session`, `transcript` | Methods that only touch `self.branch` move onto `impl BranchFlow`; the rest option 3. |
| `composer/images.rs` | 130 | `attachments`, `input` | Onto `impl ComposerAttachments` where only `self.attachments` is used; else option 3. |
| `composer/mod.rs` | 167 | `input`, `session`, `prompts` | Option 3. |
| `composer/response_annotations.rs` | 23 | `transcript` | Option 3. |
| `input_history.rs` | 70 | `input_history_navigation`, `input`, `input_history_scope` | Onto `impl InputHistoryNavigation` with `input` and `scope` passed as arguments. |
| `questions/actions.rs`, `questions/render.rs` | 671 | `prompts`, `session`, `host` | Methods that only touch `self.prompts` move onto `impl PendingPrompts`; render methods option 3. |
| `session/mod.rs`, `startup.rs`, `events.rs`, `turn.rs`, `conversation.rs`, `background_tasks.rs`, `update_recovery.rs` | 1,607 | `session`, `host`, `binding`, `transcript`, `turn` | Option 3. These are the pane's lifecycle and event handling; they need the whole entity. |
| `session/history/mod.rs` | 214 | `history_ui`, `session` | Onto `impl SessionHistoryUi` where only `self.history_ui` is used; the rest option 3. |
| `thread_controls/defaults.rs` | 49 | `binding`, `session` | Option 3. |
| `view/mod.rs`, `view/banners.rs`, `view/history.rs` | 888 | everything | Option 3. `Render` and the render helpers move to `mod.rs`. `view/` keeps `session_state.rs` as free functions (5.1) and any element builders that take explicit arguments. |
| `workflows.rs` | 92 | `workflows`, `session` | Onto `impl WorkflowUi` where possible; else option 3. |

Expected result: `agent_tab/mod.rs` holds the struct, its lifecycle, its
event handling, and its `Render`, on the order of 3,500 to 4,000 lines. The
`composer/`, `questions/`, and `session/history/` files keep their
sub-structs with their own impls.

### 4.3 `Shell` (13 files)

Nine of the 22 fields are single-file state. Extract two sub-structs first,
then move the remaining blocks.

1. **Render-only state** (`tab_strip`, `token_usage`, `agent_usage`,
   `git_status`, `root_observed`, `needs_focus`) is touched by `render.rs`
   and once by `pump.rs`. Define `struct ShellChrome` in `shell/render.rs`
   holding those six fields, and turn the `render.rs` block into
   `impl ShellChrome` plus one `Render for Shell` block that moves to
   `mod.rs` and calls into `self.chrome`.
2. **Agent notification state** (`agent_timer_generation`,
   `pending_agent_resume`, `pending_agent_close`) is touched only by
   `agent_notifications.rs` and `render.rs`. Define
   `struct AgentNotificationState` in `shell/agent_notifications.rs`, move
   the three fields, and make the block `impl AgentNotificationState`. Methods
   that also need `workspaces` or `window_id` take them as arguments.
3. `root_availability` is `workspace_dirs.rs`-only and `doomed_workspace` is
   `close.rs`-only. They stay on `Shell`; their blocks move to `mod.rs`
   (option 3). Single scalars do not justify a sub-struct.
4. `close.rs`, `panes.rs`, `tabs_open.rs`, `workspaces.rs`, `rename.rs`,
   `panels.rs`, `settings_workspace.rs`, `pump.rs`, and the one block in
   `ui/persistence.rs`: option 3.

Expected result: `shell/mod.rs` near 2,800 lines; `render.rs` and
`agent_notifications.rs` remain as sub-struct owners.

### 4.4 `TranscriptView` (10 files)

1. `transcript_width`, `transcript_height`, `transcript_origin`, and
   `image_preview` are touched only by `render/image_preview.rs` and
   `view/mod.rs`. Define `struct ImagePreviewLayer` in
   `render/image_preview.rs` with those four fields and make the block
   `impl ImagePreviewLayer`.
2. `stashed_position` and `reserve_below` are touched only by `rows.rs` and
   `view/mod.rs`. Define `struct PickerReservation` in `rows.rs` with those
   two fields; the methods that only touch them move onto it.
3. `render/mod.rs`, `render/user_row.rs`, `render/work_row.rs`,
   `render/questions.rs`, `incremental.rs`, `reveal.rs`, the remainder of
   `rows.rs`, and `view/fixtures.rs` (test-only: goes to `view/tests.rs`
   under the test exception): option 3.

### 4.5 `AgentSession` (9 files)

1. `child_refresh` / `child_readers` (only `execution/children.rs`) and
   `workflow_refresh` / `workflow_readers` (only `execution/workflows.rs`)
   are the same shape. Define one
   `struct PollingRefresh<R> { task: Option<Task<()>>, readers: R }` in
   `execution/mod.rs`, replace the four fields with two, and move the methods
   that only touch the pair onto `impl<R> PollingRefresh<R>`.
2. Every other block (`branch.rs`, `events.rs`, `history.rs`,
   `maintenance.rs`, `questions.rs`, `startup.rs`, and the rest of
   `children.rs` / `workflows.rs`): option 3. The `controller` field is used
   from every file, so no further sub-struct exists to extract.

### 4.6 `TeamSession` (8 files)

The split is per workflow (`planning`, `controls`, `outcomes`, `members`,
`summaries`, `recovery`, `moderation`), not per field, and the struct has
four fields. Option 3 for all seven blocks; `team/moderation.rs` keeps its
free functions. Before moving, apply 5.4 so the three pure forwards do not
travel.

### 4.7 `SessionController` (8 files)

Already decomposed into ten sub-structs (`runtime`, `delivery`, `restore`,
`naming`, `input`, `branch`, `commands`, `children`, `workflows`,
`controls`). For each of `activity.rs`, `content.rs`, `events.rs`,
`input.rs`, `readiness.rs`, `settings.rs`, `transitions.rs`: methods that
touch exactly one sub-struct field move onto that sub-struct's own impl (in
the file that defines the sub-struct); the rest option 3. Apply 5.4 first so
the five setters in `settings.rs` are gone before the move.

### 4.8 `TeamPane` (4 files)

1. `profile_index`, `member_name`, `member_role` are only touched by
   `view/membership.rs`. Define `struct MemberDraft` there.
2. `rows`, `revision` are only touched by `view/timeline.rs`. Define
   `struct TimelineMirror` there.
3. `view/options.rs` (495 lines) touches shared state: option 3.

### 4.9 `TeamRuntime`, `ClaudeTasks`, `ConversationBranch`, `RoomStore`

All four are cohesive; no sub-struct is indicated. Option 3 for every block:

- `TeamRuntime`: `controls.rs`, `dispatch.rs`, `events.rs`, `recovery.rs`.
- `ClaudeTasks`: `observe.rs`, `shells.rs` (the `ShellIndex` sub-struct in
  `shells.rs` already owns its own impl and stays).
- `ConversationBranch`: `local.rs`, `protocol.rs`. The two files keep their
  free functions for the file-rewind and server-fork mechanisms.
- `RoomStore`: `attachments.rs`, `dispatch.rs`. Apply 5.4 first so the
  dispatch domain logic moves to `team::session` rather than into the store.

### 4.10 `ThreadControls`

Dissolved by 5.1. The `harness_rows.rs` block is `impl AgentPane` and is
covered by 4.2.

## 5. Redundant type fixes

Each item: location, what is redundant, the change, the callers, and the
check that proves behavior is unchanged. Items are independent unless a
dependency is stated. Every item is one commit.

### 5.1 Self-less types become free functions (app)

Status: **Complete**. SessionStateBadge: `1159b6bc`; ThreadControls:
`a8912a4e`.

**`SessionStateBadge`**, `app/src/agent_tab/view/session_state.rs:18`. A
zero-field unit struct whose single method `render(&self, goal, plan_mode,
cx)` never reads `self`. Stored as `AgentPane.session_state`.

- Change: `pub(crate) fn session_state_badge(goal: &Option<GoalStatus>,
  plan_mode: bool, cx: &mut Context<AgentPane>) -> Option<impl IntoElement>`.
  Delete the struct and the pane field.
- Callers: the one `self.session_state.render(...)` in `view/mod.rs`; the
  construction in `session/mod.rs:269`.
- Check: existing render tests for the plan-mode and goal badges.

**`ThreadControls`**, `app/src/agent_tab/thread_controls/mod.rs:28`. One
field `effort_drag: Option<usize>` that none of its own methods read; the
field is only reached as `pane.controls.effort_drag` from `effort.rs`.
`remember_defaults`, `model_options`, `render_row`, `render_claude_row` carry
a `&self` they never use.

- Change: those four become free functions in their current files taking
  `&ConversationSettings` and `AgentKind`. `effort_drag` moves onto
  `AgentPane` directly. Delete the struct.
- Callers: `effort.rs:134,184,257,277,304`; `harness_rows.rs:37,145,260`.
- Check: composer pill rendering and effort drag tests.

### 5.2 Dead abstractions (terminal, config, platform)

Status: **Complete**. Dimensions and Selection::rotate: `b0157ae6`;
EventListener and WindowId: `94ae414d`; background color types: `7ecde1a2`;
ChildEvent: `e87c5017`.

**`Dimensions` trait and `Selection::rotate`**,
`terminal/src/terminal/grid/mod.rs:14` and `terminal/src/selection.rs:133`.
The trait's only implementor is `#[cfg(test)] impl Dimensions for (usize,
usize)`; its only consumer is `rotate`, which has zero callers in the
workspace.

- Change: delete both, plus the `use crate::terminal::grid::Dimensions` in
  `selection.rs`.
- Check: `cargo test -p nmt_terminal` still passes; nothing else references
  either name.

**`EventListener` default methods**, `terminal/src/event.rs:299`. Four of five
methods (`event`, `send_event_with_high_priority`, `send_redraw`,
`send_global_event`) have no callers anywhere; only `send_event` is used.

- Change: reduce the trait to `fn send_event(&self, event: TerminalEvent)`.
  This also drops the `WindowId` parameter (next item).
- Callers: the implementors in `event.rs`, `session/proxy.rs:70`, and
  `remote_net/src/hub.rs:256`.

**`WindowId`**, `terminal/src/event.rs:19`. The only production construction
is `WindowId::dummy()` at `pty_pipe/session.rs:116`; every implementor binds
the parameter as `_id`. About 25 call sites in `pty_pipe/mod.rs` and
`pty_pipe/marks.rs` thread a constant zero.

- Change: delete the type and the parameter from `send_event`.
- Special case: `PtyProfiler::new(window_id.0, ...)` at
  `pty_pipe/mod.rs:318` compiles only under `--cfg enable_profiling`. Pass a
  literal `0` there and verify with
  `./scripts/profiling.ps1 check -p nmt_terminal`.

**`ColorComposition` and `render_types::Color`**,
`config/src/colors/mod.rs:21` and `config/src/render_types.rs:7`.
`Colors::background` is `(ColorArray, ColorWGPU)`; every reader takes `.0`
(`term.rs:133`, `app/.../pane_model/settings.rs:55`, `app/.../settings.rs:90`,
`app/.../settings/theme.rs:226`, `ghostty/tests.rs:682`). `.1` is never read,
and `render_types::Color` has no other production consumer.

- Change: `background: ColorArray`; delete `ColorComposition`,
  `render_types.rs`, and the `ColorWGPU` alias. Update
  `config/tests/colors.rs`.
- Check: the five readers compile with `.0` removed; color tests pass.

**`ChildEvent`**, `platform/src/lib.rs:115`. Single-variant enum `Exited`
returned as `Option<ChildEvent>` from `next_child_event` across 8 impls.

- Change: `fn next_child_event(&mut self) -> Option<()>` is misleading;
  rename to `fn child_exited(&mut self) -> bool`. Update the eight impls
  (`platform/src/{unix,windows}/mod.rs`, `platform/src/windows/child.rs`,
  `remote_net/src/net_pty.rs`, `agent/src/claude_code/usage_fetcher.rs`,
  `app/src/terminal_tab/pane_model/test_session.rs`) and the channel type in
  `windows/child.rs:26,51,64`.

### 5.3 One-field and one-variant wrappers (agent)

Status: **Complete**. RestoredWorkflowRun: `af674f69`; VendorUpdateResult:
`b6d10717`; TeamCapabilities: `e3b07df8`; RecoveryNotice: `da5c66bf`;
TurnActivity: `c113c30b`; InputError: `10b0a6c7`; DescendantQuery:
`8daa9a53`; ConversationId: `a68bfef2`.

| Type | Location | Change |
|---|---|---|
| `RestoredWorkflowRun` | `agent/src/workflow.rs:176` | One `pub run: WorkflowRun` field, no impl, constructed in 3 places and destructured at once in `ClaudeWorkflows::merge_restored`. Use `Vec<WorkflowRun>` in `WorkflowSource::restore` and `Session::restore_workflows`. Callers: `claude_code/stream_json/mod.rs:1115`, `claude_code/workflows/disk.rs:189,303,313,322,344,370,398`. |
| `VendorUpdateResult` | `agent/src/update/mod.rs:282` | One `pub diagnostic: String` field. `ProviderMaintenance::update` returns `Result<String, UpdateError>`. Callers: `claude_code/update.rs:166`, `codex/update.rs:33`, `update/coordinator_tests.rs:33,61,64`. |
| `TeamCapabilities` | `agent/src/session/team_capabilities.rs:41` | One `pub moderation: ModeratorAdmission` field; every caller reaches through to it. Replace with `ModeratorAdmission` and move `unverified(kind)` onto it. Callers: `codex/app_server/team.rs:103`, `session/backend/team.rs:61-67`, `team/session/mod.rs:48`. |
| `RecoveryNotice` | `agent/src/team/storage/mod.rs:47` | Single variant `TornFinalRecord`, produced at one site (`mod.rs:128`), carried as `Vec<RecoveryNotice>` through `RoomStore::open` and `TeamSession::open`, never matched. Return `bool truncated` instead. Update `storage/tests.rs:142`. |
| `TurnActivity` | `agent/src/chat/mod.rs:356` | Single variant `Retrying { attempt, total, reason }` inside `Option` at both consumers. Make it `pub struct TurnRetry { .. }`. Callers: `Event::StatusDetail`, `session/controller/events.rs:88`, `dsh/mapping.rs:550`. |
| `InputError` | `agent/src/subprocess/input.rs:14` | Single variant `Closed` through six `Result<_, InputError>` signatures in `subprocess/mod.rs`. Replace with a unit struct `InputClosed` keeping the `thiserror` message. |
| `DescendantQuery` | `agent/src/codex/app_server/background_tasks/mod.rs:54` | `Copy` struct with one private `starting_sequence: u64`, no methods, used as a `HashMap` value. Use `HashMap<u64, u64>` and move the field's doc comment to the map. |
| `ConversationId` | `agent/src/team/identity.rs:45`, `team/member.rs:41,87` | Generated UUID newtype whose only reader is one test assertion (`team/tests.rs:51`). Remove from the `identities!` list, the `Member` field, the accessor, and the persisted column. Persisted rooms written before this change carry an extra key that serde ignores. |

### 5.4 Forwarding layers (agent)

Status: **Complete**. SessionController setters: `be33cd73`; TeamSession
forwards: `e9f4692d`; RoomStore dispatch logic: `565e0284`; DeadlineTimer
forward: `c397bade`.

**`SessionController` setters**, `agent/src/session/controller/settings.rs:64-83`.
`set_approval`, `set_effort`, `set_approval_reviewer`, `set_sandbox`,
`set_tier` are two-line forwards into `self.controls.settings.<field>`; they
exist because `controls` is the one private sub-struct field while
`runtime`, `delivery`, `input`, `branch` and the rest are `pub`.

- Change: make `controls` `pub` like its siblings; delete the five setters
  and the `controls()` getter at `mod.rs:8`. Callers write
  `controller.controls.settings.effort = ...` directly.

**`TeamSession` forwards**, `agent/src/team/session/mod.rs:85,155,159`.
`room()`, `revision()`, and `commit_room()` forward to `RoomStore` with no
added logic beyond one error conversion.

- Change: inline the three; callers inside `team::` reach `self.store`
  directly. Depends on nothing; must land before 4.6.

**`RoomStore` dispatch logic**, `agent/src/team/storage/mod.rs`.
`reserve_dispatches`, `dispatch`, and `read_attachment` (in
`storage/dispatch.rs` and `storage/attachments.rs`) are dispatch domain
logic on a durability type.

- Change: move the three into `team/session/` as `TeamSession` methods (or
  free functions) that call `RoomStore::commit`. Must land before 4.9.

**`DeadlineTimer`**, `agent/src/deadline_timer.rs:39`. `set()` forwards
verbatim to `TimerHandle::set`; `handle()` clones the inner. The `Drop` that
stops the thread is a real invariant, so the type stays. Delete only the
`set()` forward and have callers use the handle. Lowest priority in this
section.

### 5.5 1:1 wrappers over another crate (app)

Status: **Complete**. PendingAttachments: `5ae68e02`; AgentInputHistory:
`543d4d5b`; RestartManagerSession: `f2bad5a6`; ApplicationStatus:
`4e413186`.

**`PendingAttachments` and `Attachment<'a>`**,
`app/src/agent_tab/composer/attachments/mod.rs:200-260`. Eight methods
forward name-for-name to `nmt_agent::images::PendingAttachments<Arc<Image>>`;
`Attachment<'a>` wraps a reference so `iter()` can `.map(Attachment)`.

- Change:
  `pub(crate) type PendingAttachments = nmt_agent::images::PendingAttachments<Arc<Image>>;`
  plus one free
  `fn attach_png(pending: &mut PendingAttachments, image: &Image) -> Result<String, AttachError>`
  that supplies the decode closure. `iter()` yields
  `&nmt_agent::images::Attachment<Arc<Image>>`; `render.rs` reads `.image`
  and `.placeholder()` directly. `ComposerAttachments` at `:51` holds real
  state and is unchanged.
- Cross-crate note: the generic `T` on the core type is instantiated only
  here. Keep it; the app-side wrapper was the redundant half.

**`AgentInputHistory` forwards**, `app/src/agent_tab/input_history.rs:22-30`.
The newtype is needed for `impl Global`. `entries` and `record` are private
one-line forwards with one caller each, while `flush` at `:39` already reaches
through with `.0`.

- Change: make the field `pub(super)` and delete both methods; the two
  callers (`:165`, `:211`) use `.0`.

**`RestartManagerSession(Session<SystemApi>)`**,
`platform/src/windows/restart_manager.rs:130`. Four methods forward verbatim.

- Change: make `Session<A>`'s methods `pub` and export
  `pub type RestartManagerSession = Session<SystemApi>;`.

**`ApplicationStatus(u32)`**, `platform/src/windows/restart_manager.rs:36`.
`bits()` has no callers; `AffectedApplication::status` is never read; the
`From<u32>` at `:359` is never used.

- Change: delete the field, the newtype, and the `From`.

### 5.6 Mirrored state (app)

Status: **Complete**. AgentPane mirrors: `e5682a27`; PaneSettings:
`141fb299`; remembered thread settings conversions: `483bd65f`; drag
preview state: `f6868606`.

**`AgentPane` mirrors `AgentSession`**, `app/src/agent_tab/mod.rs:268-285`
vs `app/src/agent_tab/execution/mod.rs:53-57`. `kind`, `profile`,
`workspace`, `active_workspace`, `agent_route` exist on both; `session/startup.rs:48,66,67`
copies them in both directions on every start, so the two can disagree
between those statements.

- Change: delete the five pane fields. Readers call
  `self.host.read(cx).<field>`; the one writer (`host.workspace =
  self.workspace.clone()`) becomes a parameter of `host.start`.
- Risk: `host` is a `WeakEntity`; every read site already handles the
  upgrade failure path because `binding.is_current()` guards them. Verify
  each read site before converting.
- Must land before 4.2.

**`PaneSettings`**, `app/src/terminal_tab/pane_model/settings.rs:33`. Third
projection of the same values (`Config` to `TerminalSettings` to
`PaneSettings`); five of seven fields are verbatim copies, only `pad_rows`
is derived, and it is built in one place (`terminal_tab/view/mod.rs:214`).

- Change: `PaneController` reads the five copied values from
  `TerminalSettings` and keeps only `pad_rows`. Delete
  `From<&TerminalSettings> for PaneSettings` in `terminal_tab/settings.rs`.

**`AgentThreadDefaults` conversions**, `app/src/agent_tab/profile/mod.rs:73,93`.
Both `From` impls transcribe the same six fields between
`nmt_config::local_state::AgentDefaults` and `nmt_agent::chat::ThreadSettings`,
which are field-for-field twins in two crates.

- Constraint: the two structs cannot be one type. `AgentDefaults` carries
  `skip_serializing_if` on every field because it is the persisted
  `local_state.toml` shape; `ThreadSettings` serializes every field because
  it is the per-turn payload shape (`tier: null` resets). `nmt_agent` does
  not depend on `nmt_config` (`crates/agent/Cargo.toml` lists only
  `nmt_platform` and `nmt_profile`), so neither crate can host a `From`
  between them without a new dependency edge.
- Change: keep both structs. Replace the two hand-written `From` impls in
  `app/src/agent_tab/profile/mod.rs` with two free functions in the same
  file, `thread_settings_from_defaults` and `defaults_from_thread_settings`,
  so the copy is not disguised as a conversion trait on a foreign type. No
  fields change. This is a locality fix only (rule 2.3, last row).

**`TabDragPreview` / `SidebarTabDragPreview`**, `app/src/ui/tab_bar/drag.rs:12`
and `app/src/ui/workspace_sidebar/drag.rs:20`. Same fields (`label`, `width`),
same purpose; `Render` bodies differ only in height, text size, and colors.

- Change: one `DragLabelPreview { label, width, style: DragStyle }` in
  `ui/tab_bar/drag.rs` with a two-variant `DragStyle`.

### 5.7 Mirrored state (terminal, remote_net, config, sparkle)

Status: **Complete**. ScreenRowRead: `383edac0`; shared remote session
snapshot: `149dbe94`; normalized Rgba: `9257168d`; MsgSender::waker:
`0b0870d8`; Sparkle translation comment: `cf4f75c8`. The two Sparkle channel
types remain unchanged.

**`ScreenRowRead`**, `terminal/src/ghostty/types.rs:217`. Duplicates
`wrapped`, `prompt_start`, `hyperlinks` from `ScreenRowMeta` (`:199`), copied
field by field at `ghostty/mod.rs:915` and `:1024`.

- Change: `struct ScreenRowRead { cells: Vec<RowCell>, meta: ScreenRowMeta }`.

**`ProtocolSessionSnapshot`**, `remote_net/src/protocol/types.rs:33`.
Field-identical to `hub::SessionSnapshot` (`hub.rs:62`) except `u64` versus
the `SessionId` newtype; the conversion at `types.rs:121` unwraps `.0`. Unlike
`ProtocolSessionOptions`, whose narrowing is a documented security boundary,
this mirror drops nothing.

- Change: derive `Serialize` / `Deserialize` on `SessionSnapshot` (with
  `SessionId` serializing as its inner `u64`) and delete the mirror. Run the
  three `remote_net` e2e tests.

**`ColorBuilder` and `Format`**, `config/src/colors/mod.rs:366,66`.
`ColorBuilder` has no builder methods and the same shape as
`render_types::Color`; `Format` is a two-variant option where every
production caller passes `SRGB0_1`.

- Change: after 5.2 removes `render_types::Color`, rename `ColorBuilder` to
  `Rgba`, inline the `SRGB0_1` arm, delete `Format`. `SRGB0_255` survives
  only in `mod.rs:584` and `config/tests/colors.rs`; rewrite those two to
  divide by 255 at the call site.

**`MsgSender::waker`**, `terminal/src/event.rs:129`. `Option<Arc<Waker>>`
that is never `None`: the only constructor wraps its argument in `Some`.

- Change: store `Arc<Waker>`; drop the `if let` in `send`.

**`sparkle::Channel`**, `sparkle/src/lib.rs:43`. Variant-for-variant copy of
`nmt_config::update::UpdateChannel`, translated by hand at
`app/src/sparkle.rs:75-80`. `sparkle` avoids depending on `nmt_config` on
purpose (it is a thin FFI crate).

- Change: none. Keep the copy; add a comment at the translation site naming
  the dependency boundary as the reason. Listed so it is not re-raised.

### 5.8 Single-instantiation generics

Status: **Complete** for all requested changes. ClaudeMaintenance:
`c9b0a5f7`; concrete pane resize state: `e9a75010`; UsageSource:
`3691df3c`. The generics explicitly marked to keep remain in place.

| Type | Location | Change |
|---|---|---|
| `ClaudeMaintenance<C>` | `agent/src/claude_code/update.rs:97` | One production instantiation (`HttpClaudeReleaseChannel`, `app/src/agent_updates/mod.rs:94`), erased to `Arc<dyn ProviderMaintenance>` on the next line. Make it `ClaudeMaintenance { releases: Box<dyn ClaudeReleaseChannel> }`. |
| `PaneTree<L, S>`, `PaneNode<L, S>`, `SplitOutcome<S>`, `RemoveOutcome<S>` | `app/src/pane_tree.rs:115` | `S` is `Entity<ResizableState>` at every non-test use (`terminal_layout.rs`, `ui/persistence.rs`, `ui/shell/panes.rs`). Drop `S`; keep `L`. |
| `UsageSource<T>` | `app/src/usage_refresh.rs:13` | No named implementor; only the blanket impl over `Fn`. Replace with `type UsageSource<T> = Arc<dyn Fn(&AtomicBool) -> Result<T, FetchError> + Send + Sync>`. Callers: `usage_sources.rs:20,52,56`. |
| `TabManager<S>`, `TerminalLayout<L>`, `RemoteSessionHub<S>` | `tabs.rs`, `ui/terminal_layout.rs`, `remote_net/src/hub.rs:301` | Parameters exist for test doubles (`u32`, `tests/hub_supplied_pty.rs`). Keep. Listed so they are not re-raised. |

### 5.9 Re-declared APIs (app)

Status: **Complete**. TerminalLayout: `0c99c04a`; TabManager and
WorkspaceManager: `3b213fad`.

**`TerminalLayout`**, `app/src/ui/terminal_layout.rs:12`. Eight of twelve
methods are single-expression forwards to `self.tree`.

- Change: `pub(crate) fn tree(&self) -> &PaneTree<L>` and delete the eight
  forwards; keep `split`, `remove`, `resize`, `apply_pending_ratios`, which
  add `ResizableState` handling.

**`TabManager<S>`**, `app/src/tabs.rs:114`. Ten methods forward to
`ActiveList` (`activate`, `focus_next`, `focus_prev`, `reorder`, `active_id`,
`active_index`, `len`, `tabs`, `find`, `find_mut`).

- Change: `pub(crate) fn list(&self) -> &ActiveList<Tab<S>>` and
  `list_mut`; delete the ten forwards. `WorkspaceManager`
  (`workspace/mod.rs:144`) has the same shape; apply the same change.

### 5.10 Ownership already enforces once-only

Status: **Complete**, `ca872756`.

**`inbox::Message`**, `app/src/agent_tab/execution/inbox/mod.rs:15`. Wraps
`Option<Value>` so `take()` can `.expect("queued message is consumed once")`.
The only caller owns the `Message` and calls `take()` once.

- Change: the channel carries `Result<Value, String>`; ownership gives the
  once-only guarantee at compile time and the panic path disappears.
  Caller: `execution/startup.rs:224`.

### 5.11 Test doubles with too many layers

Status: **Complete**, `65b8a2bc`. UpdateEnvironment remains unchanged as
specified.

**`FileUserSession` + `FileUserSessionSource` + `ClosePreparation<S>`**,
`app/src/update/mod.rs:422-460`. Two traits, an associated type, and a
generic enum so that `prepare_close_with` can be driven by one test double;
the production impl forwards three methods verbatim to
`RestartManagerSession`, and `on_close_prepared` re-pins
`S = RestartManagerSession` anyway.

- Change: one trait `FileUserSession` with the three methods plus an
  `open(path) -> Result<Self, _>` associated function. Delete
  `FileUserSessionSource`, `SystemSessionSource`, and the type parameter on
  `ClosePreparation`. `update/tests.rs` implements the single trait on its
  scripted double.

**`UpdateEnvironment`**, `app/src/agent_updates/maintenance.rs:24`. Thirteen
methods, one production impl, one test impl. The boundary is real (it is what
makes `run_transaction` testable). No change; listed so it is not re-raised.

### 5.12 Shared core worth extracting, not merging

Status: **Complete**, `5df11ef7`.

**`ControlState`**, `agent/src/claude_code/stream_json/control/mod.rs:31`
and `agent/src/codex/app_server/control.rs:35`. Both carry a monotonic id
counter, a `HashMap<Id, Operation>` of in-flight requests, a `closed` flag,
and the same `track` / `finish` / `close` / `is_closed` semantics where
`track` is a no-op once closed. Claude adds deadlines and effort ordering;
Codex adds query-generation replacement.

- Change: extract `struct PendingRequests<Id, Op>` in `agent/src/subprocess/`
  with the shared core; both `ControlState` types embed it. Do not merge the
  two types.

### 5.13 Naming collisions that are not duplicates

Status: **Complete**. The Codex reducer is ThreadState in `a1ba4f6b`;
the other names listed to keep remain unchanged.

No code change. Rename only where the collision misleads:

- `codex/app_server/conversation.rs:45` `ConversationState` is a Codex
  protocol reducer, unrelated to `transcript/conversation.rs:43`. Rename the
  Codex one `ThreadState`.
- `TurnOutputUsage` (Claude accumulates completed responses; Codex baselines a
  cumulative counter), `Session` (three adapters), `Operation`,
  `QuestionRequest`, `Submission`, `State`: keep as is.

## 6. Ordering

Phase A, independent and low risk, any order, one commit each:
5.2 (all five), 5.3 (all eight), 5.5, 5.7 (except `sparkle`), 5.8, 5.9,
5.10, 5.11, 5.12, 5.13.

Phase B, prerequisites for the moves: 5.1, 5.4 (all four), 5.6.

Phase C, impl locality moves, smallest first so the checker's allowlist
shrinks while the procedure is rehearsed: 4.9 (four types), 4.8, 4.7, 4.6,
4.5, 4.4, 4.3, 4.2.

Phase D (complete in `34ff0eef`): delete
`scripts/impl-locality-baseline.txt` and remove allowlist handling; the
checker now rejects every reported violation.

## 7. Verified non-findings

Checked during the audit and found justified. Do not re-open without new
evidence.

- `AgentCapabilities` and `AgentKindExt` are single-impl traits because
  `AgentKind` is defined in `nmt_profile`; the orphan rule requires them.
- All ten `From` impls in `config/src/appearance.rs` have production
  callers in the settings pages.
- `profiling/src/transcript/{enabled,disabled}.rs`: `--cfg enable_profiling`
  is set by `scripts/profiling.ps1` through `scripts/profiling.toml` into a
  separate `target/profiling` directory. Both halves are live.
- `TurnOutputUsage`, `ConversationState`, `Session`, `ControlState` same-name
  pairs implement different algorithms (see 5.12 and 5.13).
- `WorkflowSource` has one production implementor but is dispatched through
  `dyn` from `Backend::workflow_source`, which returns `None` for two of three
  providers.
- `HasId` has two implementors and one shared `ActiveList<T>` engine.
- `ClipboardAccess` (two methods, one test impl) and `UpdateEnvironment`
  (5.11) are test boundaries that earn their keep.

## 8. Locality checker reference

The implemented checker is
[`scripts/check-impl-locality.py`](../../scripts/check-impl-locality.py).
It handles nested generic bounds, ordinary and unsafe impl headers, and
same-name definitions in different crates. Comments and string contents
are excluded before indexing. Section 2.3 describes the allowed exceptions.

Run the repository scan and its regression suite from the repository root:

```text
python scripts/check-impl-locality.py
python -m unittest discover -s scripts -p test_impl_locality.py -v
```

The default scan exits with status 1 for any reported violation and prints the
implementation location and required destination. `--list` prints the keys
for inspection. Neither mode uses a migration baseline.
