# Design issues in `crates/` — audit of 2026-09-13

Companion to `readability-refactor-plan.md`, which covers naming, layout and
closure extraction. This document covers everything that is not a
readability item: correctness-affecting shapes, ownership and invariant
splits, duplication that has already drifted, dead code, crate structure,
and violations of the "Technical taste" rules in `AGENTS.md`.

Scope: every crate under `crates/`, production code only. Every `path:line`
was verified against the working tree at `47e2b70f` (branch `dev`). The
claims that carry the most weight (zero callers, lock held across blocking
work, silent success paths) were re-checked by hand with `rg` after the
scan.

Decisions from earlier audits that this document respects and does not
re-open: the agent `Backend` stays an enum (a trait was rejected on
evidence, `docs/research/agent-harness-refactor.md` §4);
`crates/platform/src/unix` stays; `Result<_, String>` signatures stay; file
length alone is not a finding.

## 1. Ranked summary

| Tier | Theme | Count | Where |
|------|-------|------:|-------|
| 1 | Behavior-affecting: silent success, un-stoppable work, process abort, lock across blocking I/O | 12 | remote_net, terminal, platform, agent/dsh |
| 2 | Invariants enforced in two places: public fields written across module or crate boundaries, hand-synced parallel state | 9 | terminal, agent, app |
| 3 | Duplication that has already drifted into behavior differences | 7 (+8 mechanical) | app, agent, platform |
| 4 | Dead code and over-wide public surface | ~6 clusters, ~200 `pub` names | terminal, agent, config |
| 5 | Crate structure and settings shape | 6 | config, remote_session_hub, input, app settings |
| 6 | Taste-rule violations (hidden side effects, literal booleans, chained `==`, catch-all arms) | 14 | app, agent, terminal |

The single most valuable fixes, in order: the remote host that cannot be
shut down (1.1), the DeepSeek host registry lock held across a two-minute
start (1.4), the two parallel question protocols in `chat::Event` (2.5),
`Shell::focus_active` writing settings to disk (6.1), and the 68 raw
`AppSettings` field writes with no funnel (2.7).

## 2. Tier 1 — behavior-affecting

### 1.1 `HostHandle::shutdown()` does not stop the remote host
`crates/remote_net/src/host.rs:187`, flag read only at `:202` and `:219`.
The `shutdown` atomic is checked between relay connections. In the steady
state `run_control` is awaiting `ws.next()` and nothing observes the flag;
`spawn_connection` (`:264`) never checks it either. Turning the remote
session setting off (`crates/app/src/remote.rs:49`) leaves the relay socket
registered and still accepting clients, and the `remote-host` thread plus its
Tokio runtime live for the process lifetime.
Direction: add a cancellation branch to the `select!` in `run_control`,
check `shutdown` in `spawn_connection`, and join or `shutdown_timeout` the
runtime from `HostHandle`'s `Drop`.

### 1.2 `NetWriter::write` reports every byte as written
`crates/remote_net/src/net_pty.rs:176` returns `Ok(buf.len())` after
`send_input`, which is itself `let _ = self.commands.send(..)`
(`client.rs:61`). The comment calls this fire-and-forget by design, but the
effect is that a remote session whose pump has exited accepts every
keystroke, `PtyPipe::pty_write` sees `Ok(n)`, and `TerminalSession::write_input`
returns `true` to the view. This contradicts the rule that a command returns
what actually happened.
Direction: return `io::ErrorKind::BrokenPipe` when the command channel is
closed.

### 1.3 Engine replies bypass the write queue and are dropped under load
`crates/terminal/src/pty_pipe/mod.rs:790`:
`let _ = self.pty.writer().write_all(&responses);`. Every other PTY write
goes through `PtyState.write_list` with partial-write resumption
(`pty_write`, `:1058`). DA/DSR/OSC replies call `write_all` on the
non-blocking writer directly; when the 64 KiB ring in
`platform/src/windows/pipes.rs` is full the write returns short or zero and
the reply is lost, precisely when the shell is busiest.
Direction: push `responses` onto `state.write_list`.

### 1.4 DeepSeek host registry lock held across a blocking start
`crates/agent/src/dsh/host.rs:118` takes `SHARED.lock()` and, still holding
it, calls `Host::start(launch)?` at `:133`. `Host::start` spawns a process,
waits up to `PNPM_DLX_START_TIMEOUT` (120 s) on `address_rx`, and then does a
blocking HTTP login in `ApiClient::new`. Every other DeepSeek tab opening,
including one with a different launch key, blocks on the mutex for that
whole window. The Codex host next door already solves this with a
`starting` flag and a `Condvar` (`codex/app_server/host/mod.rs:103-126`).
Direction: mark the key as starting, drop the lock, start, re-acquire to
publish.

