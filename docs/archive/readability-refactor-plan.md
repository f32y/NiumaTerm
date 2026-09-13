# Readability refactor plan for `crates/`

Scope: every crate under `crates/`, production code only (`*_tests.rs`,
`tests/` and `#[cfg(test)]` modules excluded). Line numbers were verified
against the working tree on 2026-09-13 (branch `dev`, HEAD `47e2b70f`).

## 1. What this plan enforces

Commit `998264d0` (`refactor: refine codebase`) established two habits in
`crates/app/src/main.rs`. This plan applies the same two habits across the
rest of the workspace.

### Habit A — flatten the entry, name the handlers

| Code | Rule | Example from `998264d0` |
|------|------|-------------------------|
| A1 | A one-line wrapper whose whole body is a call to a sibling with a fixed argument is removed; callers call the real function. | `parse_startup_args()` deleted; `main()` calls `parse_startup_args_from(env::args_os())`. |
| A2 | A `run_*` / `*_impl` / `*_inner` layer with a single caller and no generic or test reason is inlined into its entry. | `run_app(url, testing, profiling)` inlined into `main()`. |
| A3 | A closure of roughly 15+ lines passed to a registration or spawn API becomes a named function; the registration site becomes one line. | `cx.observe_global(on_settings_changed)`, `cx.on_window_closed(on_window_closed)`, `cx.on_app_quit(on_app_quit)`. |
| A4 | The entry function of a file comes first; the private helpers it calls follow it. | `parse_startup_args_from` and the three `on_*` functions moved below `main()`. |

### Habit B — names that say what they are

| Code | Rule | Example from `998264d0` |
|------|------|-------------------------|
| B1 | Functions that react to an event share one prefix inside a module: `on_<event>`. | `dispatch_cli_action` → `on_ipc_cli`, `dispatch_agent_event` → `on_ipc_agent_hook`. |
| B2 | A local that holds a settings field carries the field's full name. | `restore_session` → `restore_last_session_when_opening`, `smooth_panels` → `enable_smooth_scrolling`. |
| B3 | Accessor naming is consistent inside a module (either all `get_*` or none). | `last_active_shell` → `get_last_active_shell`. |

Note on B3: outside `main.rs` the workspace is almost uniformly bare-noun
(`title()`, `profile()`, `session_id()`); `crates/agent` and
`crates/app/src/agent_tab` contain zero `get_*` functions. The B3 findings
below therefore go the other direction — remove the few stray `get_*` so each
module is internally consistent — rather than spreading `get_*` everywhere.

## 2. Overview

Counts of verified findings per crate and category:

| Crate / area | A1 | A2 | A3 | A4 | B1 | B2 | B3 |
|--------------|---:|---:|---:|---:|---:|---:|---:|
| app — main, update, workspace, misc | 6 | 0 | 11 | 3 | 2 | 3 | 1 |
| app — agent_tab, agent_updates, agent_usage | 3 | 0 | 17 | 3 | 2 | 4 | 0 |
| app — ui, window, shell, sidebar, tab_bar | 1 | 0 | 13 | 4 | 3 | 5 | 0 |
| agent | 2 | 0 | 16 | 1 | 5 | 3 | 0 |
| terminal | 5 | 0 | 2 | 0 | 1 | 0 | 1 |
| platform | 1 | 0 | 6 | 1 | 1 | 0 | 3 |
| config | 5 | 0 | 0 | 0 | 0 | 0 | 0 |
| remote_net, remote_session_hub | 0 | 0 | 7 | 0 | 0 | 0 | 0 |

Three conclusions drive the ordering of the work:

1. **A3 is the dominant pattern (72 sites).** Handler bodies of 15–122 lines
   are inlined into `cx.spawn`, `cx.observe*`, `cx.subscribe`,
   `window.open_dialog`, `thread::spawn`, `tokio::select!` arms and
   immediately-invoked closures. Constructors and loaders are the usual
   victims: `new()` is a struct literal plus a 40-line worker loop.
2. **A1 has a large "delete, don't inline" subset.** Nine wrappers have zero
   production callers; they should be removed rather than inlined.
