# `crates/` Composition Refactor: Work Orders

Work orders for moving behaviour onto the state objects it operates on. Each
order is a pure structural move with no behaviour change, sized for one commit,
and written so an engineer (or a coding model) with no prior context can carry
it out from this file alone.

Read the whole of sections 1 to 3 before starting any order. Line numbers refer
to the tree at commit `9df0b1f6`; re-locate symbols with `rg` before editing,
because earlier orders shift them.

## 1. Background: what is wrong and what the target shape is

Several large types have their `impl` blocks scattered across many files, one
file per concern, while the type's fields stay flat on the parent. The parent
file is short, but every method still reaches into every field through
`self.<field>`, so the concern boundaries the file split suggests do not exist
in the code. Counts from the current tree:

| Type | `impl` blocks | Files | Lines under `impl` |
|---|---|---|---|
| `AgentPane` (`crates/app_agent`) | 28 | 26 | ~9,600 |
| `Shell` (`crates/app/src/ui`) | 14 | 13 | ~5,600 |
| `TranscriptView` (`crates/app_agent/src/transcript`) | 10 | 9 | ~3,300 |
| `TerminalPane` (`crates/app_terminal/src/view`, `links`) | 8 | 7 | ~2,300 |
| `GhosttyTerminal` (`crates/terminal/src/ghostty`) | 6 | 6 | ~2,400 |
| `ClaudeTasks` (`crates/agent_utils/src/claude_code/tasks`) | 3 | 3 | ~950 |

The state side has already been grouped once: `AgentPane` owns `ThreadControls`,
`TurnState`, `SessionRuntime`, `SlashPalette`, `RewindFlow`, `ForkFlow`,
`WorkflowUi`, `SessionHistoryUi` and more; `Shell` owns `WorkspaceManager`,
`Sidebar`, `TabStrip`. The methods never followed. So most orders below move
methods onto a struct that already exists; only a few create a new one.

Three kinds of file exist, and each has its own correct shape:

1. **Component**: the file owns a small cluster of fields and implements one
   state machine over them (a selection drag, a scrollbar fade, a notification
   card set, a shell index). Fields and methods move together into a struct
   the parent owns; the parent delegates.
2. **Pure projection**: the file only reads state and produces UI or query
   results (`transcript/render/*`, `ghostty/grid_read.rs`, `surface/reads.rs`).
   These become free functions that borrow what they read. Wrapping them in a
   struct adds a layer with nothing inside it.
3. **Orchestrator**: event pumps, the `Render` composition root, command
   routers. They touch every subsystem by design and stay on the parent. They
   shrink as the components around them take over the work.

Two shared dependencies cannot be moved into any component and must be passed
in as parameters instead. Never give a component a back-reference to its
parent.

| Parent | Shared dependency | How to pass it |
|---|---|---|
| `AgentPane` | `kind: AgentKind` (17 files), `runtime: SessionRuntime` (16 files) | `kind` by value; `&SessionRuntime` or the specific field (`epoch`, `status`) by reference |
| `Shell` | `workspaces: WorkspaceManager` (10 of 12 files), `next_id: u64` | `&mut WorkspaceManager`, `&mut u64` (the existing `Shell::alloc_id(&mut u64)` already does this) |
| `GhosttyTerminal` | `terminal: VtTerminal` (raw FFI handle) | by value per call; the handle stays on the parent so `Drop` keeps single ownership |
| `TerminalPane` | `surface: TerminalSurface` | `&TerminalSurface` |

## 2. Repository rules that apply to every order

These come from `CLAUDE.md` and the git hooks in `.githooks`. The hooks reject a
commit that breaks them; do not bypass a hook with `--no-verify`.

**Files and modules**

- Multi-file modules use `foo/mod.rs`. Never leave `foo.rs` beside `foo/`.
  When turning a single file into a directory: `git mv foo.rs foo/mod.rs`
  first, then split, so history stays traceable.
- Keep production code in one file under roughly 800 lines.
- Inline `#[cfg(test)]` modules live in a child file: `#[cfg(test)] mod tests;`
  resolving to `<module>/tests.rs`.
- Every `use` line is anchored at the crate root: `use crate::...` or an
  external crate name. `use super::...` and `use self::...` are forbidden in
  new or edited code. Do not rewrite import lines in files the order does not
  otherwise touch.
- Widen visibility one step at a time (private, `pub(super)`, `pub(crate)`,
  `pub`) and only as far as a real caller needs.

**Comments**

- A comment states the constraint, invariant, tradeoff, or failure mode that
  makes the code appropriate. Never write "moved from X", "extracted for the
  refactor", "the work order says", or any reference to this document.
- Keep the existing doc comments on fields and methods when moving them.
- The pre-commit hook rejects added comments containing implementation
  instruction references, and rejects any added line containing one of the
  seven words listed under "Basic rules" in the root `CLAUDE.md`. The match
  is case-insensitive and whole-word, so those words are unusable in code,
  comments, and commit messages alike.

**Scope**

- Touch only the files the order names plus the call sites that stop
  compiling. Do not tidy adjacent code, comments, or formatting.
- No behaviour change. If a move would require changing what a method does,
  stop and leave a note under the order's status line instead.
- Other agents work in this repository. Never restore or discard changes you
  did not make; run `git status` before starting and only stage your own files.

**Commits**

- One order per commit. Subject: `refactor(<scope>): <lowercase subject>`,
  at most 60 characters. Body lines wrap at 80. Printable ASCII only.