### 1.5 Missing `conpty.dll` aborts the process instead of failing the tab
`crates/platform/src/windows/conpty.rs:62` uses `expect` on
`load_conpty()`. It is reached from `TerminalSession::new`, which already
returns `Result<_, EngineError>` with a `PtySpawn` code.
Direction: `ConptyApi::new() -> io::Result<Self>` and propagate.

### 1.6 Three more locks held across I/O in `crates/agent`
- `claude_code/workflows/disk.rs:259` holds `self.cache.lock()` while
  reading a whole agent transcript JSONL at `:267`; the source is shared
  with the background refresh executor. `git.rs:83-92` in the same crate
  shows the correct compute-drop-read-reacquire shape.
- `update/mod.rs:570, 750, 767` call `write_cache` (create_dir_all + write +
  rename + retry) while the coordinator mutex is held, although the doc
  comment at `:420` promises provider work happens outside it.
- `claude_code/stream_json/mod.rs:295` wraps the delivery closure in
  `Arc<Mutex<_>>` so the reader thread and the deadline timer serialize on
  it. The mutex exists only because the closure bound is `Fn + Send`;
  `Backend::spawn` already requires `Sync` (`session/backend/mod.rs:81`).

### 1.7 Silent lossy detach freezes a slow remote client
`crates/remote_session_hub/src/windows.rs:216-233` drops a subscriber on
`TrySendError::Full`; `remote_net/src/host.rs:490-506` then sees
`Disconnected`, breaks, and keeps the socket open with no further output and
no `Exited` frame. The client's `pump` has no read timeout, so the tab
freezes instead of resuming from a fresh checkpoint, which is the documented
intent of the overflow policy.
Direction: send `ClientBound::Error` or `Frame::Exited` on overflow detach,
or have the client treat prolonged silence as `PumpExit::Disconnected`.

### 1.8 Worker threads with no cancel and no join
- `remote_net/src/client.rs:120-139`: `open_remote_session` gives up after
  `NET_TIMEOUT * 2` but the thread it started keeps its runtime and keeps
  connecting; the `JoinHandle` is discarded.
- `remote_net/src/net_pty.rs:75-105`: the drain thread's only exit is the
  client pump closing the channel; a closed tab keeps appending up to 8 MiB
  nobody reads.
- `remote_net/src/host.rs:465-513`: one polling bridge thread per attached
  session with 500 ms stop latency, because the push wake-up
  (`SessionSubscription::set_wake_thread`, `windows.rs:101`) was never
  connected and has zero callers.
- `terminal/src/pty_pipe/session.rs:105`: `drop(pipe.spawn())` discards the
  PTY thread handle; `TerminalSession::drop` sends `Msg::Shutdown` and does
  not wait, so `flush_pending_on_exit` may still be running against a
  `FrameStore` the app believes is finished.
- `agent/src/dsh/events.rs:119`: `Downlinks::drop` sets `stopped` and
  returns; the worker may be inside a 10 s connect or a reconnect sleep and
  still delivers a `nmt/connection-reset` frame for a session that is gone.
- `agent/src/dsh/host.rs:217-229`: after startup the stdout reader keeps
  parsing every line and calling `address_tx.send` (which always fails once
  the receiver is dropped) for the host's whole life.

### 1.9 `expect` on thread spawn inside `Result`-returning paths
`platform/src/unix/ipc.rs:161`, `platform/src/windows/ipc.rs:84`,
`terminal/src/pty_pipe/mod.rs:1302`, `remote_net/src/net_pty.rs:105`.
Also `platform/src/windows/pipes.rs:46-58`: the error-forwarding macro
panics the pump thread when the receiver is gone, i.e. during teardown, and
the panic then surfaces in a `Drop`-time `join` at `:216`.

### 1.10 DeepSeek background loads fail without a signal
`crates/agent/src/dsh/session/loads.rs`: ten detached `thread::spawn`
sites, unbounded, six of which (`:183, :202, :231, :259, :293, :325`) report
failure only via `tracing::warn!`. With
`Capabilities::async_command_discovery = true` the UI reads an empty palette
as "still loading", so a failed `commands/list` leaves the composer in a
permanent loading state. `load_search` (`:159`) and `load_fork_checkpoints`
(`:346`) already deliver a failure frame and their comments say why.