3. **A2 has no instances anywhere.** Every `*_inner` / `*_with` layer found
   has a second caller, a `?`-propagation reason, or exists so tests can
   inject a path or clock. Nothing to do.

## 3. Priority order

### P0 — finish what `998264d0` started (same file, same idea, trivial)

| Location | Code | Change |
|----------|------|--------|
| `crates/app/src/main.rs:451` | B2 | `smooth_panels` → `enable_smooth_scrolling` inside `on_settings_changed`; `main()` already uses the new name for the same read at line 256. |
| `crates/app/src/main.rs:533` | B2 | `save_session` → `restore_last_session_when_opening` inside `on_app_quit`. |
| `crates/app/src/main.rs:769` | B1 | `dispatch_focus_notification` → `on_ipc_focus_notification`; it is the third arm of the same `IpcAction` dispatch as `on_ipc_cli` and `on_ipc_agent_hook`. |
| `crates/app/src/window.rs:134` | B2 | Parameter `restore_session` of `from_local_state` → `restore_last_session_when_opening`; the caller side was renamed, the callee was not. |
| `crates/app/src/window.rs:145` | B2 | Parameter `save_session` of `to_local_state` → same field name. |
| `crates/app/src/sparkle.rs:64`, `crates/app/src/update/mod.rs:725` | B1 | `settings_changed` → `on_settings_changed` in both; each is called only from `main::on_settings_changed` as a fan-out of that event. |

### P1 — delete dead wrappers (zero production callers, verified with `rg`)

| Location | Wrapper | Notes |
|----------|---------|-------|
| `crates/config/src/lib.rs:390` | `save_settings(patch)` | App imports `save_settings_to` directly. The name also collides with `ui::settings::save_settings`, which misleads a grep. |
| `crates/config/src/local_state.rs:214` | `load()` | Startup uses `try_load()`. Delete together with `load_from` (line 218, test-only). |
| `crates/config/src/local_state.rs:241` | `save(state)` | All writers go through `save_windows` / `save_agent_defaults`; a whole-file `save` next to them invites a lost-update bug. |
| `crates/terminal/src/session/mod.rs:617, 637, 703, 760` | `screen_page`, `screen_row_text`, `selected_text`, `selection_screen_range` | Each is `self.<name>_in(&self.snapshot(), ..)`; zero callers, tests included. |
| `crates/terminal/src/session/mod.rs:734` | `selection_range` | Same shape; only `terminal_tab/pane_model/tests.rs:365` calls it — point the test at the `_in` form. |
| `crates/agent/src/codex/app_server/mod.rs:389` | `send_user_message(text, settings)` | `session/backend/mod.rs:155` routes Codex through `send_user_message_with_skill`; nothing calls the two-argument form. |
| `crates/app/src/agent_tab/session/mod.rs:119` | `AgentPane::new(profile, workspace, window, cx)` | Body is `Self::new_resuming(.., None, ..)`; only tests call it, production goes through `AgentPane::attach`. Either delete and update the nine test sites, or keep it `#[cfg(test)]`. |

### P1 — inline single-caller fixed-argument wrappers