- The body explains what moved where and why the boundary is right. Omit any
  `Verification` section unless you ran the application or a functional test
  that exercises the moved behaviour; the hook rejects a `Verification`
  section that mentions cargo commands, rustfmt, or compilation.
- End with `Co-Authored-By: <model id> <noreply@<vendor>.com>` using the
  model id your runtime reports.
- The pre-commit hook runs rustfmt on staged Rust files, `cargo clippy
  --all-targets --quiet` for the affected workspace members, and a
  first-party clippy pass with `-D clippy::absolute_paths` (so write
  `use gpui::Context;` and `Context<Self>`, never `gpui::Context<Self>` inline).
- Push the `dev` branch to the `private` remote only.

## 3. Standard procedure for a component extraction

Follow this sequence for every order in section 4 unless the order says
otherwise.

1. **Read the parent struct** and the target file end to end. For every method
   in the file, write down which parent fields it reads and writes. The order
   lists the expected set; confirm it against the code.
2. **Classify each method** with this rule:
   - Touches only the component's own fields plus the listed shared
     dependencies: move it onto the component. Shared dependencies become
     parameters.
   - Also calls a parent method that owns other state (`set_command_feedback`,
     `start_session_with_options`, `sync_session_memory`, `focus_active`,
     backend sends): keep a thin wrapper on the parent that calls the
     component method and then performs that side effect. The wrapper is the
     parent's reaction to a result; the component returns the result.
3. **Create or extend the component struct.** Put the struct next to its
   methods. When the struct already exists elsewhere (`pane_state.rs`), move
   the struct definition into the component's module with `git mv` where a
   whole file moves, and plain edits where only a struct moves.
4. **Move the fields** off the parent and into the component. Update the
   parent's constructor. Update every `self.<field>` in other files to
   `self.<component>.<field>` (or to a component method when the access is a
   write that has an obvious verb).
5. **Rewrite the impl block** from `impl Parent` to `impl Component`. Inside
   moved methods `self.<field>` stays as is; `self.<other>` becomes a
   parameter.
6. **GPUI handlers.** A component method that builds UI and needs listeners
   takes `cx: &mut Context<Parent>` and routes the listener back through the
   parent:

   ```rust
   impl ThreadControls {
       pub(crate) fn render_row(&self, kind: AgentKind, cx: &mut Context<AgentPane>) -> AnyElement {
           // ...
           cx.listener(|this: &mut AgentPane, _, _window, cx| {
               this.controls.select_model(/* ... */);
               cx.notify();
           })
           // ...
       }
   }
   ```

   A component method that spawns a task uses the same shape: it takes
   `cx: &mut Context<Parent>`, and the task's `this.update(cx, |this, cx| ...)`
   closure reaches the component through `this.<component>`.
7. **Build and check** with the exact commands in section 6. Fix every
   warning the first-party clippy pass reports in files you touched.
8. **Run the crate's tests.** Update test code that constructed the parent or
   read a moved field; keep assertions unchanged.
9. **Commit** with the subject the order gives.

Stop and record a note instead of proceeding when: a method turns out to touch
fields the order does not list and there is no clean parameter for them; two
files write the same field and the order does not say which one owns it; a
test's assertion would have to change.

## 4. Work orders

Orders are grouped by parent type and listed in the order they should be done
within a group. Groups A to E are independent of each other; orders inside a
group build on the earlier ones unless marked independent.

Each order has a status line; update it as `done <commit hash>`, `blocked:
<reason>`, or `skipped: <reason>`.

### Group A: `AgentPane` (`crates/app_agent`)

The struct is at `crates/app_agent/src/lib.rs:548`. Its already-grouped state
lives in `crates/app_agent/src/pane_state.rs` (`SessionRuntime`,
`ThreadControls`, `TurnState`, `ChildAgents`) and in `lib.rs`
(`SessionHistoryUi` at `:445`, `SlashPalette` at `:521`, `RewindFlow` at
`:113`, `GitBranchPoll` at `:121`).

#### A1. Move command feedback onto `SlashPalette`

Status: pending

`AgentPane::set_command_feedback` (`composer/mod.rs:257`) and
`visible_command_feedback` (`composer/mod.rs:291`) write and read only
`self.palette.feedback` and `self.palette.feedback_seq`, yet they are called
from ten files (`composer/slash.rs` 16 times, `session/conversation.rs` 9,
`composer/rewind.rs` 9, `composer/fork.rs` 9, `session/events.rs` 7, plus
`session/mod.rs`, `session/history.rs`, `session/thread_settings.rs`,
`composer/palette.rs`, `composer/images.rs`). Every one of those is a
cross-file dependency on the composer module for something the palette owns.

Target:

```rust
impl SlashPalette {
    pub(crate) fn set_feedback(
        &mut self,
        kind: CommandFeedbackKind,
        message: impl Into<SharedString>,
        cx: &mut Context<AgentPane>,
    )
    pub(crate) fn visible_feedback(&self) -> Option<&CommandFeedback>
}
```

- The spawned dismissal timer inside `set_feedback` reaches the palette through
  `this.palette` in its `update` closure.
- `is_command_busy` (`composer/mod.rs:298`) stays on `AgentPane`; it reads
  four subsystems.
- Replace every `self.set_command_feedback(k, m, cx)` with
  `self.palette.set_feedback(k, m, cx)`; same for `visible_command_feedback`.