### 1.11 Two divergent reconnect policies for one relay link
`remote_net/src/client.rs:149-151` (5 attempts, linear 2 s, sleeps before
the first retry so an instant drop costs 2 s of blank terminal) versus
`remote_net/src/host.rs:32, 223-227` (unbounded, 1 s × attempt capped at
30 s, sleeps after failure). Same problem, two curves, no shared helper.
Related: the invariant comment at `protocol/types.rs:27-29` says
`seq >= base_seq` while the filter at `client.rs:385` is `seq > resume_after`;
and `host.rs:576-584` reuses one `seq` across every chunk of a large event,
so `seq` identifies a group rather than a frame.

### 1.12 `set_hook_executable` silently keeps the first path
`crates/agent/src/process.rs:70`: `let _ = self.hook_executable.set(path)`.
A second call is dropped and every hook registration written afterwards
uses the first path; nothing tells the caller which one won.

## 3. Tier 2 — invariants split across owners

### 2.1 `RenderBuffer`: 18 fields behind a capture protocol, 5 poked from outside
`crates/terminal/src/render_buffer.rs:30-34` exposes `revision`,
`theme_revision`, `viewport_top`, `title`, `current_directory` as `pub`;
`ghostty/mod.rs:1148-1153` writes three of them between `begin_capture` and
`finish_capture`, and `pty_pipe/mod.rs:764, 1006-1011` writes the other two.
A caller that forgets `revision` publishes a frame the app's change
detection skips.
Direction: `begin_capture` takes the three header values, `finish_capture`
takes the two revisions, all 23 fields become private. (This is the
"RenderBuffer behavior" item from the August audit, still open.)

### 2.2 ConPTY resize realign: one state machine as eight loose fields
`crates/terminal/src/pty_pipe/mod.rs:134-152` (`conpty_resize_echo_realign`,
`conpty_resize_echo_pending`, `conpty_resize_repaint_reads_remaining`,
`conpty_resize_at`, `conpty_resize_prompt_row`, `conpty_resize_cols`,
`conpty_resize_rows`, `su_realign_armed`), set in `on_resize`
(`:1036-1050`), driven inline in `on_pty_chunk` (`:477-560`) and `on_input`
(`:928-936`), 43 references in one file, every use gated on
`nmt_platform::USES_CONPTY`. Tests set the fields directly
(`ghostty_mirror_tests.rs:550, 609, 682, 768`). The pure predicates already
moved to `platform/src/conpty_realign.rs`; only the latch is still inline.
Direction: `enum ConptyResize { Idle, Armed {..}, Repainting {..} }` owned by
a small struct with `on_resize` / `on_read` / `on_input`. (August audit's
"ConptyResizeGuard extraction", still open and now larger.)