| Location | Wrapper | Call site(s) |
|----------|---------|--------------|
| `crates/config/src/lib.rs:236` | `Config::load_for_startup()` → `load_for_startup_from(&config_file_path(), &config_dir_path())` | `crates/app/src/main.rs:555`. This is the literal twin of `parse_startup_args`. |
| `crates/config/src/lib.rs:121` | `selected_config_dir(path)` → `config_dir_for_mode(path, TESTING_MODE.load(..))` | `config_dir_path()` at line 140. |
| `crates/app/src/update/mod.rs:528` | `prepare_close(path)` → `prepare_close_with(&SystemSessionSource, path)` | line 492. |
| `crates/app/src/update/mod.rs:368` | `retry_file_use(cx)` → `inspect_file_users(cx)` | `update/file_users.rs:67`. |
| `crates/app/src/update/mod.rs:197` | `check_now(cx)` → `check(cx)` | `ui/shell/render.rs:467`, `ui/settings/about_page.rs:111`; make `check` `pub(crate)`. |
| `crates/app/src/update/install.rs:215` | `discard_previous(install)` → `discard_previous_files(install)` | Exists only to undo the `as discard_previous_files` rename at the import (line 21); import under the real name instead. |
| `crates/app/src/utils.rs:8` | `get_data_dir()` → `nmt_platform::environment::data_dir()` | `logging.rs:15`, `remote.rs:70/130/134`; replace with a `pub use` or call `data_dir()` directly. |
| `crates/app/src/workspace/mod.rs:290` | `new_workspace(tabs, id, name, roots)` → `new_workspace_of_kind(.., Some(roots), WorkspaceKind::Normal)` | `ui/shell/workspaces.rs:310`, `workspace/mod.rs:387`. |
| `crates/app/src/agent_tab/execution/mod.rs:190` | `SessionOwner::create(..)` → `create_with_team(.., None, ..)` | 3 callers; rename `create_with_team` to `create` and drop the wrapper. |
| `crates/app/src/agent_tab/session/mod.rs:540` | `send_text(text, cx)` → `send_text_inner(text, None, None, cx)` | `composer/slash.rs:86` only. |
| `crates/app/src/ui/settings/opacity.rs:50` | `effect_on_content_area(cx)` → `cx.global::<AppSettings>().appearance.transparent_main_view` | line 56 only; the wrapper name hides the field it reads (also B2). |
| `crates/agent/src/codex/app_server/mod.rs:302` | `detach()` → `detach_with(250ms, true)` | `Drop::drop` at line 1192 only. |
| `crates/platform/src/unix/process.rs:172` | macOS `group_process_count(pgid)` → `process_group_count(pgid)` | Replace with `use crate::unix::macos::process_group_count as group_process_count;` under the same `cfg`. |

Deliberately kept (documented two-variant public API, not a shim):
`crates/platform/src/unix/mod.rs:440 create_pty_with_env`.

### P2 — extract inline handlers (A3), largest first

Each row: the registration site, the closure length, and the proposed named
function. The registration line stays where it is; the new function goes
directly below the function that registers it.

#### app — update flow and terminal view

| Location | Lines | Extract to |
|----------|------:|------------|
| `crates/app/src/update/file_users.rs:20` | 122 | `fn build_file_use_prompt(window, prompt, cx)`; the `handle.update` call becomes a one-liner. |
| `crates/app/src/update/file_users.rs:39` | 102 | Inside the above: `fn file_use_footer(reason) -> DialogFooter` and `fn file_use_body(..)`. |
| `crates/app/src/update/file_users.rs:151` | 33 | `fn build_recovery_dialog(window, applications, cx)`. |
| `crates/app/src/update/mod.rs:489` | 37 | `fn on_close_prepared(prepared, cx: &mut AsyncApp)`; `cx.spawn` keeps only the await. |
| `crates/app/src/update/mod.rs:794` | 19 | `fn finish_check(found, channel, cx)`. |
| `crates/app/src/update/mod.rs:764` | 16 | `fn check_if_enabled(cx)`; separates the interval from the per-tick policy. |
| `crates/app/src/usage_sources.rs:26` | 22 | `fn fetch_daily_usage(_: &AtomicBool) -> Result<..>` registered via `Arc::new(fetch_daily_usage)`. |
| `crates/app/src/terminal_tab/view/mod.rs:181` | 27 | `fn on_terminal_settings_changed(&mut self, cx)`; `cx.observe_global::<TerminalSettings>(Self::on_terminal_settings_changed)`. |
| `crates/app/src/terminal_tab/view/mod.rs:209` | 21 | `fn on_wake(&mut self, wake, cx)`; the spawn keeps the receiver loop. |
| `crates/app/src/terminal_tab/view/mod.rs:486` | 26 | `fn on_copy_finished(&mut self, text, completion, window, cx)`. |
| `crates/app/src/terminal_tab/view/mod.rs:911` | 25 | `fn attach_image_releases(&mut self, window, cx)`; one-time subscription setup currently opens `render`. |

#### app — agent_tab, agent_updates, agent_usage