- Move `SlashPalette` itself out of `lib.rs` into `composer/palette.rs`
  (the file that already implements most of its behaviour) only if `lib.rs`
  imports stay simple; otherwise leave it and do the move in A6.

Commit: `refactor(agent): move command feedback onto the slash palette`

#### A2. Consolidate thread controls: rendering and defaults on `ThreadControls`

Status: pending

Four files implement one unit of behaviour over `ThreadControls` (defined at
`pane_state.rs:46`, nine fields: `settings`, `seed_thread_defaults`,
`seed_approval_reviewer`, `restore_on_ready`, `models`, `approval_presets`,
`agent_presets`, `agent_preset`, `effort_drag`):

| File | Lines | Methods | Fields read beyond `controls` |
|---|---|---|---|
| `view/settings_row/mod.rs` | 306 | `render_settings_row`, `model_options`, `folded_settings_pill`, `settings_group`, `settings_pill`, `settings_pill_frame`, `setting_picker` | `kind` |
| `view/settings_row/harness_rows.rs` | 343 | `render_claude_settings_row`, `render_deepseek_settings_row`, `render_codex_settings_row` | `kind` |
| `view/settings_row/effort.rs` | 274 | `effort_levels(kind)`, `effort_panel`, free fn `effort_gauge_step` | none (already field-free) |
| `session/thread_settings.rs` | 145 | `apply_model_selection`, `apply_agent_preset`, `defaults_key`, `stored_thread_settings`, `profile_model`, `profile_effort`, `remember_thread_defaults` | `kind`, `profile` |

Target module `crates/app_agent/src/thread_controls/`:

- `git mv src/view/settings_row/mod.rs src/thread_controls/mod.rs` and move
  the `ThreadControls` struct definition (with its doc comments) from
  `pane_state.rs` into it.
- `git mv src/view/settings_row/harness_rows.rs src/thread_controls/harness_rows.rs`
- `git mv src/view/settings_row/effort.rs src/thread_controls/effort.rs`
- `git mv src/session/thread_settings.rs src/thread_controls/defaults.rs`
- Declare `mod thread_controls;` in `lib.rs`; remove `mod settings_row;` from
  `view/mod.rs` and `mod thread_settings;` from `session/mod.rs`.

Method shape:

- Rendering: `ThreadControls::render_row(&self, kind: AgentKind, cx: &mut Context<AgentPane>) -> AnyElement`.
  Listeners use `this.controls` as in section 3 step 6. `view/mod.rs` calls
  `self.controls.render_row(self.kind, cx)`.
- Defaults: methods that only read `controls`, `kind`, `profile` take
  `kind: AgentKind` and `profile: &AgentProfile` as parameters.
- `apply_model_selection` and `apply_agent_preset`: read their bodies. If
  they send overrides to the backend or call `set_feedback`, keep a
  two-line wrapper on `AgentPane` (`session/mod.rs`) that calls the
  `ThreadControls` method for the state change and then performs the send.
  Callers: `session/events.rs`, `composer/slash.rs`, `session/mod.rs`.
- `effort_gauge_step` keeps its current visibility scope adjusted to the new
  module path.

Pitfall: `pane_state.rs` has a module doc comment describing the four groups
it holds; shorten it to the three that remain.

Commit: `refactor(agent): give thread controls their rendering and defaults`

#### A3. `ComposerAttachments`: images, placeholders, response annotations

Status: pending. Independent of A2.

Three files operate on `attachments: PendingAttachments`
(`composer/attachments/mod.rs:76`) and `response_annotations: Vec<String>`:

- `composer/images.rs` (147): paste, decode, scale, insert `[Image #N]`
  placeholder, keep placeholders in sync with composer text. Reads `input`,
  `attachments`, `kind`, `focus`.
- `composer/response_annotations.rs` (98): add/remove annotation blocks.
  Reads `response_annotations`, `input`.
- `view/attachments.rs` (175): thumbnails and annotation chips. Reads
  `attachments`, `response_annotations`.

Target: a struct `ComposerAttachments { images: PendingAttachments,
response_annotations: Vec<String> }` in `composer/attachments/mod.rs`,
replacing the two parent fields with one `attachments: ComposerAttachments`.
Move the three files' methods onto it; `input: &Entity<InputState>` and
`kind` become parameters; the render method takes `cx: &mut Context<AgentPane>`.
`git mv src/view/attachments.rs src/composer/attachments/render.rs`.

Check `session/turn.rs` and `session/mod.rs`, which read
`response_annotations` when a message is sent and when a turn settles; route
those through a `take_annotations()` / `clear()` method.

Commit: `refactor(agent): fold attachments and annotations into one composer part`

#### A4. Workflow methods onto `WorkflowUi`

Status: pending. Independent of A2, A3.

`workflows.rs` (486) is the only file that touches `self.workflows`
(`WorkflowUi` at `workflows.rs:52`). Its outside reads are `kind`,
`runtime.epoch`, the session id, and `cwd()`, and `workflow_refresh_plan`
already snapshots exactly those into `RefreshPlan` (`workflows.rs:82`).

Target: move every `impl AgentPane` method in the file onto `impl WorkflowUi`,
taking `&RefreshPlan` (or the individual values) as parameters. Methods that
`cx.spawn` take `cx: &mut Context<AgentPane>` and reach the component through
`this.workflows` in the task. `cx.emit(WorkflowActivity)` stays in a thin
parent wrapper if the emit needs `Context<AgentPane>`; otherwise the
component method can emit through the same `cx`.