### 2.3 `SessionController`: 17 of 18 fields `pub`, written from the app crate
`crates/agent/src/session/controller/mod.rs:57`. The app writes them through
`Rc<RefCell<_>>`: `agent_tab/composer/branch/fork.rs:217-218` and
`agent_tab/execution/branch.rs:81-82` both clear
`controls.seed_thread_defaults` / `seed_approval_reviewer` (the branch-reset
rule written twice); `agent_tab/execution/startup.rs:36-39` resets
`controls.settings`, `command_catalog`, `skill_catalog` (the "new
conversation" reset lives in the UI crate); `composer/slash.rs:147, 174` set
`controls.settings.model` / `.approval`.
Direction: `SessionController::{begin_branched_conversation,
reset_for_restart, set_model, set_approval}`; drop `pub` from `controls`,
`command_catalog`, `skill_catalog`.

### 2.4 Three-state encoded as a boolean pair
`crates/app/src/agent_tab/execution/startup.rs:96-97` builds a three-way
`SettingsSeed` and immediately shreds it into `seed_thread_defaults` and
`seed_approval_reviewer` with two `matches!`. A fourth variant produces
`(false, false)` silently. Same shape in
`claude_code/stream_json/mod.rs:114` (`turn_active` / `turn_reported` encode
idle / started-unconfirmed / running; the comment at `:126` says so) and
`session/update_readiness.rs:13` (`empty` is the negation of the other three
flags).
Direction: store the enum.

### 2.5 Two question protocols in the shared event vocabulary
`crates/agent/src/chat/mod.rs:499-518`: `QuestionsRequested` /
`QuestionsResolved` (id-less, Claude) and `InputRequested` / `InputResolved`
(identified, Codex). DeepSeek straddles both: raises `InputRequested`
(`dsh/session/mod.rs:618`) and resolves with `QuestionsResolved`
(`dsh/mapping.rs:184`). The split propagates upward as
`SessionInput::receive` + `receive_legacy` (`session/input/mod.rs:102, 124`)
and `Backend::respond_input` + `respond_questions`
(`session/backend/mod.rs:753, 780`).
Direction: make `QuestionRequest.id` optional (or synthesize one for Claude)
and delete the legacy pair, one `SessionInput` entry point, and one `Backend`
method with its catch-all arm.

### 2.6 Index-parallel collections synced by hand
- `crates/app/src/ui/shell/workspace_dirs.rs:85`: `available: Vec<bool>`
  documented as "parallel to `roots.ordered()`", resynced from an async task
  and guarded only by a length check at `:134`; a same-length edit mislabels
  rows.
- `crates/app/src/agent_tab/session/prompts.rs:10`: `presentations` is
  index-parallel to `SessionInput::batches()`, resynced at `:45-49` and
  `:65-68`, indexed unchecked at `:34`; one level down,
  `QuestionPresentation::editors` (`questions/mod.rs:42`) is parallel to
  `draft.questions()`. `QuestionDraft::key()` already exists and is used at
  `prompts.rs:24`.
- `crates/app/src/agent_tab/transcript/view/mod.rs:120-135`: `zoomed_image`,
  `zoom_open`, `zoom_fade`, `zoom_origin` are four hand-synced fields for
  one closed / open / fading state; the comment at `:130` admits the image
  alone cannot say.
- `crates/input/src/lib.rs:405-418`: `SequenceBuilder` caches `kitty_seq`,
  `kitty_encode_all`, `kitty_event_type` beside the `flags` they derive from,
  and re-derives them from `flags` elsewhere in the same file.

### 2.7 `AppSettings` has a save funnel but no write funnel
`crates/app/src/ui/settings/state.rs:64`. Load and save are collapsed
(`AppSettings::load` at `:284` with one caller; `save_settings` at
`ui/settings/mod.rs:146` with five). But 63 `cx.global_mut::<AppSettings>()`
plus 5 `update_global` sites write fields inline: `appearance_page.rs` 19,
`system_page.rs` 7, `remote_session_page.rs` 7, `profiles_page.rs` 7,
`agent_page.rs` 6, and two escapees outside the settings module,
`ui/shell/close.rs:627` (`editing.discard_on_exit = true` from a dialog's
`on_ok`) and `ui/font_picker.rs:153` (four font fields). `TerminalSettings`
and `AgentSettings` prove the better shape in the same crate
(`ui/settings/terminal_bridge.rs:6`).
Related: `SettingsEditing` (`state.rs:84`) lives inside the persisted
settings global although it holds transient UI state (theme filter
keystrokes, `save_error`, pairing input), which is why every theme-filter
keystroke fires the `observe_global::<AppSettings>` handler that
`main.rs:667-670` works around.
Direction: route page edits through named mutation methods on `AppSettings`;
move `SettingsEditing` onto `SettingsSurface`.

### 2.8 Dialog scratch state promoted to app-lifetime globals
`crates/app/src/ui/settings/agent_profile_dialog.rs:75` `AgentProfileDraft`
is a `Global`, set at `:97`, mutated from 20 closures, committed at `:161`,
and never cleared on cancel, so a stale draft including `api_key` outlives
the dialog for the process lifetime and only one dialog can exist.
`FontPickerGlobal` (`ui/font_picker.rs:78`) and `OpacitySliderState`
(`ui/settings/fields.rs:58`) have the same shape.
Direction: `Entity<AgentProfileDraft>` captured by the dialog closure.

### 2.9 Half-built value patched by a second owner
`crates/app/src/workspace/mod.rs:221` `WorkspaceSummary` (14 fields) is
built by the manager with `terminal_activity`, `progress`, `agent_status`,
`unread_count`, `latest_unread_text` at defaults, then patched by the shell;
the doc comments at `:234` and `:249` say so. A consumer cannot tell a filled
summary from an unfilled one.
Direction: `WorkspaceSummary` (manager-owned) + `WorkspaceChrome { summary,
activity, progress, unread }` produced by the shell.

## 4. Tier 3 — duplication that has drifted

Two of these have already produced observable behavior differences.

| # | Copies | Drift |
|---|--------|-------|
| 3.1 | Tab-open epilogue in `ui/shell/tabs_open.rs` `:48-59` (team), `:140-158` (profile), `:180-202` (remote), `:255-283` (agent): alloc id → `new_tab` → `focus_active` → `sync_session_memory` → `notify` | The remote path omits `sync_session_memory`, so a remote tab is not written into session memory at creation. Confirm whether that is intended before unifying. |
| 3.2 | `KeyOutcome` decode in `terminal_tab/view/mod.rs` `:563-590` (`on_key_down`) vs `:593-622` (`feed_terminal_key`) | `on_key_down` routes `CopyPending` through `on_copy_finished` and shows the "copied" notification; `feed_terminal_key` re-implements the spawn at `:606-617` and shows nothing. A copy triggered via `SendTab` gives no feedback. |
| 3.3 | Agent-monitor mutation epilogue, 6 copies: `ui/shell/agent_notifications.rs` `:40-48, :101-109, :282-292, :323-332, :434-441`, `ui/shell/pump.rs:21-29` | Each copy applies a different subset of `{remove_native_notifications, notify, reschedule_agent_timer, process_native_notifications}`. |
| 3.4 | `codex/usage_fetcher.rs:26-108` re-implements `JsonLineProcess` (`subprocess/mod.rs:73-160`): piped spawn, `KillOnCloseJob`, reader threads, bounded wait | Hardcodes the executable name `codex` instead of `AgentCli::from_launch`, so a profile using `npx codex` or a pinned path falls back to PATH; has no cancellation flag while the Claude fetcher takes `&AtomicBool`. |
| 3.5 | Hook install/uninstall/status: `claude_code/hook.rs:112-158` vs `codex/hook.rs:101-182`, seven near-identical wrappers each over the shared `hook_store` | Codex has an extra `Stale` branch and `"timeout": 10`; Claude's command is a `const`, Codex's is computed. One `HookRegistration { events, entry, command }` per harness collapses both to three shared fns. |
| 3.6 | Background-task registry wrapper: `claude_code/tasks/mod.rs:74-235` vs `codex/app_server/background_tasks/mod.rs:62-140` (`root`, `set_root -> bool`, `snapshot`, `set_discovery` over `Option<BackgroundTaskRegistry>`) | The `Option` re-checking produced `claude_code/tasks/mod.rs:208, 214`: an `expect("registry exists")` three lines after the same field was treated as possibly `None`. |
| 3.7 | `override_value` in `platform/src/unix/environment.rs:91` vs `windows/environment.rs:37` | Identical bodies; the only content is `==` vs `eq_ignore_ascii_case`. |

Mechanical duplicates with no drift yet (one shared helper each):
`ui/shell/close.rs` close-confirmation description builder (6 copies,
`:82, :277, :315, :406, :459, :590`); `ui/workflows.rs:52-88` vs
`ui/background_tasks/mod.rs:155-196` `set_target` / `set_visible` (and the
hand-rolled `Option` equality is just `==`); `ui/shell/panels.rs:87-117`
`sync_workflow_target` / `sync_task_target`; `main.rs:772-825` IPC fan-out
over `ShellRegistry`; `main.rs:582-591` vs `:677-695` two encodings of
"last-active window first"; `catalog.rs:181-191` vs
`session/backend/mod.rs:223-232` the same `adapter_commands` table keyed on
`AgentKind` and on `Backend`; `VersionStatus` eight-field unsupported literal
written three times (`codex/update.rs:106`, `claude_code/update.rs:121, 136`);
`remote_net/src/client.rs:96, 398, 436` the same 9-line current-thread
runtime prologue with the same comment; `platform/src/windows/pipes.rs`
read and write pumps mirrored (`:21-219` vs `:221-410`); the
`create_pty_with_management` wrapper trio in `platform/src/unix/mod.rs:440`
and `windows/mod.rs:94`; IPC message framing in `unix/ipc.rs:164` and
`windows/ipc.rs:121`; `KillOnCloseJob::attach_or_kill` and
`ProcessTree::other_process_count` in both `process.rs` files.

## 5. Tier 4 — dead code and public surface

### Unreachable clusters
| Location | What | Size | Evidence |
|----------|------|-----:|----------|
| `terminal/src/graphics.rs:330, 346, 231, 126` | Graphics resize subsystem (`ResizeCommand`, `ResizeParameter`, `resized`, `compute_display_dimensions`) | ~175 | `GraphicData.resize` is `None` at every construction site (`:165, :363, :392`, `ghostty/kitty.rs:493`). Also `:354` has a bare `#[test]` with no `#[cfg(test)]` gate. |
| `terminal/src/terminal/pos.rs:47-127` | `StandardCharset`, `Charsets`, `CharsetIndex`, `Boundary`, `Cursor<T>` | ~110 | Charset translation is inside the Ghostty engine now; `Cursor<T>` is named nowhere outside the file. |
| `terminal/src/event.rs:155-322, 485` | 34 of 55 `TerminalEvent` variants (`PrepareRender*`, `RenderRoute`, `Paste`, `Copy`, `CreateWindow`, `SelectNativeTab*`, `Quit`, ...) each with a hand-written `Debug` arm at `:324-449`; `SearchState` | ~200 | Neither constructed nor matched outside the file. |
| `agent/src/team/storage/ownership.rs:25-40, 148-200, 244-250` | Two-phase ownership transfer (`TransferTicket`, `prepare_transfer`, `commit_transfer`, `cancel_transfer`) | ~90 | Zero production callers; `cancel_transfer` has one occurrence in the workspace. |
| `config/src/colors/mod.rs:110-300` | 11 chrome color fields (`tabs_active`, `vi_cursor`, `tab_border`, `selection_foreground`, `split_active`, `search_match_*` ×4, `hint_*` ×2) | ~75 with defaults | Only occurrences are the declaration and the `Default` impl. A theme file editing `tab-border` changes nothing. |
| `config/src/lib.rs:101-108` | `CursorConfig::blinking`, `blinking_interval` | small | `AppSettings::load` copies only `shape`; `RenderBuffer::cursor_blinking()` (`render_buffer.rs:154`) also has zero callers, so blink is unread on both sides. |
| `terminal/src/session/mod.rs:321` | `mark_read_only` and the `read_only` flag loaded on the `write_input` hot path | small | Nine callers, all tests. |
| `terminal/src/input.rs:66-71` | Everything in `nmt_input` gated on `KeyEncodeFlags` | most of the crate | The single production call passes `KeyEncodeFlags::empty()`; `PtyPipe` publishes real kitty flags into `vt_modes` but nothing converts them. Either derive flags from the session mode here or delete the unreachable half. |
| `remote_net` | `HostBound::Detach`, `HostBound::Kill` (matched, never constructed); `list_remote_sessions` and the whole session-listing path (zero callers); `set_wake_thread` | | Decide whether these are features; today `RemoteSessionHub::kill` is unreachable. |
| `config/src/render_types.rs:15-29` | `TRANSPARENT`, `BLACK`, `WHITE` | 3 consts | The module's `Color` type is used; only the constants are dead. |
| `platform` | `create_pty_with_fork` (carries a TODO about unchecked spawn failure), `command_per_pid`, `kill_pid`, both `spawn_daemon`s, `windows::create_pty`, `cmdline` (identity fn) | | Zero callers outside the crate. Visibility-only for `unix/`; deletable elsewhere. |

### Over-wide `pub`
`crates/agent` exports 822 distinct `pub` names of which 199 have no mention
outside the crate. Representative: `launcher::run_bounded` and its
`ProcessLimits` / `ProcessOutput` / `ProcessError` cluster; `dsh/host.rs`
`HostError` and the four `NPX_*` / `PNPM_DLX_*` consts; `dsh/version.rs`;
`claude_code/sessions/{titles,replay,fork,task_history}` loaders; every
session method reached only through `Backend`; `codex::ProviderConfig`
re-exported from `lib.rs:1` as `CodexProviderConfig` and named nowhere; the
`monitor.rs` cluster re-exported from `lib.rs:7-11`. Full list in the scan
output; direction is `pub` → `pub(crate)` per module, and
`#[cfg(any(test, feature = "test-support"))]` for the 20-odd test-only items
(`input::build_key_sequence`, `encode_terminal_key`, `KeyUpAction`;
`kitty_virtual::{DIACRITICS, encode_placeholder, decode_placeholder}`;
`local_state::{active_workspace_index, active_tab_index}`;
`UpdateCoordinator::with_clock`; ...).

Misplaced module: `crates/agent/src/annotations.rs` (61 lines) has zero
callers inside `crates/agent`; its only consumer is
`crates/app/src/agent_tab/composer/response_annotations.rs`.

## 6. Tier 5 — crate structure and settings shape

### 5.1 `nmt_config` ships AES-GCM and `toml_edit` to the VT engine
`crates/config/Cargo.toml:12-21`. `nmt_terminal` takes only colors,
`CursorShape`, `active_colors` and `NewlineShortcut` from it, but inherits
`aes-gcm`, `regex` (used once to parse `#rrggbb`), `toml_edit`, `tempfile`
because `config/src/profile/` (agent profiles and credential encryption)
lives in the same crate.
Direction: move `profile/` into its own crate depended on by `app` only.
This also enables 5.4.

### 5.2 `nmt_remote_session_hub` earns none of the reasons for a crate
549 lines, entirely `#[cfg(windows)]`, one dependent (`nmt_remote_net`),
no separate build target, no dependency it does not share with its parent,
and already removed from `default-members` in the root `Cargo.toml`.
Direction: fold into `nmt_remote_net::hub`.

### 5.3 `nmt_input` is a module in crate clothing
960 lines, deps `bitflags` + `smol_str`, wrapped by `nmt_terminal::input`
already; the app's three imports want only `ModifiersState`. Weakest of the
three; fine to leave if the crate-level key-encoding test suite is wanted.

### 5.4 Three representations of the agent launch profile
`nmt_config::profile::AgentProfile`, `nmt_agent::profile::LaunchProfile`
(`crates/agent/src/profile.rs:13-24`, a borrowed field-for-field twin), and
`AppSettings.agent_profiles`. `ProfileLauncher` duplicates
`AgentProfileLauncher`, `AgentKind` duplicates `AgentProfileKind`. Adding one
profile setting touches three structs and two enum pairs, with the
conversion hand-written in `app`.
Related, still open from the August audit (R4): the five harness identity
enums remain (`AgentKind`, `AgentProfileKind`, `ProviderKind`,
`BackgroundTaskProvider`, `BackgroundTaskRefs`), each grew a DeepSeek arm
except `ProviderKind` (`update/mod.rs:52`, two arms, reachable from a
three-element `AgentKind::ALL`).

### 5.5 `Config` and `AppSettings` are two copies with validation on one side
`crates/app/src/ui/settings/state.rs:64-83, 281-370`. `AppSettings::load`
clones each `Config` section into a gpui `Global`, flattens `cursor` to
`cursor_shape`, and runs nine `clamp_*` / `*_or_default` normalizers
(`:203-278`) after deserialization in a different crate from the `serde`
defaults. `nmt_remote_net` and `nmt_remote_session_hub` read `nmt_config`
directly and get the unclamped values.
Direction: normalize in `Config::load` (a `normalize(&mut self)` on
`AppearanceConfig`), and let `AppSettings` hold `Config` rather than mirror
it.

### 5.6 Wide structs that cluster cleanly
`Colors` (44 fields → `TerminalPalette { normal, bright, dim }` +
`ChromeColors`, the latter being the dead group in Tier 4);
`AppearanceConfig` (28 → `fonts` / `window` / `tabs` sub-tables);
`PtyPipe` (34 → extract the ConPTY latch from 2.2 and a `BlockTracking`
for `sniffer`, `launch_cwd`, `engine_blocks`, `mark_seq`, `prev_alt_screen*`);
`Shell` (22 → a `PendingShellWork` for the five "needs a window, wait for
render" fields); `TranscriptView` (25 → `TranscriptMetrics` for the eight
geometry fields that forced `pub(super)`); `dsh::Session` (20 → a
`TurnState` for the in-flight cluster).

## 7. Tier 6 — taste-rule violations

### 6.1 `Shell::focus_active` is a focus call that writes settings to disk
`crates/app/src/ui/shell/mod.rs:472-479`: reached from 25+ sites, it calls
`ui::settings::save_settings` when the settings tab stops being active, and
also clears the bell and last-command outcome (`:496`), rewrites the tab
title (`:490`), and acknowledges an agent notification (`:517, :526`).
Pure reorders inherit all of it: `ui/shell/workspaces.rs:30, :53`
(`reorder_workspaces`, `reorder_tab`) can trigger a config-file write from a
drag. `settings_workspace.rs:77 note_active(..) -> bool` is the edge detector
whose only use is to fire that write.
Direction: `focus_active` focuses; a separate `on_active_tab_changed` does
the rest and is called by the few real activation paths; the settings save
moves to the settings-surface teardown that owns it.

### 6.2 Boolean parameters with literal-only call sites
- `agent_tab/context_usage.rs:412 token_usage_rows(.., include_total: bool)`:
  both callers pass `true`.
- `ui/shell/close.rs:10 should_confirm_close(is_agent && confirm_agent_close, ..)`:
  a four-argument wrapper around a bare `&&`, one production caller, exists
  for `ui/shell/tests.rs:61-64`. This is the exact shape `AGENTS.md` forbids.
- `agent_tab/session/events.rs:353 on_turn_completed(.., _interrupted: bool, ..)`:
  threaded through from `:86-87` and never read.
- `terminal/src/ghostty/block.rs:125 format_range_clamped(.., unwrap: bool, trim: bool)`
  and `ghostty/mod.rs:955`: all three production calls pass `true, true`.
- `platform/src/{windows/mod.rs:111, unix/mod.rs:520} create_pty_with_management(.., manage_process_tree: bool)`:
  literal at both wrappers, then threaded three levels into `conpty::new`
  and re-checked twice (`conpty.rs:327, 343`).

### 6.3 Chained `==` / `matches!` instead of one `match`
- `agent_tab/execution/startup.rs:96-97` (see 2.4).
- `agent_tab/team/dispatch.rs:169, 176`: `status() == Idle` then
  `status() == Exited` in two disjoint chains seven lines apart.
- `ui/settings/agent_profile_dialog.rs:665, 676, 704, 715`: `profile.kind`
  compared four times beside an existing `match profile.kind` at `:688`;
  `AgentProfileLauncher` three more times at `:575-591`.
- `agent/src/session/backend/team.rs:32-56`: `kind == DeepSeek && ..` then
  `kind == Codex`.
- `terminal/src/session/selection.rs:141-149`: `== Lines` then `!= Semantic`
  with an implicit third case.

### 6.4 Catch-all arms that will swallow a fourth harness
`agent/src/session/backend/mod.rs:749` (`restore_question_requests`), `:775`
(`respond_input`), `session/backend/team.rs:19, 62, 74`. Every other arm in
the file is exhaustive; these five re-open the silent-site class the
capabilities table was built to close. Also a new `Backend` variant leak at
`session/input/approval.rs:60-66` asking "does this harness acknowledge
approvals asynchronously" by matching on identity instead of a capability.

### 6.5 Whole-window repaint where an entity notify would do
20 `cx.refresh_windows()` sites, worst in `agent_updates/transaction.rs`
(7), `ui/settings/theme.rs:127, 140`, `ui/settings/remote_session_page.rs:217,
350`, `ui/shell/updates_layer.rs:107, 267`. The frame-pump rule itself is
clean: the single `on_next_frame` site (`agent_tab/view/mod.rs:552`) pairs
with `cx.notify()`, and all four `request_animation_frame` calls are inside
`render`.

### 6.6 Single-thread data behind synchronization primitives
`pty_pipe/mod.rs:123 content_version: Arc<AtomicU64>` never cloned;
`session/mod.rs:172 staged_blocks: Mutex<Vec<_>>` touched only on the PTY
thread; `session/mod.rs:138 pages: Mutex<PageCache>` on a value never
`Arc`-shared; `session/mod.rs:164 open_prompt: Mutex<bool>` beside three
`AtomicBool` siblings, locked once per frame.

### 6.7 Shared option a backend silently drops
`platform/src/lib.rs:48 PtyOptions.bootstrap` is honored by the Unix path
and discarded by the ConPTY destructure at `windows/conpty.rs:186-193` with
no signal. Move it to the Unix-specific input or return an error for a
`Some` that cannot be honored.

## 8. Status of the August 2026 open items

| Item | Status |
|------|--------|
| `register_application_identity` has no caller | Resolved. Renamed to `notifier::register_identity` (`platform/src/windows/notifier.rs:72`), called from `shell_integration.rs:97` via the System settings toggle; `identity_registered()` gates every toast. Native toasts are opt-in on a fresh install rather than broken. |
| JsonLineProcess + KillOnCloseJob consolidation | Mostly done (`subprocess::JsonLineProcess` serves Claude and the Codex host). Residual: `codex/usage_fetcher.rs` (3.4). |
| AppSettings load/save collapse + mutation funnel | Load/save collapsed; write funnel still open (2.7). |
| Shell pure-state extraction | Partially done (`WorkspaceManager`, `TabManager`, pane tree, `TabSurface`, `UpdateNotificationLayer` are window-free). Remaining tangles: `close.rs` decision tree and `workspace_dirs.rs` availability policy behind dialogs. |
| agent_utils domain split | Resolved; `crates/agent/src/lib.rs` is a clean domain layout with no grab-bag module. |
| RenderBuffer behavior | Open (2.1). |
| ConptyResizeGuard extraction | Open, larger than before (2.2). |
| R4 collapse identity enums | Open (5.4). |
| R5 hand-written two-element UI lists | Resolved; both lists iterate `AgentKind::ALL`. |

## 9. Execution notes

- Tier 1 items are independent of each other and of the readability plan;
  each is one focused commit with a `Verification` section describing the
  scenario exercised (e.g. toggling the remote host off and confirming the
  thread exits; filling the PTY write ring and confirming a DA reply
  survives).
- Tier 2 items 2.1, 2.2 and 2.3 change public shapes and should each land
  before the readability extractions touch the same files
  (`pty_pipe/mod.rs`, `render_buffer.rs`, `session/controller`).
- Tier 4 deletions are the cheapest wins and reduce the surface the other
  tiers have to reason about; do them first within each crate. Do not delete
  anything under `platform/src/unix` — narrow visibility only.
- Tier 5 crate moves (5.1, 5.2) are one commit each and should be done
  before 5.4 / 5.5, which depend on the profile crate existing.
- `docs/adr/` does not exist. The cross-cutting decisions this audit keeps
  running into (identity-enum layout, settings mutation model, ConPTY
  realign protocol) are documented only in
  `docs/research/agent-harness-refactor.md` and code comments; once 2.2, 2.7
  and 5.4 land, a short decision record for each would stop the next audit
  from re-deriving them.