| Location | Lines | Extract to |
|----------|------:|------------|
| `crates/app/src/agent_tab/execution/startup.rs:165` | 113 | `async fn pump_backend_messages(this, batches, spawned, epoch, name, on_result, cx)`; install step, batched drain, frame-budget break and exit epilogue are three concerns in one body. |
| `crates/app/src/agent_tab/session/history/mod.rs:123` | 66 | `async fn load_history_passes(this, request, scope, cwd, cx)`; the two-pass protocol described in the doc comment lives entirely inside the closure. |
| `crates/app/src/agent_updates/transaction.rs:67` | 62 | `fn active_work_dialog(key, busy) -> impl Fn(Dialog, ..)`; `request_update` shrinks to its busy/not-busy decision. |
| `crates/app/src/agent_tab/transcript/code/mod.rs:181` | 55 | `async fn parse_loop(view, cx)`; the revision-supersession rule is the real content of `start_parse`. |
| `crates/app/src/agent_tab/composer/mod.rs:236` | 43 | `fn cache_expiry_dialog(pane, idle) -> impl ..`. |
| `crates/app/src/agent_usage.rs:570` | 41 | `fn render_usage_details(codex, claude, .., cx) -> impl IntoElement`; hover-card body nested seven levels inside `render`. |
| `crates/app/src/agent_tab/execution/children.rs:126` | 40 | `async fn poll_watched_children(this, cx)`. |
| `crates/app/src/agent_updates/transaction.rs:179` | 34 | `async fn drive_transaction(key, mode, sessions, coordinator, cx)`. |
| `crates/app/src/agent_updates/mod.rs:194` | 32 | `async fn run_automatic_checks(cx)`; `cx.spawn(run_automatic_checks).detach()`. |
| `crates/app/src/agent_tab/thread_controls/effort.rs:247` | 31 | `fn on_effort_drag_move(pane, index) -> impl Fn(&MouseMoveEvent, ..)`; the 15-line `on_mouse_down` at line 232 is the same pattern. |
| `crates/app/src/agent_tab/execution/workflows.rs:119` | 28 | `async fn poll_workflow_refresh(this, cx)`. |
| `crates/app/src/agent_tab/execution/maintenance.rs:163` | 24 | `async fn finish_suspension(this, backend, epoch, result, cx)`. |
| `crates/app/src/agent_tab/session/mod.rs:322` | 21 | `async fn poll_git_branch(this, cx)`; a re-read-the-interval loop inside a constructor. |
| `crates/app/src/agent_usage.rs:99` | 17 | `fn on_settings_changed(this, cx)`; `cx.observe_global::<AppSettings>(Self::on_settings_changed)`. |
| `crates/app/src/agent_tab/view/mod.rs:271` | 16 | `fn on_transcript_mouse_up(this, event, window, cx)` via `cx.listener(Self::on_transcript_mouse_up)`. |
| `crates/app/src/agent_tab/view/mod.rs:241` | 14 | `fn on_escape(this, _, window, cx)`; four-branch precedence rule with a 5-line comment is policy, not markup. |

#### app — ui, shell, sidebar

| Location | Lines | Extract to |
|----------|------:|------------|
| `crates/app/src/ui/shell/agent_notifications.rs:346` | 85 | `Shell::on_agent_pane_event(&mut self, session, event, cx)`; `watch_pane` in `pump.rs:7` already delegates to a named fn, this sibling does not. |
| `crates/app/src/ui/git_status.rs:445` | 79 | `async fn load_snapshot(this, cwd, generation, branch_max_age, cx)`. |
| `crates/app/src/ui/shell/workspaces.rs:179` | 62 | `fn new_workspace_dialog(dialog, name_input, dirs, shell, window) -> Dialog`. |
| `crates/app/src/ui/shell/close.rs:524` | 59 | `fn close_last_workspace_dialog(dialog, shell, id, message, note) -> Dialog`. |
| `crates/app/src/ui/shell/workspace_dirs.rs:390` | 58 | `fn workspace_dirs_dialog(dialog, editor, shell, id, window, cx) -> Dialog`. |
| `crates/app/src/ui/shell/updates_layer.rs:219` | 54 | `fn expire_elapsed_cards(shell, cx) -> bool`; `ensure_timer` keeps only the ticker. |
| `crates/app/src/ui/settings/agent_profile_dialog.rs:102` | 50 | `fn agent_profile_dialog(dialog, target, window) -> Dialog`, next to the existing `agent_profile_dialog_content` at line 402. |
| `crates/app/src/ui/git_status.rs:354` | 25 | `async fn poll_git_status(this, cx)` placed after `GitStatusModel::new`. |
| `crates/app/src/ui/shell/agent_notifications.rs:308` | 20 | `Shell::process_due_agent_deadlines(&mut self, generation, cx)`. |
| `crates/app/src/ui/shell/mod.rs:282` | 17 | `Shell::on_window_activation(&mut self, window, cx)`. |
| `crates/app/src/ui/shell/mod.rs:245` | 15 | `fn on_window_bounds_changed(_, window, cx)`. |
| `crates/app/src/ui/shell/pump.rs:10` | 14 | `Shell::on_agent_interrupted(&mut self, pane, cx)`; the registration three lines above already uses the one-line delegation form. |