Commit: `refactor(agent): move workflow bookkeeping onto workflow ui`

#### A5. `PendingPrompts`: approval and question cards

Status: pending. Depends on A1.

`pending_approval: Option<String>` and `pending_questions: Option<QuestionPrompt>`
are written by `session/events.rs`, answered by `session/turn.rs`
(`respond_approval`, `respond_questions`, `handle_question_control`,
`toggle_question_option`), and rendered by `view/banners.rs` (approval card,
question card). Six files read them.

Target: `struct PendingPrompts { approval: Option<String>, questions: Option<QuestionPrompt> }`
in a new `crates/app_agent/src/session/prompts.rs`. Move the four answer
methods and the two card renderers onto it. The answer methods send a
response to the backend; split each into a component method that updates
state and returns the response payload, and a parent wrapper in `turn.rs`
that sends it through `self.runtime`.

While in `session/turn.rs`: `start_working`, `finish_working`,
`note_visible_agent_output`, `note_response_settled` touch only `turn` and
`last_response_at`. Move `last_response_at` into `TurnState` (beside
`first_output_latency`, which has the same lifetime) and move those four
methods onto `impl TurnState`. `view/last_response.rs` then reads
`self.turn.last_response_at`.

Commit: `refactor(agent): own pending prompts and turn timing as parts`

#### A6. Unify rewind and fork as `BranchFlow`

Status: pending. Depends on A1.

`composer/rewind.rs` (654) and `composer/fork.rs` (445) are two variants of
one flow (pick a checkpoint, replace the conversation). They cancel each
other's pickers, and the three predicates `branch_flow_holds_composer`,
`branch_flow_is_working`, `branch_picker_is_open` (`fork.rs:101-133`) read
both `self.rewind` and `self.fork`.

Target: `struct BranchFlow { rewind: RewindFlow, fork: ForkFlow }` in
`composer/branch/mod.rs` (`git mv composer/rewind.rs composer/branch/rewind.rs`,
`git mv composer/fork.rs composer/branch/fork.rs`; move `RewindFlow` out of
`lib.rs:113`). The three predicates and the two picker models
(`rewind_palette_model`, `fork_palette_model`) become `BranchFlow` methods.
The session-replacement half (`start_session_with_options`, `apply_replay`)
stays on `AgentPane`; the component returns what to replay and the parent
does it.

Commit: `refactor(agent): unify rewind and fork under one branch flow`

#### A7. Small closures

Status: pending. Independent.

- `input_history/mod.rs:291-337`: the two remaining `impl AgentPane` methods
  write `self.palette.skill_binding`, `.dismissed`, `.selected` directly.
  Add `SlashPalette::reset_for_recall(&mut self)` and call it.
- `view/session_state.rs` (86): `plan_mode` and `goal` render together and
  nowhere else. Fold into `struct SessionStateBadge { plan_mode: bool, goal: Option<GoalStatus> }`
  with a `render(&self, cx)` method, or leave as is; the payoff is small.

Commit: `refactor(agent): stop reaching into palette state from history recall`

#### Leave on `AgentPane`

`session/mod.rs` (construction, spawn, send pipeline), `session/events.rs`
(the backend event pump), `composer/slash.rs` command routing,
`composer/palette.rs` control routing (`handle_palette_control`,
`activate_palette_index`), and `view/mod.rs` (`Render`). They are the
integration points. After A1 to A6 each `on_*` handler in `events.rs` should
read as a delegation to one component; if it does not, the component boundary
is wrong, and that is worth a note.

### Group B: `Shell` (`crates/app/src/ui/shell`)

Struct at `crates/app/src/ui/shell/mod.rs:129`, 29 fields. Orders B1 to B5 are
independent of each other and together move 13 fields off `Shell` without
touching `workspaces` or `next_id`.

#### B1. `UpdateNotificationLayer`

Status: pending

`updates_layer.rs` (252) owns `update_cards: HashMap<String, UpdateCard>` and
`update_notification_timer_running: bool`; it reads `window_active` once in
the timer closure (`:196`). Nothing else touches those two fields.

Target: `pub(super) struct UpdateNotificationLayer { cards: HashMap<String, UpdateCard>, timer_running: bool }`
in the same file, replacing the two `Shell` fields with one. Methods:
`update_notification_card`, `render_update_notification_layer`,
`ensure_update_notification_timer` move onto it, taking
`cx: &mut Context<Shell>`; the timer closure reads `this.window_active` and
reaches the cards through `this.update_notifications`. `on_show_settings`
stays a `Shell` method and is reached through the listener.

Commit: `refactor(shell): own update notification cards as one layer`

#### B2. `RightPanelController`

Status: pending

`panels.rs` (139) reads and writes `right_panel`, `git_model`,
`workflows_seen`, `background_tasks_seen`; it reaches the rest of `Shell`
only through `active_agent()` / `try_active_pane()`.

Target: `pub(super) struct RightPanelController { panel: Entity<RightPanel>, git_model: Entity<GitStatusModel>, workflows_seen: bool, background_tasks_seen: bool }`
in `panels.rs`. `sync_git_target`, `sync_workflow_panel_target`,
`sync_task_panel_target` take the active pane / agent as a parameter.
`agent_notifications.rs` writes `workflows_seen` and `background_tasks_seen`
from the `watch_agent_tab` subscription; give the controller
`note_workflow_seen()` / `note_background_task_seen()` and call those.
`render.rs` reads the two flags and `right_panel_shows`; route through the
controller.

Commit: `refactor(shell): put right panel targeting under one controller`

#### B3. `SettingsSurface`

Status: pending

`settings_workspace.rs` (116) owns `theme_watcher`, `settings_state`,
`settings_was_active`. Two leaks: `panes.rs:212` reads `settings_state` to
render the settings surface; `mod.rs:471-474` (`focus_active`) reads
`settings_was_active`.

Target: `pub(super) struct SettingsSurface { state: Option<Entity<SettingsState>>, theme_watcher: Option<Task<()>>, was_active: bool }`
with `open`, `retire`, `leave`, `render_target(&self) -> Option<&Entity<SettingsState>>`,
`was_active(&self) -> bool`. The `on_show_settings` action handler stays on
`Shell` (it also opens a tab through `workspaces` and `next_id`) and calls
the component.

Commit: `refactor(shell): keep settings surface state in one place`

#### B4. `RootAvailability`

Status: pending

`workspace_dirs.rs` (484) is two things: `WorkspaceDirsEditor` (already its
own entity; leave it) and a `Shell`-side availability cache over
`unavailable_roots: HashSet<String>` refreshed by a background task.

Target: `pub(super) struct RootAvailability { unavailable: HashSet<String> }`
with `refresh(&mut self, roots: Vec<String>, cx: &mut Context<Shell>)` and
`is_available(&self, path: &str) -> bool`. `edit_workspace_dirs`,
`replace_workspace_roots`, `sync_agent_workspaces` stay on `Shell` (they
mutate `workspaces`). Consider `git mv workspace_dirs.rs workspace_dirs/mod.rs`
with the editor in `workspace_dirs/editor.rs` if the file passes 800 lines
after the change; otherwise leave the layout.

Commit: `refactor(shell): track root availability as its own state`

#### B5. `InlineRenameSession`

Status: pending

In `workspaces.rs` (489) the rename half (`rename_input`,
`start_workspace_rename`, `finish_workspace_rename`, `start_tab_rename`,
`finish_tab_rename`, and their fields `workspace_rename`, `tab_rename`) is
separable; the navigation and CRUD half stays coupled to `workspaces` and
`next_id`.

Target: `pub(crate) struct InlineRenameSession { workspace: Option<(WorkspaceId, Entity<InputState>)>, tab: Option<(TabId, Entity<InputState>)> }`
in a new `shell/rename.rs`. `finish_*` needs `&mut WorkspaceManager` to apply
the name; pass it. `render.rs` reads both fields to draw the input in place
of the name; expose `workspace_input(&self, id) -> Option<&Entity<InputState>>`
and the tab equivalent.

Commit: `refactor(shell): hold inline renames in one session object`

#### B6. `persistence.rs` as a free-function store

Status: pending. Independent.

`ui/persistence.rs` (745) already writes almost everything as associated
functions taking `&mut WorkspaceManager` and `&mut u64`. Only
`session_state` and `sync_session_memory` (`:634`) read `self` (`workspaces`,
`doomed_workspace`, `window_id`). Turn the file into a module of free
functions (`session_state(&WorkspaceManager, doomed: Option<WorkspaceId>, cx)`,
`sync(window_id, ..)`), and keep a one-line `Shell::sync_session_memory`
wrapper because it has 20+ callers.

Commit: `refactor(shell): make session persistence a plain module`

#### B7. Command files as free functions over `WorkspaceManager`

Status: pending. Do after B1 to B6.

`tabs_open.rs` (277), `panes.rs` (303), `pump.rs` (176) own no state; each
is a command surface over `workspaces` and `next_id` plus the helper trio
`focus_active`, `sync_session_memory`, `register_agent_pane`. Leave them as
`impl Shell` blocks. The one improvement worth making: `panes.rs:212` should
stop reading `settings_state` directly after B3, and `pump.rs` should call
`RightPanelController` after B2.

No commit of its own; it is the follow-through check for B2 and B3.

#### Leave on `Shell`

`render.rs` (the frame loop and pending-work drain), `close.rs` (needs
dialogs, `workspaces`, and three helpers), `agent_notifications.rs`
(`watch_agent_tab` is an event router that pokes four unrelated fields; keep
it thin and let B2 absorb the panel flags).

### Group C: view layer (`crates/app_terminal`, `crates/app_agent/src/transcript`)

Orders C1 to C3 are independent of everything else in this document and are
the lowest-risk starts.

#### C1. `SurfaceSelection` (`crates/app_terminal/src/surface/selection.rs`)

Status: done b7bef98e

Note: `frozen_selection_range` stayed on `TerminalSurface`. It reads a block
out of the engine and never touches the selection anchor, so moving it onto
the component would add a method that ignores its own state. The gesture
methods became testable without a session, so the file moved to
`surface/selection/mod.rs` with `surface/selection/tests.rs` beside it.
Manual check: drag across terminal output and press the copy key; the
highlight follows the pointer, the copied text matches, and the highlight
clears on copy.

`TerminalSurface` (`surface/mod.rs:38`, 7 fields) is GPUI-free and uses
interior mutability. `selection.rs` (287) touches `selection: Mutex<Option<Selection>>`
and `session` (through `viewport_top` and the engine). Methods:
`apply_screen_selection`, `frozen_selection_range`, `selection_range`,
`selection_screen_range`, `selection_range_at`, `clear_selection`,
`apply_selection_at`, `begin_selection`, `update_selection`,
`finish_selection`, `selection_text`, `copy_selection`; free functions
`selection_screen_range`, `block_selection_range` stay free.