#### agent

| Location | Lines | Extract to |
|----------|------:|------------|
| `crates/agent/src/dsh/session/loads.rs:382` | 104 | `fn reconcile_models(client, session_id, selected, wanted_model, wanted_effort, declares_image_input) -> Value`; the nested `read_catalog` closure becomes a second helper. Every other loader in the file is under 25 lines. |
| `crates/agent/src/codex/usage_fetcher.rs:73` | 54 | `fn read_rate_limits(stdin, line_rx) -> Result<UsageSnapshot, String>`; an immediately-invoked closure used as a try block. |
| `crates/agent/src/codex/app_server/protocol.rs:399` | 44 | `fn parse_model(entry) -> Option<ModelInfo>` + `fn parse_service_tiers(entry)`. |
| `crates/agent/src/codex/app_server/protocol.rs:473` | 43 | `fn parse_thread_summary(thread, own_thread) -> Option<SessionSummary>`. |
| `crates/agent/src/dsh/subagents.rs:31` | 41 | `fn task_summary(entry, parent_session, parent_session_id, activity) -> Option<BackgroundTaskSummary>`. |
| `crates/agent/src/dsh/session/controls/mod.rs:70` | 40 | `fn run_control(client, call, deliver)`; `Controls::new` keeps `for call in receiver { run_control(..) }`. |
| `crates/agent/src/codex/app_server/mod.rs:878` | 224 (fn) | Split `process_response` into `on_transcript_read_response` (910–941), `on_descendant_page` (945–972), `on_response_error` (974–1011), `on_thread_switched` (1075–1097). |
| `crates/agent/src/team/session/planning.rs:203` | 36 | `fn next_stage(&mut self, discussion) -> Result<(StageKind, Vec<MemberId>), TeamError>` covering 168–240, with the `Moderated` arm as `moderated_next_stage`. |
| `crates/agent/src/dsh/events.rs:47` | 36 | `fn run_downlink(client, host, session_id, deliver, stopped, connected_tx)`; the inner step is already named `read_downlink`. |
| `crates/agent/src/deadline_timer.rs:51` | 25 | `fn run_deadlines(worker, callback)` after `DeadlineTimer::new`. |
| `crates/agent/src/dsh/session/mod.rs:484` | 21 | `on_question_request`; joins the existing `on_*` family at 631–799. |
| `crates/agent/src/codex/app_server/mod.rs:1104` | 16 | `fn on_host_exit(&mut self, params) -> Vec<Event>`; every other branch of `process_notification` already delegates. |
| `crates/agent/src/dsh/events.rs:344` | 15 | `on_follow`; only inlined arm of `Streams::process`. |
| `crates/agent/src/dsh/session/mod.rs:470` | 11 | `on_approval_request`. |
| `crates/agent/src/dsh/session/mod.rs:537` | 10 | `on_connection_reset`; the only inlined arm among 13 in that match. |

#### terminal, platform, remote_net, remote_session_hub