Target: `pub(crate) struct SurfaceSelection { selection: Mutex<Option<Selection>> }`
in the same file. Methods take `engine: &Engine` (or whatever `self.session`
accessor they use today) and `viewport_top: i32`. `mouse.rs` calls
`apply_selection_at`; route it as `self.selection.apply_at(&self.session..., ..)`.
Add a unit test file `surface/selection/tests.rs` only if the moved methods
become testable without a session; otherwise skip tests.

Commit: `refactor(terminal-view): own selection state on the surface`

#### C2. `ScrollbarActivity` (`crates/app_terminal/src/view/scroll.rs`)

Status: done 53eebc95

Note: `opacity` returns `Option<f32>`, not `f32`. `None` is what tells the
render path the bar has faded out and must not be painted at all, so the
sketched signature would have changed behaviour. Added `thumb_top_for` for the
two sites that subtracted the grab offset by hand. The suggested subject is 68
characters and the commit-msg hook caps subjects at 60, so it shipped as
`own scrollbar drag and fade state`.
Manual check: drag the terminal scrollbar thumb; it follows the pointer and
fades out about half a second after release.

Four `TerminalPane` fields exist only to drive scrollbar opacity and thumb
drag: `scrollbar_dragging`, `scrollbar_grab`, `last_scroll_activity`,
`scroll_activity_gen`. `scroll.rs` (143) writes the last two;
`scrollbar/mod.rs:56-78` writes the first two from an element closure, which
is a fourth file mutating pane internals from outside `view/`.

Target: `pub(crate) struct ScrollbarActivity { dragging: bool, grab: f32, last_activity: Option<Instant>, activity_gen: u64 }`
in `view/scroll.rs`, with `mark_activity(&mut self, cx: &mut Context<TerminalPane>)`,
`begin_drag(&mut self, grab: f32)`, `end_drag(&mut self)`, `is_dragging(&self)`,
`opacity(&self, now) -> f32`. The `scrollbar/mod.rs` closure becomes
`this.scrollbar.begin_drag(fraction - thumb_top)` and the drag-move check
`this.scrollbar.is_dragging()`. `scroll_thumb_to` and `scroll_to_latest` stay
on `TerminalPane` (they move `block_list` and `surface`).

Commit: `refactor(terminal-view): keep scrollbar drag and fade state together`

#### C3. `LinkHover` (`crates/app_terminal/src/links/mod.rs`)

Status: pending

`hovered_link: Option<LinkHit>` and `last_mouse_position: Option<Point<Pixels>>`
form a two-field state machine with one write site (`links/mod.rs:102`, plus
`view/mouse.rs:261` clearing it). The URL scanners (`url_at_col`,
`is_url_char`, `open_allowed`) are already pure.

Target: `pub(crate) struct LinkHover { hit: Option<LinkHit>, last_position: Option<Point<Pixels>> }`
in `links/mod.rs`, with `update(&mut self, position, hit)`, `clear(&mut self)`,
`current(&self) -> Option<&LinkHit>`. The scan that produces the hit takes
`&TerminalSurface`, `frozen_hit`, and the row offsets as parameters.

Commit: `refactor(terminal-view): own link hover state`

#### C4. `FrozenSelectionDrag` (`crates/app_terminal/src/view/mouse.rs`)

Status: pending. Do after C2.

`frozen_selection`, `frozen_select_anchor`, `selection_drag_origin` are
written only in `mouse.rs` (anchor, move, commit) and cleared once in
`input.rs:81`. Target: `pub(crate) struct FrozenSelectionDrag { selection: Option<(FrozenPoint, FrozenPoint)>, anchor: Option<FrozenPoint>, drag_origin: Option<Point<Pixels>> }`
with `begin`, `extend`, `commit`, `clear`, `current`. The `BlockListPoint`
resolver and `&TerminalSurface` are parameters. `blocks.rs` and `input.rs`
read `frozen_selection` for copy and `bounds_for_range`; route through
`current()`.

Commit: `refactor(terminal-view): own the frozen selection drag`

#### C5. `TerminalPane::blocks.rs` field ownership

Status: pending. Do after C4.

Two moves, both into types that already exist under `block_list/`:

- `last_list_measure_key` into `BlockListState` (`block_list/reconcile.rs:4`);
  `plan_remeasure` (`blocks.rs:493`) is its only consumer.
- `selected_frozen_item`, `frozen_chrome`, `frozen_separators`, `frozen_hit`
  into `struct FrozenGutterSelection` next to `FrozenHitInfo`
  (`block_list/selection.rs:23`). They are the paint record and selection
  pair; the jump/copy/re-run actions in `blocks.rs` read them together.

Commit: `refactor(terminal-view): move frozen gutter state beside its types`

#### C6. `TranscriptView` disclosures

Status: pending. Independent of C1 to C5.

`TranscriptView` (`transcript/view.rs:36`, 25 fields). `reveal/mod.rs` (495)
holds `impl Reveals` (own struct, fine) and `impl TranscriptView` disclosure
toggling that writes `expanded_rows`, `expanded_groups`,
`expanded_annotations` (written nowhere else), `revealed_heights`, and
`reveals`. Two leaks: `:341` removes from `virtual_transcripts` when a row
collapses, and `:459` writes `revealed_heights` from a prepaint closure
through a `WeakEntity<TranscriptView>`.