| Location | Lines | Extract to |
|----------|------:|------------|
| `crates/terminal/src/pty_pipe/mod.rs:921` | 120 | `fn on_resize(&mut self, window_size)`; the largest single arm in the workspace, hides the shape of `drain_recv_channel`. |
| `crates/terminal/src/pty_pipe/mod.rs:898` | 20 | `fn on_input(&mut self, input, state)`; carries a const and a four-clause ConPTY echo predicate. |
| `crates/remote_net/src/host.rs:242` | 50 | `fn on_control_message(shared, text)`; `run_control` should show only "ping or message". |
| `crates/platform/src/windows/pipes.rs:259` | 49 | `fn pump_buffer_to_pipe(..)`. |
| `crates/platform/src/windows/ipc.rs:83` | 47 | `fn serve_pipe(testing, on_message)`; `.spawn(move \|\| serve_pipe(testing, on_message))`. |
| `crates/platform/src/windows/pipes.rs:81` | 40 | `fn pump_pipe_to_buffer(pipe, producer, inner, errors)`; mirror of the row two above, comparable only once both are named. |
| `crates/remote_session_hub/src/windows.rs:411` | 40 | `fn on_checkpoint(result, id, stream, sender, receiver) -> Result<SessionSubscription, HubError>`; currently nested four levels deep inside `send(Msg::Checkpoint(CheckpointRequest(Box::new(..))))`. |
| `crates/remote_net/src/client.rs:432` | 35 | `async fn fetch_session_list(relay_url, host_id, host_public_key, device)`. |
| `crates/remote_net/src/client.rs:348` | 28 | `fn on_inbound_frame(chan, data, output, resume_after) -> Option<PumpExit>`; the sibling arm is 10 lines. |
| `crates/remote_net/src/host.rs:529` | 25 | `async fn on_session_event(sink, chan, bridges, session_id, event)`; the sibling arm at 557 already delegates to `handle_frame`. |
| `crates/platform/src/unix/mod.rs:718` | 23 | `unsafe fn prepare_pty_child(child, main, take_controlling_terminal) -> io::Result<()>`; the one part of a 280-line fn that runs in the child process. |
| `crates/platform/src/windows/notifier.rs:94` | 23 | `fn write_shortcut(exe_path, shortcut) -> windows::core::Result<()>`; leaves the COM init/uninit bracket as the only thing in `register_identity`. |
| `crates/remote_net/src/client.rs:96, 398, 436` | 9 × 3 | `fn on_worker_thread<T>(name, body: impl FnOnce() -> Result<T, NetError>)`; the identical "build a current-thread runtime or report `NetError::Internal`" block, with the same 3-line comment, is copy-pasted three times. |
| `crates/remote_net/src/host.rs:476` | 17 | `fn forward_events(subscription, session_id, events, cancel)`. |
| `crates/platform/src/unix/ipc.rs:160` | 17 | `fn serve_socket(listener, on_message)`; makes the two platform servers visibly the same shape. |

### P3 — entry first, helpers after (A4, pure code motion)

| File | Entry currently at | Helpers above it |
|------|--------------------|------------------|
| `crates/app/src/ui/shell/render.rs` | `impl Render for Shell` at 494 | `title_bar_leading_region` 62, `bind_actions` 74, `render_title_bar` 103, `render_app_menu_button` 309, `render_session_heading` 326, `render_workflows_button` 367, `render_background_tasks_button` 389, `app_menu` 438. |
| `crates/app/src/ui/workspace_sidebar/mod.rs` | `Sidebar::new` 389 / `render` 405 | eight helpers from line 80 to 327; worst offender in scope. |
| `crates/app/src/agent_usage.rs` | `impl Render for AgentUsageView` at 495 | ten helpers starting with `quota_gauge` at 40. |
| `crates/app/src/agent_tab/context_usage.rs` | `impl RenderOnce` at 324 | eight helpers from 52 to 284. |
| `crates/app/src/ui/tab_bar/mod.rs` | `TabStrip::new` 227 / `render` 271 | six sizing/color helpers from 97 to 207. |
| `crates/app/src/agent_tab/view/banners.rs` | `impl AgentPane` at 97 | `update_overlay_phase` 40, `latency_readout` 51, `composer_stats_label` 64 (while `multi_root_notice` at 488 is already correctly placed after). |
| `crates/app/src/update/install.rs` | `install_additions` 123, `plan` 179, `apply` 188 | `version_key` 42, `staged_names` 60, `versions` 81, `differing` 100. |
| `crates/app/src/workspace/mod.rs` | `impl WorkspaceManager` | `tabs_progress` at 259, single caller at 590. |
| `crates/app/src/logging.rs` | `init_logging` 56 | `log_dir` 14, `rotate_logs` 24. |
| `crates/agent/src/claude_code/usage_fetcher.rs` | `fetch_with_cancel` 287 | nine helpers from 147 to 232; the Codex twin puts `fetch()` first. |
| `crates/platform/src/unix/macos/login_shell.rs` | `missing_variables` 74 | `marker` 38; the other three helpers already follow the entry. |
| `crates/app/src/ui/shell/mod.rs` | `Shell::new` 230 | Applies once the two observers in P2 become named fns — place them after `new`. |