Target: `pub(crate) struct Disclosures { expanded_rows: HashSet<usize>, expanded_groups: HashSet<usize>, expanded_annotations: HashSet<usize>, reveals: Reveals, revealed_heights: HashMap<RevealedPart, Pixels> }`
as a plain owned field (never a separate `Entity`, so the prepaint closure can
do `this.disclosures.record_height(part, h)`). `toggle_disclosure` returns
which row collapsed so the parent drops the virtual transcript. `render/*`
calls `is_disclosing` from four files; it becomes `self.disclosures.is_disclosing(..)`.

Commit: `refactor(transcript): own disclosure state as one part`

#### C7. `LiveTurn` and `TurnLedger` (`transcript/view.rs`)

Status: pending. Do after C6.

`working_started`, `working_output_tokens`, `working_detail`, `compacting`
change together on `start_working` / `settle_turn`; `settled_turns`,
`completed_turn_seconds`, `completed_turn_output_tokens`, `interrupted_turns`
are the per-turn ledger. Two structs, `LiveTurn` and `TurnLedger`, in
`transcript/view.rs` (or `transcript/turns.rs` if `view.rs` passes 800
lines). `clear()` (`view.rs:216-236`) becomes four component resets.
`rows.rs` reads all eight when building row specs; pass `&LiveTurn` and
`&TurnLedger`.

Commit: `refactor(transcript): group live turn and turn ledger state`

#### C8. Render files as free functions

Status: pending. Do last in group C.

`transcript/render/text_style.rs` (56) has zero `self` uses;
`render/turn_rows.rs`, `render/compaction_row.rs`, `render/user_row.rs`,
`render/mod.rs` are read-only projections. Convert them from `impl TranscriptView`
methods to free functions that borrow `&[Entry]`, `&Disclosures`, `&LiveTurn`,
`kind`, `cwd` and take `cx: &mut Context<TranscriptView>` where they build
listeners. `render/work_row.rs` is the one render file that mutates
(`virtual_transcripts.entry` at `:223`, `.remove` at `:314`); give
`virtual_transcripts` a `VirtualTranscriptCache` with `ensure` / `drop` first,
then convert the rest. Start with `text_style.rs` as a one-commit warm-up.

Commit (per file or per pair): `refactor(transcript): render <row> as a free function`

#### Leave alone

`transcript/rows.rs` (the join point of every state set), `view/mod.rs` and
`view/blocks.rs` in `app_terminal` (both `Render` bodies mutate state),
`surface/mod.rs`, `surface/reads.rs` (a thin read-through that arguably
belongs on `TerminalSession`; a separate decision), `surface/mouse.rs`,
`surface/input.rs`, `surface/scroll.rs` (already pure or trivial).

### Group D: `GhosttyTerminal` (`crates/terminal/src/ghostty`)

Struct at `ghostty/mod.rs:72`. The FFI handle `terminal` stays on the parent
and is passed by value into every component method.

#### D1. `RenderStateReader`

Status: pending

`render_state.rs` (371) exclusively owns `render_state`, `row_iter`,
`row_versions`, `content_revision` (the last three appear nowhere else except
`new` and `Drop`). Target: `struct RenderStateReader { render_state: VtRenderState, row_iter: VtRenderStateRowIterator, row_versions: Vec<u64>, content_revision: u64 }`
with `update(&mut self, terminal: VtTerminal, rows, cols)`, `cursor`, `colors`,
row visiting. `snapshot_into` (`render_state.rs:75`) is an orchestrator that
also calls `color_palette`, `grid_ref_at`, `placements`, `scrollbar`; it stays
on `GhosttyTerminal` and drives the reader. `Drop` frees the iterators; move
that into `impl Drop for RenderStateReader`.

Commit: `refactor(terminal): own render state and row damage together`

#### D2. `KittyState`

Status: pending. Independent of D1.

`kitty.rs` (484) owns `placement_iter` and `shipped_images` (mutated nowhere
else). Target: `struct KittyState { placement_iter: VtKittyGraphicsPlacementIterator, shipped_images: FxHashMap<u32, (u32, u32, usize)> }`
with `placements(&mut self, terminal)` and `block_placements(&mut self, terminal, block: &BlockRef)`.
Callers: `render_state.rs:108`, `block.rs:330`. Free functions
`placement_scalar`, `placement_geometry`, `kitty_image_graphic_data` stay free.

Commit: `refactor(terminal): own kitty placement iteration and image cache`

#### D3. `TitleMirror`

Status: pending. Independent.

`last_title` and `last_pwd` in `mod.rs` are change-detection mirrors read only
by `poll_title` / `poll_pwd`. Target: `struct TitleMirror { title: String, pwd: String }`
with `poll_title(&mut self, terminal) -> Option<String>` returning the new
value when it changed. Two fields; small but clean.

Commit: `refactor(terminal): keep title and pwd change detection together`

#### D4. Read-only files as free functions

Status: pending. Do after D1, D2.

`grid_read.rs` (380), `format.rs` (107), `block.rs` (393) own zero mutable
state; `visit_row_cells` (`grid_read.rs:123`) already takes no `self`.
Convert them to free functions over `VtTerminal` (plus `cols` where needed).
`BlockRef` / `AcquiredBlock` already carry per-block state and stay as they
are.

Do not touch `scrollbar_override`: its heuristic (`mod.rs:507`) calls
`format_text` and `scroll_viewport_top_raw`, which is a viewport policy
reaching into the text formatter. Record it as a design question; it needs a
decision, and a structural move would just relocate the dependency.