### P4 — naming families (B1 / B2 / B3, pure renames)

#### B1 — one prefix per event-reaction role

| Module | Today | Proposed |
|--------|-------|----------|
| `crates/app/src/agent_tab/session/events.rs:28, 441` | `apply_event`, `apply_replay` dispatch to seven `on_*` arms | `on_event`, `on_replay`. |
| `crates/app/src/agent_tab/**` (cross-module) | `apply_event`, `apply_execution`, `apply_branch_update`, `apply_fork_update`, `apply_rewind_update`, `apply_workflow_refresh_results` vs. `on_*` vs. `handle_*_control` vs. `present_*` | `on_*` for "something arrived, react"; keep `handle_*_control` for the keyboard-control family; keep `present_*` for pure rendering of a result. |
| `crates/app/src/ui/shell/agent_notifications.rs:277` | `apply_agent_event` (called from `main::on_ipc_agent_hook`) | `on_agent_event`. |
| `crates/app/src/ui/shell/pump.rs:35` | `pump_pane` — the only observer callback in `shell/` not named for its event | `on_pane_notified`. |
| `crates/app/src/ui/settings/terminal_bridge.rs:69` | `snapshot` next to `agent_snapshot` | `terminal_snapshot`. |
| `crates/terminal/src/pty_pipe/mod.rs:1303, 464` | `handle_request`, `process_pty_chunk` in the same loop as the new `on_input` / `on_resize` | `on_request`, `on_pty_chunk`. |
| `crates/platform/src/windows/conpty.rs:436` | `on_resize` — a setter called only from `Pty::set_winsize`, Unix twin is `Child::set_winsize` | `set_winsize`; `on_*` on a command is the reverse confusion. |
| `crates/agent/src/dsh/events.rs:409, 366` | `Streams::event`, `Streams::control` | `on_event`, `on_control`. |
| `crates/agent/src/claude_code/tasks/mod.rs:251, 314` | `apply_lifecycle`, `apply_subagent_stop` dispatched from an `observe_*` match | `observe_lifecycle`, `observe_subagent_stop`. |
| `crates/agent/src/codex/app_server/background_tasks/mod.rs:446` | `apply_descendant_notification` among `observe_*` siblings | `observe_descendant_notification`. |
| `crates/agent/src/codex/hook.rs:172` | `read_hooks` / `write_hooks`, while the Claude mirror uses `read_settings` / `write_settings` and Codex itself binds the result to `let mut settings` | `read_settings` / `write_settings`. |

Crate-wide in `crates/agent`, the role "turn one provider message into
events" is spelled `process_*` (Claude `stream_json`, Codex `app_server`),
`on_*` (DeepSeek `dsh/session`), and `handle_*` (Codex `host/router`). The
public dispatcher `process(&mut self, message)` can keep its name in all
three backends; the per-message handlers should converge on `on_*`. This is
the widest rename in the plan and should be its own commit per backend.

#### B2 — locals named after the settings field they hold

| Location | Today | Proposed |
|----------|-------|----------|
| `crates/app/src/ui/shell/close.rs:301, 446, 629` | `warn` (a three-state enum, not a bool) | `warn_before_terminating_shell` |
| `crates/app/src/ui/shell/close.rs:445` | `confirm` (line 305 spells the field out inline) | `confirm_before_closing_workspace` |
| `crates/app/src/ui/workspace_sidebar/mod.rs:428, 429` | `show_daily_usage`, `show_quotas` | `show_daily_token_usage`, `show_agent_usage` |
| `crates/app/src/sparkle.rs:45, 66` | `enabled` (`update/mod.rs:728` calls the same field `checking_enabled`) | `check_updates` |
| `crates/app/src/agent_tab/composer/branch/fork.rs:83` | `smooth` (`transcript/view/mod.rs:476` binds the same field as `smooth_wheel`) | `smooth_wheel` |
| `crates/app/src/agent_tab/transcript/view/mod.rs:475` | `collapse` (next line binds `smooth_wheel` in full) | `collapse_tool_calls` |
| `crates/app/src/agent_tab/team/view.rs:278` | `opacity` (shadows the element property of the same name) | `background_opacity` |
| `crates/app/src/agent_tab/thread_controls/mod.rs:112` | `style` (reads as element styling in a render fn) | `model_list_style` |
| `crates/agent/src/profile.rs:114, 122` | `url`, `key` (sibling `model` already matches its field; `key` is ambiguous next to env `(name, value)` pairs) | `api_base_url`, `api_key` |
| `crates/agent/src/claude_code/stream_json/mod.rs:504` | `mode` (named after the JSON key, not the field; siblings `model` 494 and `effort` 516 match their fields) | `approval` |

#### B3 — remove stray `get_*` where the module is bare-noun

| Location | Today | Proposed |
|----------|-------|----------|
| `crates/terminal/src/ghostty/mod.rs:302` | `get_string` — the sole `get_*` among ~50 bare-noun accessors, called by `title()` two lines up | `read_string` |
| `crates/platform/src/windows/shell_extension.rs:53, 85` | `get_exe_path`, `get_folder_path`, with `dll_path()` between them | `exe_path`, `folder_path` |
| `crates/platform/src/unix/macos/mod.rs:186` | `get_proc_path` beside `macos_process_name`, `macos_cwd` | `proc_path` |
| `crates/app/src/workspace/mod.rs:501, 527` | `tabs_of(WorkspaceId)` and `tab_manager_for(TabId)` return the same `Option<&TabManager<TabSurface>>` under two schemes | one family: `tabs_of` / `tabs_for_tab`, or `tab_manager_of` / `tab_manager_for` |

## 4. Execution notes

- **Commit granularity.** P0 is one commit. P1 deletions are one commit per
  crate (`refactor(config): drop unused load and save wrappers`). P2
  extractions are one commit per file or per flow (the `update/` flow, the
  `terminal_tab/view` flow, ...) so each diff is reviewable as pure code
  motion. P3 is one commit per file. P4 renames are one commit per module
  family; the crate-wide `process_* → on_*` rename in `crates/agent` is one
  commit per backend.
- **Ordering.** Do P1 deletions before P2 extractions in the same file so
  the extractions do not touch code that is about to disappear. Do P3 after
  P2 in the same file so the newly named handlers land in their final
  position once.
- **Hook constraints.** The pre-commit hook runs rustfmt and clippy on staged
  Rust and rejects comments that cite a plan or task as their rationale;
  extracted functions inherit the comments of the closure they came from,
  which already state technical reasons. Commit subjects must stay under 60
  characters and use `refactor(<scope>): ...`. The `Verification` section
  is omitted for these commits: they are behavior-preserving and a green
  build is not evidence of exercised behavior.
- **No behavior change is intended anywhere in this plan.** Where an
  extraction changes a closure's captured borrows (typically `&mut self`
  methods pulled out of `cx.spawn`), keep the same weak-entity upgrade path
  the closure used; do not widen lifetimes to make the borrow checker happy.
- **What is not in this plan.** Header layout (pub use → pub mod → mod →
  `#[cfg(test)]` → use) was the third habit of `998264d0` and is out of
  scope here; the module organization commit `c235ac39` already covers it.