Commit: `refactor(terminal): read grid, blocks, and formatting as free functions`

### Group E: `ClaudeTasks` (`crates/agent_utils/src/claude_code/tasks`)

Struct at `tasks/mod.rs:80`, 12 fields. `registry: Option<BackgroundTaskRegistry>`
is the shared dependency. The type is a facade over the registry plus three
side indexes; the orders extract the three.

#### E1. `ShellIndex`

Status: pending

`shells.rs` (150) owns `shell_meta`, `shell_meta_order`, `bash_commands`,
`bash_command_order` (with `MAX_SHELL_META` and the LRU bound). Methods
`remember_shell`, `remember_handoff_output_file`, `remember_output_file`,
`remember_bash_command`, `reserve_shell_meta`, `shell_command` move onto
`pub(super) struct ShellIndex { .. }`. `is_shell` (`:122`) and `shell_detail`
(`:132`) also read the registry; keep them on `ClaudeTasks` and have them call
the index.

Pitfall: `observe.rs` (`observe_background_snapshot`) touches `shell_meta`
directly as well as calling `reserve_shell_meta`. Replace the direct access
with an index method before moving the field.

Commit: `refactor(claude): own background shell metadata as an index`

#### E2. `ChildTranscripts`

Status: pending. Do after E1.

`pending_transcripts`, `open_child_tools`, `launch_prompts` form one unit:
open a child's conversation, accumulate items, dedupe the launch prompt,
drain. Written in `observe.rs`; `mod.rs:214` pushes restored transcripts and
`take_transcripts` drains. Target: `pub(super) struct ChildTranscripts { .. }`
with `open`, `push`, `push_restored`, `drain`. `observe.rs` stays an
`impl ClaudeTasks` router (one method per record shape) and delegates.

Commit: `refactor(claude): keep child transcript accumulation in one place`

#### E3. `AliasTable`

Status: pending. Do after E2.

`aliases`, `alias_order`, `MAX_ALIASES`, `link_all`. `canonical()`
(`mod.rs:351`) straddles alias lookup and registry membership: make
`AliasTable::lookup(&self, id) -> Option<&str>` and leave the registry
fallback in `ClaudeTasks::canonical`. `set_session` (`mod.rs:239`) clears
every collection; after E1 to E3 it calls three `clear()`s plus the
remaining fields.

Commit: `refactor(claude): own task id aliases as a bounded table`

### Group F: recorded, no order issued

These need a design decision before a structural move helps. Leave them.

- `crates/app/src/ui/background_tasks/`: `mod.rs` and `detail.rs` share five
  of seven fields; `PanelMode::Detail` embeds the saved list expansion; and
  `sync_elapsed_timer` (mod.rs) and `refresh_open_child` (detail.rs) call
  each other across the split. Fix order if picked up later: lift the two
  expansion bools out of the enum, invert the timer so it only reports a
  tick, then extract `Detail { mode, transcript }`.
- `crates/agent_utils/src/claude_code/stream_json/mod.rs` (1453): a cohesive
  protocol state machine. One separable piece is the control-request
  correlator (`next_request_id`, `pending_control_operations`, `send_control`
  `:886`, `process_control_request` `:1269`, `process_control_response`
  `:1338`), ~200 lines with no turn semantics.
- `crates/agent_utils/src/deepseek/session.rs` (1533): lines 128-624 are ~500
  lines of catalog-loading free functions (`load_sessions`, `load_search`,
  `load_commands`, `load_skills`, `load_agent_presets`, `load_subagents`,
  `load_workflow_transcript`, `load_replay`, `load_fork_checkpoints`,
  `load_models`) that all spawn a unary API call and push an `nmt/*` frame.
  They would move as a module owning `client`, `session_id`, `deliver`, and
  the `*_FRAME` constants.
- `GhosttyTerminal::scrollbar_override` (see D4).

## 5. Suggested execution order

1. C1, C2, C3 (small, GPUI-light or GPUI-free, independent): learn the
   procedure on these.
2. A1 (one move that deletes the most common cross-file call in `app_agent`).
3. B1, B2, B3, B4, B5 in any order; E1.
4. A2 (largest single payoff: ~1,070 lines, three inputs, no async).
5. A3, A4, D1, D2, D3, E2, E3.
6. A5, A6, C4, C5, C6, C7.
7. B6, C8, D4 (shape changes to free functions).

## 6. Verification commands

Run from the repository root after every order, before committing. Replace
`<crate>` with the package name from the crate's `Cargo.toml` (`nmt_app_agent`,
`app`, `nmt_app_terminal`, `nmt_terminal`, `nmt_agent_utils`).

```sh
cargo clippy --all-targets --quiet -p <crate>
cargo clippy --quiet -p <crate> -- -D clippy::absolute_paths
cargo test --quiet -p <crate>
cargo build --quiet -p app
git status --short        # only the order's files may appear
```

The application is a Windows GUI process. Manual checks belong to the
repository owner; when an order changes something visible, list under its
status line what to click and what should happen (for example, after C2:
"drag the terminal scrollbar thumb; it follows the pointer and fades after
release"). Launch for manual checks with `target\debug\NiumaTerm.exe --testing`.

## 7. Status

Update this section as orders complete: order id, commit hash, and any note
left under the order.

| Order | Status |
|---|---|
| C1 | done `b7bef98e` |
| C2 | done `53eebc95` |
