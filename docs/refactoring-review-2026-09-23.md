# Crates review against docs/refactoring-guide.md

Date: 2026-09-23. Tree: `dev` at 2089e938. Scope: every first-party crate under `crates/`.

## Method

- Twelve read-only reviewers, one per area. Each finding was confirmed with grep or a direct read. Nobody ran cargo or launched the app.
- The coordinator re-checked about 35 of the highest-impact claims against the source, the Claude Code CLI binary, and the DeepSeek harness checkout. Items marked **[re-verified]** were confirmed that way. One reviewer claim was wrong and is corrected here: `toml` is a production dependency of `crates/app` (used by `ui/settings/theme.rs:19`).
- Every reviewer hit the account rate limit and resumed with a tool-call budget, so coverage is partial. Test-file bodies were mostly skimmed. Each area lists what it did not cover.
- Workspace rule checks: zero `use super::`/`use self::` imports in first-party crates. Nine `impl` blocks sit outside their type's file (section 3, cross-cutting).

Tags: P1-P8 are the guide's principles; RULE marks AGENTS.md rules; BUG marks behavior defects.

## 1. Bugs, ranked

### High

1. **DeepSeek background-tasks panel never shows** [re-verified]. `ChildAgents::parent` returns `None` for DeepSeek (`crates/agent/src/session/children.rs:124-130`), so `scoped_background_tasks` (`:40-48`) drops every snapshot and the panel target at `crates/app/src/ui/shell/panels.rs:82` is `None`. 322ef095 added the dsh producers and left this arm. Fix: map DeepSeek to `BackgroundTaskKey::deepseek(identity.id)`; then recheck Stop on continuable children, the Unavailable answer for job rows, and scoping across `on_switched`.
2. **Team discussions can pause with no way out** [re-verified].
   - `continue_discussion` clears only User/UserInput/ModeChange/Reopened/Closed (`crates/agent/src/team/session/mod.rs:649-662`); `finish_with_report` clears only Budget/AttemptFailed/User (`:799-804`). Nothing removes InvalidModeration, ContextSelection, DispatchUnavailable, Storage or SummaryFailed (`crates/agent/src/team/discussion.rs:42-60`). Triggers: a moderator answering in prose, a member context over 96 KB, excluding a participant.
   - The app keys `PauseReason::Interaction` by an `InteractionId` held only in memory on `MemberHost` (`crates/app/src/agent_tab/team/operations.rs:95-110`, `member_host.rs:21`). Quitting or switching rooms while a member waits leaves a persisted pause that nothing can resolve.
   - A live attempt that turns Uncertain (member exit) blocks the room until reopen, because `restored_uncertainty` is filled only in `open` (`team/session/mod.rs:108-113`, `:234-238`, `:1031-1034`).
   - A stuck discussion makes `create_discussion` return AlreadyActive for the life of the room (`crates/agent/src/team/room.rs:130-136`).
   - Fix: an exit for every reason; key interaction pauses by member; move in-flight attempts of an ended backend generation into the recovery set; one regression test per reason.
3. **Claude paths ignore `CLAUDE_CONFIG_DIR`** [re-verified]. `projects_root`/`project_dir` (`crates/agent/src/claude_code/sessions/mod.rs:107-125`), `settings_path` (`claude_code/hook.rs:101-107`) and `configured_permission_mode` (`claude_code/stream_json/mod.rs:1507-1514`) hard-code `~/.claude`. Only `usage_fetcher.rs:169-181` honors the variable, which the CLI uses as its configuration home. With the variable set globally or through a profile's `launch.env`, history, resume, rewind, fork, child transcripts, workflow restore, the default permission mode and hook install read the wrong directory. Fix: one `claude_config_home(launch_env)` helper.
4. **Claude project-directory naming diverges from the CLI** [re-verified]. `munge_cwd` (`sessions/mod.rs:100-104`) lacks the CLI's `RC(e)` rule: a munged path over 200 UTF-16 units becomes `munged[..200] + "-" + abs(TQ(e)).toString(36)`, where `TQ` is the 32-bit `(h<<5)-h+charCode` hash. JS replaces per UTF-16 unit, so astral characters become `--`. History features look in a missing directory for long paths.
5. **Claude child transcripts are parsed on the UI thread every second** [re-verified]. `poll_watched_children` -> `load_child` (`crates/app/src/agent_tab/execution/mod.rs:578-642`) -> `load_child_transcript` (`crates/agent/src/claude_code/sessions/task_history.rs:75-125`) lists `subagents/`, parses every `*.meta.json`, and re-parses the whole child JSONL while holding `controller.borrow_mut()`. Fix: the controller plans the read, `read_in_background` runs it, events apply when the epoch is current; skip files whose length is unchanged.
6. **Codex: resuming a thread closed earlier in another tab loses its events** [re-verified]. Traffic for an unowned thread is held (`crates/agent/src/codex/app_server/host/router.rs:576-579`), including the `thread/closed` Codex broadcasts after its unload delay (60 s default). `claim_thread` delivers the held close and then calls `remove_thread` (`:190-221`), so the resumed tab stops owning the thread. Requires another Codex tab keeping the shared host alive. Fix: forget held traffic when `thread/closed`/`thread/deleted` arrives for an unowned thread, and on `detach` for the owner's threads. Update `host/tests.rs:731-761`, which pins the current behavior.
7. **Codex: concurrent approvals overwrite each other** [re-verified]. `pending_approval = Some(rpc_id)` (`crates/agent/src/codex/app_server/mod.rs:890`) replaces a pending request, which never gets an answer; a child's `serverRequest/resolved` is dropped by the parent-thread filter (`conversation.rs:265-288`). The controller models one approval (`session/controller/mod.rs:1009-1020`), so either queue by rpc id or decline while one is pending.
8. **OSC 9 / OSC 777 desktop notifications never fire** [re-verified]. `install_callbacks` (`crates/terminal/src/ghostty/callbacks.rs:48-81`) registers WRITE_PTY, BELL, CLIPBOARD_WRITE and PROGRESS_REPORT only; the engine's DESKTOP_NOTIFICATION option is unused and `TerminalEvent::DesktopNotification` is built only in `session/tests.rs:305`. The app handler at `crates/app/src/ui/shell/mod.rs:2726-2737` never runs. `TitleWithSubtitle` and `Exit` are never constructed; `ResetTitle` only in a test.
9. **Kitty images replaced at the same size keep their old pixels** [re-verified]. `shipped_images` keys on `(width, height, data_len)` (`crates/terminal/src/ghostty/kitty.rs:34-38`, `:339-353`). The engine's `KITTY_IMAGE_DATA_GENERATION` changes on re-transmit and on every animation frame, so animated images also freeze. Fix: key on generation and share the read with `ghostty/mod.rs:1037-1074`.
10. **ConPTY leaks two handles per tab and the reader never sees EOF** [re-verified]. `conin_pty_handle.into_raw_handle()` and `conout_pty_handle.into_raw_handle()` go to CreatePseudoConsole and are never closed (`crates/platform/src/windows/conpty.rs:203-209`), so this process keeps the output pipe's write end open and termio ends only through `poll_exit`. `ConptyApi::new()` also reloads the DLL for every PTY. Fix: pass `as_raw_handle()`, drop the `OwnedHandle`s after S_OK, cache the API in a `OnceLock`; recheck termio's BrokenPipe path.
11. **Quitting overwrites config.toml with the startup snapshot** [re-verified]. `on_app_quit` saves whenever settings were not discarded (`crates/app/src/main.rs:521-535`, `ui/settings/state.rs:121-127`). `AppSettings` loads once and nothing watches config.toml, so hand edits to modeled keys made while the app runs revert at quit. Fix: keep the last persisted `Config` as a baseline and save only when the live config differs; that baseline replaces `discard_on_exit`.
12. **DeepSeek replies probably do not stream token by token** [code difference re-verified; effect needs a manual check]. The follow request sends `{address, maxMessages}` (`crates/agent/src/dsh/events.rs:248-250`). The harness's own client sends `assistantStream: true` (`D:\Park\deepseek-harness\packages\api\session-controller\src\client\transport.ts:181`), and harness commit f99b06eaed moved live tokens into those frames. The adapter still maps the older `assistant/chunk` shape (`dsh/mapping.rs:492-508`, `:646-696`). Confirm by watching a reply on the pinned 0.1.5-rc.1.

### Medium

13. Vertical tab "Rename" does nothing for rows under an inactive workspace: `start_tab_rename` searches `active_tabs()` only (`crates/app/src/ui/shell/mod.rs:2353-2372`); use `tabs_for_tab(id)` [re-verified].
14. A stray `[Image #` before a real placeholder drops the attachment: `placeholder_spans` jumps past the real placeholder when the digit parse fails (`crates/agent/src/images.rs:245-267`), then `reconcile` clears `items` [re-verified]. Fix: on parse failure set `cursor = digits_at`; add a regression test.
15. Deleted or edited agent profiles keep being probed and can raise hourly update error cards: `reconcile_profiles` prunes only the app's list (`crates/app/src/agent_updates/mod.rs:176-213`) and `UpdateCoordinator` has no removal operation [re-verified].
16. `nmt_platform` no longer compiles for Linux: `path::Path`, `fs::read`, `fs::read_link` at `crates/platform/src/unix/mod.rs:595`, `:1105`, `:1136` have no `std::path`/`std::fs` module import (`:30-43`) [re-verified]. The flatpak branch also exports `TERM=rio` (`:589`). Either add a Linux type-check job or declare Linux unsupported.
17. Releasing a dragged scrollbar thumb still sends a mouse-up to the terminal (`crates/app/src/terminal_tab/pane_model/mod.rs:601-616`); with mouse reporting on, the program receives an unmatched release [code path re-verified].
18. Codex background-task discovery can stay at `Loading` after the panel reopens: `seen_cursors` clears only on root change, so page 1's cursor is dropped on the second open (`crates/agent/src/codex/app_server/background_tasks/mod.rs:95-129`, `:606-608`, `:708-715`). A late failure for an old root's `thread/list` surfaces as a session error (`control.rs:107-113`, `app_server/mod.rs:970-979`).
19. DeepSeek replay of a log whose last turn never closed restores `running = true` and turns the next prompt into `steer` (`crates/agent/src/dsh/session/mod.rs:1016-1043`).
20. DeepSeek tab renames never reach the harness: tab renames go through `rename_session`, which answers Unsupported for DeepSeek, while `/rename` is offered only for DeepSeek (`crates/agent/src/session/backend/mod.rs:339-352`, `:626-640`).
21. Windows registry reads run on the settings render path (`shell_integration_dll_mismatched()` at `crates/app/src/ui/settings/mod.rs:301` [call site re-verified], plus System page getters at `system_page.rs:116-137`, `222-245`); registry and COM writes run in click handlers.
22. The shell session mirror misses thread-control changes: `remember_defaults` emits nothing, so quitting before a turn ends saves the old model/effort (`crates/app/src/agent_tab/thread_controls/defaults.rs:37-45` -> `crates/app/src/ui/persistence/snapshot.rs:165-167`).
23. While `scrollbar_override` is set, every PTY write runs `format_text` over screen plus scrollback and drops scroll commands (`crates/terminal/src/ghostty/mod.rs:385-391`, `:538-562`, `:691-715`).
24. Clipboard writes and PNG decodes run on a runtime worker inside the PTY task (`crates/terminal/src/session/proxy.rs:110-116`, `ghostty/callbacks.rs:229-270`; app `terminal_tab/frame_source/mod.rs:410-412`).
25. The transcript typewriter slices `text[..previous_bytes]` unchecked (`crates/app/src/agent_tab/transcript/typewriter.rs:50`). `changes_since` keeps the last delta's offset while `merge_completed` replaces the text, so a completed text shorter than the streamed prefix, or one with a shifted multibyte boundary, panics on the UI thread [mechanism re-verified; rare trigger].

### Low

26. The built-in profile is named "PowerShell" on every platform (`crates/app/src/ui/settings/state.rs:66-72`); the same literal is a fallback title at `ui/shell/mod.rs:894-898`, `tab_bar/horizontal.rs:128-132`, `tab_bar/vertical.rs:145-148` [re-verified].
27. `hook_store::write` claims crash safety and writes plus renames with no fsync or write-through (`crates/agent/src/hook_store.rs:220-235`) [re-verified]; `update::write_cache` uses a fixed `.tmp` name with no fsync (`update/mod.rs:832-845`).
28. `crates/agent_hook_cli/tests/hook_console_subsystem.rs` reads PE offsets with no `#![cfg(windows)]`; off Windows the binary is an empty `main` (`src/main.rs:92-93`) [re-verified].
29. Unix signals a pid after reaping it (`crates/platform/src/unix/mod.rs:897-936`, `unix/process.rs:208-213`).
30. A Windows shell path with spaces and no args reaches CreateProcessW unquoted (CWE-428) (`crates/platform/src/windows/mod.rs:209-211`).
31. IPC servers serve one client at a time with no read timeout; the unix accept error path busy-loops (`crates/platform/src/windows/ipc.rs:95`, `unix/ipc.rs:151`).
32. Frozen terminal history is bounded only by a fixed 139 MB engine default; `set_block_budget_bytes` has test callers only (`crates/terminal/src/ghostty/mod.rs:796-807`).
33. The branch chip shows a stale branch when the title-bar Git setting is off (`crates/app/src/ui/shell/mod.rs:3347-3358`).
34. `RootAvailability::refresh` has no ordering guard (`crates/app/src/ui/shell/workspace_dirs.rs:388-404`).
35. DeepSeek fork-checkpoint and workflow-transcript frames carry no session id and can land in a newly switched conversation (`crates/agent/src/dsh/session/loads.rs:397-446`, `actions.rs:417-430`).
36. Hard-coded English user strings: `"Git unavailable"` (`crates/app/src/agent_tab/view/composer_status.rs:327`), `"Could not read the pasted image"` (`composer/attachments/mod.rs:493`) [both re-verified], updater "PID n" recovery names, the coordinator's restore-failure message.
37. Every malformed Claude control response reports "malformed file restore response" (`crates/agent/src/claude_code/stream_json/control.rs:499`).
38. Input-history scope canonicalizes on the UI thread; on Windows it yields `//?/c:/...` keys, while a missing directory falls back to `c:/...`, so one workspace can get two histories (`crates/agent/src/input_history/mod.rs:54-72`).
39. The API-key hint in the profile dialog is wrong for Codex custom URLs and for DeepSeek (`crates/app/src/ui/settings/agent_profile_page.rs:693-697`, `958-979`; `crates/agent/src/profile.rs:9-15`, `101-113`).
40. `config`'s `&str` codec maps `"hidden"` to Block while serde maps it to Hidden (`crates/config/src/lib.rs:82-111`).

## 2. Security and ownership decisions

- Windows updater trust: the package and its `.sha256` both come from `niumaterm-downloads.f32.io`, and nothing verifies a signature or a pinned key before `replace_files` swaps executables (`crates/updater/src/windows/download.rs:60-68`, `:199-252`; `releases.rs:39-50`, `:143-163`). Chunked responses bypass the size caps. Options: minisign/ed25519 over the manifest, or WinVerifyTrust on the staged exe; stream text reads with a byte counter.
- Profile credentials are encrypted with a key compiled into the binary (`crates/profile/src/lib.rs:369-379`). The DPAPI helpers lost their callers with the remote-session removal.
- Team automatic summaries have no producer (section 3). 7b98d4e7 kept them deliberately; delete them or hide the toggle.
- Format decisions pending: a Team snapshot VERSION bump for any Room field removal; the persisted default for `InputHistoryScope.target`; agent update cache fields.

## 3. Refactoring findings by area

Area reviewers grep-verified each item; [re-verified] marks a second check by the coordinator.

### Cross-cutting (coordinator)

- RULE impl placement, nine blocks [re-verified]: codex `impl Session` in `crates/agent/src/codex/app_server/questions.rs:187`, `team.rs:41`, `title_generation.rs:84` (type at `mod.rs:123`); dsh `impl Session` and `impl Drop for Session` in `crates/agent/src/dsh/session/actions.rs:27`, `:492` (type at `session/mod.rs:51`; a second in-file impl block at `:1084`); `platform::Clipboard` constructors and `Default` in `crates/platform/src/windows/clipboard.rs:7,15` and `unix/clipboard.rs:13,35` (type in `clipboard.rs`, shared by both platforms, so the cfg-pair exception does not apply).
- P6 `crates/app/src/agent_tab/transcript/format.rs:162` `hidden()` is a verbatim copy of the public `nmt_agent::transcript::conversation::hidden` (`crates/agent/src/transcript/conversation.rs:272`) over the same `Item` type; the controller's notification decision and the view's row grouping depend on the two agreeing [re-verified].
- P2/P5 forwarders: `crates/terminal/src/session/config.rs:6` `default_shell()` (one caller); `crates/agent/src/hook_store.rs:130` `home_dir()` (six callers).
- RULE-spirit glob imports: `use crate::ui::shell::*` (3 files) and `use crate::ui::settings::*` (8 pages) expose a parent's private import list to children, the same coupling `use super::*` creates.

### crates/agent/src/claude_code

- P3 `ControlState` keeps one request in three id-keyed ledgers (`stream_json/control.rs:31-38`, `76-99`, `133-139`, `173-185`; `stream_json/mod.rs:467-504`, `799-801`, `1039-1064`, `1295-1300`). Each send makes four calls; `poll_timeouts` fabricates an error response to reach removal; `send_user_message` drops the ids `request()` returned and re-parses them. Collapse to one entry per id `{class, deadline, input, operation}` with `Init`/`Effort` variants. Preserve effort ordering, the fatal INIT timeout, cancellation in `close()` and `cancel_generated_title`.
- P3 `ClaudeTasks::created_epoch`/`epoch`: the filter in `advance_epoch` is always true (`tasks/mod.rs:86-91`, `358-363`, `415-434`) [re-verified]. Unverified side question: if stream-json emits `system/init` per turn, `init` marks background agents that outlive a turn as Stopped.
- RULE `on_control_response` compares `request_id` with `INIT_REQUEST_ID` three times, two always true (`stream_json/mod.rs:1299`, `1322`, `1335`).
- P6 live and restored child readers build launch, result-state and status updates twice (`sessions/task_history.rs:264-283`, `307-311`, `393` vs `tasks/mod.rs:507-527`, `579-588`, `286`); move them into `tasks/records.rs`.
- P6 three assistant content-block decoders have drifted (`stream_json/transcript.rs:350-391`, `sessions/replay.rs:229-302`, `records.rs:221-275`): only two accept `server_tool_use | mcp_tool_use`; only replay trims text and maps `isApiErrorMessage` to `Item::Error`; a child row changes shape depending on which read landed last.
- P6 two `user_prompt_text` functions with different joins and blank handling (`stream_json/parse.rs:15-41` vs `sessions/titles.rs:254-322`; `tasks/children.rs:96-104`).
- P6 workflow lifecycle decoding duplicates task lifecycle decoding and drops `paused` (`workflows/mod.rs:260-278` vs `tasks/records.rs:69-91`); `replace_number` duplicates `replace_text` (`background_task/mod.rs:573`).
- P6 hand-rolled bounded maps -> `IndexMap` (`tasks/records.rs:136-175`, `tasks/shells.rs:54-69`, `161-191`, `workflows/mod.rs:34-41`); `reserve_shell_meta` at `tasks/mod.rs:668` pushes duplicate order entries and evicts real metadata early [re-verified].
- P6 the fork writer hand-rolls temp-file publishing (`sessions/fork.rs:272-317`) -> `NamedTempFile::new_in` + `persist_noclobber`.
- RULE trivial helpers extracted for tests: `enable_file_checkpointing` (`stream_json/mod.rs:1404`), `file_rewind_request` (`:1495`), `initial_ready_model` (`:1400`), `legacy_command_catalog(bool)` (`parse.rs:182`), `window_from_composition` (`transcript.rs:445`), `oauth_status_allows_cli_fallback` (`usage_fetcher.rs:211`); `remaining_percentage(Option)` is always passed `Some`.
- P5 `ClaudeCheckpoint::parent_message_id` read only by tests (`sessions/mod.rs:83`); `TranscriptIndex` snapshot fields computed on every read and used only by `checkpoints()` (`sessions/index.rs:16-45`).
- P1/P2 records.rs tests live in `stream_json/tests.rs:1211-1296`, forcing `pub(super)` at `records.rs:156`, `181`; `#[cfg(test)] use` blocks in `sessions/mod.rs:38-66` and `stream_json/mod.rs:33-63` exist only for glob imports.
- Docs attached to the wrong functions (`stream_json/mod.rs:392-396`, `555-557`, `682-683`); the usage fetcher has two identical section scans and strips terminal sequences twice (`usage_fetcher.rs:409`, `559`, `591-640`).
- Not covered: large test-file bodies, hook/control/generation tests, the usage-fetcher PTY flow.

### crates/agent/src/codex

- P2/P3 `RouterState.root_by_owner` is written and never read (`host/router.rs:123`, `137`, `247`, `258`, `635`, `666`).
- P3/perf descendant ownership re-syncs on every processed message, cloning ids and locking the router mutex the host reader also uses (`app_server/mod.rs:374-378`, `401`; `background_tasks/mod.rs:137-139`; `router.rs:595-616`); it also re-claims a child dropped on `thread/closed`. Claim once when `CodexTasks::confirm` reports new ids.
- P5/RULE slash commands compared as strings in six places (`app_server/mod.rs:499`, `505`, `513`, `529`, `983`, `1140`; `control.rs:30`; `protocol.rs:49-69`, `141-152`); parse once into `enum CodexCommand`. `codex_command_response(name, Option)` receives a value both call sites already know.
- P8 `EarlyMessages` is a wrapper left from removed retention limits (`host/router.rs:690-718`, tests `720-796`; `host/tests.rs:218-257`).
- P5 `RequestPurpose` variants never told apart (`host/router.rs:35-72`, `351`); `ThreadScope::Parent`/`Unscoped` handled identically, forcing `thread_id.unwrap_or_default()` (`background_tasks/mod.rs:39-48`, `160-174`; `app_server/mod.rs:1239-1261`).
- P6/P1 the same bounded FIFO map hand-written twice (`background_tasks/mod.rs:66-69`, `285-308`; `launch_messages.rs:11-95`) -> `IndexMap`; fold `launch_messages.rs` into its only consumer.
- P6/P8 Codex hook tests repeat the shared installer tests (`codex/hook_tests.rs:93-241` ~ `claude_code/hook_tests.rs:85-230`); `hook_store` has none of its own.
- P5 boolean results read only by tests: `set_root` (`background_tasks/mod.rs:92-95`), `observe_raw_response_item` (`:763`), `LaunchMessages::observe` (`launch_messages.rs:50`).
- RULE trivial wrappers: `parse_title_generation_result(method)` (`title_generation.rs:213-227`), `is_legacy_compaction_notification` (`compaction.rs:103-105`), `resolved_title` (`title_generation.rs:76-82`). `CodexHost::send` repeats the abort `write_tracked` already performs and inspects its result twice (`host/mod.rs:246-279`).
- P2 `provisional_title_from_prompt` routes a plain `provisional_title(prompt, None)` call through the adapter (`title_generation.rs:413-418`).
- P2/P5 27 `pub use` re-exports that nothing imports through this path (`app_server/mod.rs:8-17`); `#[cfg(test)] use` lines for glob imports (`:43-68`); the `Arc<dyn Fn(Value) ...>` alias defined three times (`mod.rs:121`, `title_generation.rs:31`, `host/mod.rs:33`).
- P3 `Session.detached` equals `control.is_closed()` / `host.is_none()` (`app_server/mod.rs:128`, `334`, `367-369`, `383`, `802`).
- Minor: stacked docs at `app_server/mod.rs:406-411`; `parse_replay`'s doc on `parse_fork_checkpoints` (`protocol.rs:518-521`); an `"unknown"` arm identical to `_` (`protocol.rs:720-721`).
- Not covered: `update.rs` and `usage_fetcher.rs` compared with their Claude counterparts.

### crates/agent/src/dsh

- P2 tool `view` plumbing for a shape the harness stopped sending in a42c0b523a (`mapping.rs:162-173`, `218-234`, `404-461`; `history.rs:144`; tests `tests.rs:1562-1810`). Build call rows from parsed arguments and read `data["meta"]` for results. Preserve `meta.diffs`, exit-code parsing, unknown tools keeping name and raw arguments.
- P2 the `stream/error` arm and `ReplayFrame.error` have no producer (`mapping.rs:196-201`; `frames.rs:105-106`; `session/mod.rs:993-1000`).
- P3 `subagent_modes` duplicates `subagent_rows` (`session/mod.rs:128`, `436`, `503`, `620`, `765-774`; `actions.rs:147`); `ProjectionTracker.permission` mirrors `presets` (`projections.rs:32-37`, `122-133`, `208-214`); `job_activity` has no reader (`session/mod.rs:137-139`, `790-796`).
- P4 `on_replay` maps every record twice (`session/mod.rs:1016-1039` and `history.rs:108-218`).
- P6 request payloads built in several places (`session/cancel` at `actions.rs:478`, `585-589`, `controls.rs:103-106`; `session/updateQueue` at `actions.rs:349-353`, `577-583`; `$events/result` at `api.rs:196-208`, `actions.rs:47`, `90`; `session/selectModel` at `actions.rs:192-202`, `loads.rs:549-559`).
- P6/P1 frame type names split between producer literals (`events.rs`) and consumer constants (`session/mod.rs:153-173`); `REPLAY_MESSAGES` is documented as the replay window, and the real window is the literal `200` at `events.rs:195`.
- RULE `Switched` decoded twice (`session/mod.rs:874-885`); `payload["type"]` read five times (`:529`, `533`, `560`, `579`, `634`); `resume_thread` and `remove_queued_prompt` always return `true`; `agent_args` wrapper with a tautological test (`catalogs.rs:222-239`, `tests.rs:821`).
- P5 `Delivery` alias defined twice (`events.rs:31`, `controls.rs:18`) and spelled out 18 more times.
- P1 frame decoders in `loads.rs:145-232` live away from their consumer in `session/mod.rs`.
- P8 the HTTP loopback harness appears four times (`tests.rs:34-87`, `api_tests.rs:13-53`, `controls_tests.rs:13-53`, `switch_tests.rs:14-47`); about 150 lines.
- Structural: background-read results round-trip through JSON `nmt/*` frames (13 frame names, serde structs, `failed_read_events`, two error-field spellings). A typed local path would remove most of that; keep the in-channel ordering `switch_conversation` relies on.
- Not covered: host.rs lifecycle, api.rs login/cookies, models.rs, workflows.rs folding, most of tests.rs.

### crates/agent/src/session and team

- P2 Team automatic summaries have no producer: `room.summaries.push` appears only in tests; `Invocation::PublicSummary`, `StageKind::Summary`, `AttemptState::Summarized` are never constructed (`team/model.rs:101-136`; `room.rs:25-43`, `252-284`, `329-477`; `attempt.rs:33-39`; `validation.rs:52-89`) [re-verified]. The UI toggle only switches between two errors that both end in a ContextSelection pause.
- P7/perf every attempt keeps its full prompt (`prepared_text`, up to 96 KB) forever; each Team operation clones the Room before its no-op check and commit deep-compares it (`team/attempt.rs:28`; `storage/mod.rs:171-198`; `team/session/mod.rs:599`, `616`, `934-955`). Keep the prompt only while Reserved.
- P3 derivable ledgers in the persisted Room: `Member.coverage` (`member.rs:17-31`; `outcomes.rs:54-82`); `ArrangementState` mirrors attempt outcomes through seven hand-synced writes and `accept()` skips one (`discussion.rs:84-94`, `263-274`); `Room.input_history` duplicates UserInput messages (`room.rs:27`); `Discussion.objective` is never read; `Budget.report_reserved` is always true on disk (`budget.rs:17-114`).
- P4 the app builds `ConversationWork` from the controller's own state and passes it back (`session/update_readiness.rs:13-65`; app `agent_tab/execution/mod.rs:915-950`).
- P6/BUG rename split into `rename_conversation` (DeepSeek) and `rename_session` (Claude/Codex); see bug 20.
- P4 moderator-decision rejection is handled in two layers and can create two pause entries (`team/session/mod.rs:1049-1165`; app `team/operations.rs:403-430`).
- P6 `dispatch::finish` repeats `outcomes::fail` (`team/session/dispatch.rs:151-186`, `outcomes.rs:161-194`); the room history reader re-implements snapshot decoding (`team/storage/history.rs:49-62` vs `storage/mod.rs:115-143`).
- P2 dead or test-only entry points: `SessionController::disconnected` (`controller/mod.rs:1438-1459`), `TeamSession::saved_rooms`, `TeamSession::dispatch` (tests only), `Room::rename_member` (`cfg(test)`); over-wide `validate_mode`, `prepare_update_stop`, `apply_replay`.
- RULE helpers public only for app tests: `scoped_background_tasks` (`children.rs:40-48`), `resolve_ready_settings` (`settings.rs:44-82`, two bool parameters, one call site passes constants), setters for public fields.
- P6 `ChildAgents::claim_restore` and `WorkflowData::claim_restore` are identical; `workflows.rs:234` re-checks ids it just looked up; `session/tests.rs:6-22` hand-rolls a temp dir.
- Not covered: session test files beyond a skim; branch/restore/input state machines in depth.

### crates/agent root, chat, transcript, subprocess, input_history, background_task, update; agent_hook_cli

- P2 background-task rows carry provider data nothing reads: most of `BackgroundTaskRefs` (`background_task/mod.rs:53-131`) and per-row `parent_session`, `agent_type`, `updated_at`, `model`, `depth` (`:178-202`); Codex `depth_of` exists only to fill `depth`. Keep the Claude stop ids and DeepSeek `continuable`.
- P2 `AgentProjection.latest_unread_text` and notification `order` are computed inside every shell render and never read (`monitor.rs:94`, `117-127`, `431-456`, `469-490`); `native_group` is always `"NiumaTerm"`.
- P2 `WorkflowAgent` write-only fields `phase_title`, `isolation`, `prompt_preview`, `result_preview` (`workflow.rs:57-71`).
- P2/P3 `VersionStatus.remediation` and `.provider` are unread; `cacheable_status` exists only to strip `remediation` (`update/mod.rs:154-164`, `811-819`); `register(provider, ...)` restates the provider (`:420-426`).
- P5/P6 `ProviderKind` restates `AgentKind` names and ids; the AgentKind->ProviderKind conversion is written twice (`session/capabilities.rs:144`, app `agent_updates/transaction.rs:348`). Keep the digest input bytes (`"claude\0"`, `"codex\0"`).
- P2 `InputHistoryScope.target` is left from remote sessions (`input_history/mod.rs:21-51`; `store.rs:161-168`, `254`, `267`); keep `"local"` inside the migration hash.
- P3/P6 task progress has two sources; Claude and dsh render `TodoWrite` as markdown checklists that `task_tally` parses back (`chat/mod.rs:155-183`, `transcript/mod.rs:216-222`; `claude_code/records.rs:158`, `dsh/mapping.rs:600-633`).
- RULE `request_native_delivery` wraps one comparison and is tested twice (`monitor.rs:528-533`).
- RULE/P6 the shared input ticket encodes Claude's message schema (`subprocess/input.rs:24`, `77-79`, `196-198`).
- P5 `BackgroundTaskTranscriptUpdate` uses two bools for three modes and `state` is always `Some` (`background_task/transcript.rs:30-84`).
- P2 English `&str` impls for states used only by one app test (`workflow.rs:134-156`, `background_task/mod.rs:587-599`, `chat/mod.rs:567-574`).
- P6 atomic-write helpers copied: `hook_store::write`, `update::write_cache`, `input_history/store.rs:202-244` ~ `nmt_config::persistence::update`; one helper (NamedTempFile, `sync_all`, `replace_file`) covers all three.
- P6 the Claude adapter copies `launch_env_value` for `ANTHROPIC_MODEL`, and the two launch-model readers disagree on the `launch.model` fallback (`profile.rs:64-66`, `197-221`; `stream_json/mod.rs:1380-1397`).
- P2 dead or test-only API: `AgentCli::environment` (`launcher.rs:89`), `PendingAttachments::attach` (`images.rs:78-90`), `try_write_batch` single caller, `record`'s ignored bool, `FetchCancellation` alias, `TranscriptEntry<M = ()>` default, re-exports `AgentProcess` and `AGENT_HOOK_EXE_ENV`.
- P2 the direct `tungstenite` dependency duplicates `tokio_tungstenite::tungstenite`.
- Not covered: `crates/agent/tests/**` bodies; timer races in `subprocess/requests.rs`; `git.rs` cache concurrency.

### crates/app/src/agent_tab/{transcript,view,composer,questions,profile}

- P3 `TranscriptView` repeats writes the controller already made: `start_working`, `discard_turn` (`transcript/view/mod.rs:367-388`) [re-verified] and `push` (`:949-976`), whose only production payload is `Error`, so its image and scroll branches are unreachable. Push errors through the session and call `sync_content()`.
- P8/P2 a test-only write API on `TranscriptView` (`view/tests.rs:13-191`) keeps `source_revision` and `RowSource::all_specs` alive; incremental and profiling tests measure a path production never takes.
- perf `shut_height` is computed for every revealed row on every frame, O(k^2) at block ends (`view/mod.rs:506-524`; `row_structure.rs:338-461`); compute it only while a disclosure moves.
- perf CodeView restyles the whole text every render and clones the full output on a dirty `ensure` (`code/mod.rs:70-79`, `167-170`, `326-330`); the excluded tool-kind list is duplicated and an `.expect` relies on both copies agreeing (`code/mod.rs:43`, `72`, `84`; `code/source.rs:41`).
- P5/RULE one-bool enums and test-only predicates: `ComposerAction` (`composer/mod.rs:49-80`), `UpdateOverlayPhase` (`view/blocking_overlay.rs:46-70`), `TurnSummary` (`rows.rs:181-199`), `feedback_is_current`/`feedback_is_transient` (`composer/palette.rs:290-304`), `is_dark_surface` (`render/text_style/mod.rs:81-83`).
- P6 a provider difference hard-coded in the view (`format.rs:404-406`); it belongs in `Capabilities`.
- P8/P2 app tests of agent launch logic keep two re-exports and a `cfg(test)` import block alive (`profile/agent_profile_launch_tests.rs`; `profile/mod.rs:1-15`).
- P3 the collapse mode is stored twice (`view/mod.rs:90`, `1247-1259`; `incremental.rs:19`).
- P1 items away from their only consumers: `last_response_label`, `USER_BUBBLE_*`, the types in `questions/mod.rs`, viewport geometry stored on `ImagePreviewLayer`. P5 redundant aliases (`EntryMetadata as EntryPresentation`, `is_work_item as is_work_row`) and `TRANSCRIPT_THUMBNAIL` duplicating `THUMBNAIL`. P2 18 `pub(super) use` re-exports with no outside consumer (`transcript/mod.rs:3-17`).
- Docs on the wrong functions (`view/mod.rs:171-172`, `527-529`; `row_structure.rs:74-76`).

### crates/app/src/agent_tab root, team, session, execution, thread_controls

- perf/P7 the Team scheduler runs a full room refresh on every member-session notify, which fires per streamed delta batch (`team/mod.rs:435`; `execution/mod.rs:442`; `team/operations.rs:34-58`, `113-136`). Schedule on a small per-member key instead.
- P3/P7 `ReadyDefaults` is rebuilt and pushed into the controller before every event (`execution/mod.rs:649`, `1472-1489`); its only reader is `finish_ready`.
- P2 `TeamCommand::MemberSettings` and `Skip` are never constructed; `Exclude` only in tests (`team/controls.rs:25-35`; `team/operations.rs:277-331`) [re-verified]. `skip_arrangement` is reachable only through `Skip`.
- P2 start-with-identity plumbing left by 2089e938: `SessionOwner::start(recovery)` always receives `None`; `start_session` builds a Claude identity nobody reaches; `start_session_with_options` has a dead callback (`execution/mod.rs:187-193`; `mod.rs:3196-3251`, `2600-2631`).
- P5/P4 `AgentPaneEvent` is emitted from two entities plus a `cfg(test)` channel, so title tests cannot observe host-emitted titles (`mod.rs:141-171`, `789`, `2894-2901`).
- P2 `BranchUpdate::Branching` arms are unreachable, and branch updates are decoded in three places (`execution/mod.rs:496-504`; `mod.rs:376-389`, `533-594`, `2217-2219`).
- P2 unused accessors: `AgentPane::kind`, `profile_name`, `recovery_readiness`; `AgentSession::downgrade`; duplicate `working_directory`/`cwd`; `cx` parameters that are only passed on.
- P6/P4 title-claim logic duplicated between the pane and Team members (`mod.rs:3073-3082`; `team/member_host.rs:95-121`).
- P5 `ExecutionOutcome` has four variants and one used distinction (`team/operations.rs:354-364`); `PaletteDirection` and `InputHistoryDirection` are the same enum.
- P6 slash-command outcome presentation written twice (`mod.rs:1643-1694`, `2320-2361`); workflow refresh interest is a hand-rolled counter the view mutates (`workflows.rs:24-70`) while child refresh already returns a guard.
- P1 fragment files: `session/turn.rs`, `capabilities.rs`, `session/history/mod.rs`, `team/events.rs`, effort constants in `thread_controls/mod.rs:45-101`, `context_usage.rs`.
- P6/P8 five `open_pane` test-helper copies; pure agent-function tests in app `session/tests.rs:1-312`.
- Minor: each lazily attached member pane starts a git-branch poll and a history scan that member panes never show (`team/view.rs:404-431`; `mod.rs:2788-2792`); `Interrupted` is emitted twice on a failed start (`execution/mod.rs:1285`, `1539`); `send_text_inner` carries text only to discard it.

### crates/app/src/ui/{settings,persistence,background_tasks,right_panel,title_bar,git_sidebar}

- P6/P5 settings rows are written field by field (25 switches, 12 dropdowns, 5 number inputs, 12-35 lines each) with string round-trips that `nmt_config` keeps only for these pages (`ui/settings/*_page.rs`; `state.rs:183-211`; config `appearance.rs:481-580`, `system.rs:94-132`, `agent.rs:134-174`, `update.rs:38-54`). Three typed helpers (`switch`, `choice`, `number`) over one `AppSettings::edit` would roughly halve ~1.5k lines. Preserve normalization, reset buttons, the UI-priority side effect, and notification registration ordering.
- P3 pages restate defaults and ranges owned by config (`appearance_page.rs:45`, `127`, `198`, `224-273`, `309-314`, `361`, `441-475`; `terminal_page.rs:43`, `66`; `about_page.rs:67`; `system_page.rs:77-79`, `217`; `fields.rs:32-37`).
- P6 the Git diff view keeps its own extension-to-language table and misses go, java, cs, kt, rb, php, lua, scala, sql, swift, zig; it is also case-sensitive (`git_sidebar/diff_view.rs:127-146`). Use `file_extension_lang`.
- P6 the effort ladder and labels are defined twice with different wording (`settings/agent_profile_page.rs:421-450` vs `agent_tab/thread_controls/mod.rs:95`, `effort.rs:45-61`); seven `settings-agent-profile-effort-*` keys can go.
- P3 background-tasks list expansion is stored twice (`background_tasks/mod.rs:61-137`, `283-303`, `388-440`); RULE `render_section` branches on its own id string; `visible_rows` is tested alone.
- P6 row and header markup duplicated across background_tasks and workflows (`background_tasks/rows.rs:91-117`; `mod.rs:390-405`, `493-538`, `587-603`; `ui/workflows.rs:102-117`, `435-462`).
- P1 `settings/table.rs` and `fields.rs` have one consumer each; persistence splits paired save/restore conversions across files and duplicates a test builder.
- P5 `SettingsPatch` borrows `Config` field by field (`state.rs:498-515`; config `application/mod.rs:310-365`).
- P8 app tests exercise `nmt_config` helpers through a `cfg(test)` re-export chain (`settings/tests.rs:395-570`).
- P3 theme and hook changes trigger the same window refresh twice (`settings/theme.rs:110-118`, `hooks.rs:95`).
- P6 the unnamed-profile label is built three times from two identical i18n keys, with different trimming.
- P1/P4 `RightPanel` exposes its child views so the shell can retarget them (`right_panel/mod.rs:57-87`, `129-135`; `shell/panels.rs:72-99`).
- perf the Git tab re-runs `git diff` plus highlighting on every poll because `snapshot_seq` increments for identical snapshots (`ui/git_status.rs:647-651`; `git_sidebar/mod.rs:61-72`, `133-164`).

### crates/app/src/ui root, shell, composition, tab_bar, workspace_sidebar

- P3 the registry session mirror depends on 25 manual `sync_session_memory` calls (`ui/shell/mod.rs:3175-3181` plus call sites, `main_surface.rs:134`, `tab_bar/horizontal.rs:625`, `671`); build the snapshot where it is consumed (quit, dock reopen) and publish once before `remove_window()`.
- P1/P4 the pane tree is split between `ui/pane_tree.rs` and a one-field wrapper `terminal_tab/terminal_layout.rs`, producing `.tree().tree()` at 13+ sites; `SplitOutcome`, `RemoveOutcome`, `resize_split` and `for_each_split_mut` exist only for that wrapper.
- perf per-tab agent and terminal status is computed up to four times per render (`ui/shell/mod.rs:813-887`, `2126-2161`, `3150-3151`; `tab_bar/horizontal.rs:141`, `vertical.rs:161`).
- P3 `doomed_workspace` duplicates the `temporary` flag (`ui/shell/mod.rs:405-408`, `527`, `1392-1396`, `3176`; `persistence/snapshot.rs:86-115`).
- P6 the close-confirmation flow is written five times and the subprocess gate twice (`ui/shell/mod.rs:950-995`, `1052-1066`, `1077-1173`, `1222-1336`, `1441-1492`).
- P1/P2 `ui/composition/` is 9 files and 377 lines; `FLOATING_SURFACE_SIDE_INSET` and `TOP_INSET` are 0.0 (`composition/metrics.rs:2`, `5`).
- P1/RULE tiny shell modules (`tab_presentation.rs` 15 lines, rename split in two files, `render.rs` holding only `ShellChrome`) and glob imports.
- P6 horizontal and vertical tab rows duplicate the 11-field snapshot, the empty-title fallback, and the reorder path (`horizontal.rs:122-143`, `611-675`, `735-750`; `vertical.rs:67-83`, `140-163`).
- P1 `ActiveList`/`HasId` belong next to `tabs.rs`; `font_picker.rs` has one consumer in settings; `TabManager<S>` is generic only for tests.
- P2/P8 `profile_root_choices` takes a list while production passes one (`tab_bar/menu.rs:74-131`).
- P6 hand-rolled `\\?\` stripping duplicates `dunce` (`ui/shell/workspace_dirs.rs:38-63`); check UNC paths.
- RULE `progress_bar_width` and `agent_tab_indicator` extracted for tests (`horizontal.rs:762-774`).
- P5 unread results and fields: `AgentNotificationState::acknowledge`, `TabSurface::disable_team` bools, `AgentRouteLocation.workspace_id/tab_id` (debug asserts only), `UpdateNotificationView.show_settings` always true.
- P6 the shell re-implements `ActiveList::focus_next/prev`, `index_of`, and `default_workspace_name()`; the home-directory fallback is copied twice.
- P3 deferred requests are scattered (`pending_agent_resume`/`pending_agent_close` in `AgentNotificationState`, `needs_focus` in `ShellChrome`); the agent activity policy is chosen in three places.
- RULE a setting is re-read per row after the shell already computed it (`workspace_sidebar/list.rs:161-162`, `drag.rs:26-27`); workspace status labels built twice per row.

### crates/app root, terminal_tab, update, agent_updates, workspace, utils

- P5/P7 `row_offsets` is always `vec![slack; rows]` (`terminal_tab/layout.rs:27-41`) [re-verified]; store one `bottom_slack`. This also fixes Kitty images partly above the top being drawn `slack` px too high in fixed-bottom mode (`paint/frame.rs:114-118`) and removes an `Arc<[f32]>` allocation per prepaint.
- P3 block-list scroll geometry is stored three times and synced by seven `update_viewport()` calls (`pane_model/list_mirror.rs:29-30`; `pane_model/frozen_hit_map.rs`; `pane_model/viewport.rs:28-33`; `pane_model/mod.rs:157-178`, `266-306`, `413-417`, `797-803`, `849-851`).
- P5 `Wake` repeats `SessionChange` and carries an unread id through a type-erased sender (`terminal_tab/wake.rs:10-91`; `frame_source/mod.rs:67-91`, `367-421`; `view/mod.rs:227-238`, `930`).
- perf frozen and live-history rows are rebuilt on every item prepaint with a String per cell (`terminal_view/item.rs:161-205` -> `frame_source/mod.rs:194-364` -> `block_list/rows.rs:103-128` -> `frame/line.rs:237-300`); cache by the existing shape key.
- P2 `TerminalCell.style_id`, `extras`, `has_cursor` and `TerminalLineData.cursor_col` are read only by tests (`frame/line.rs:17-35`, `74-91`, `174-176`).
- P3/P6 `AgentUpdates.registrations` stores a derivable key; key derivation is written twice (`agent_updates/mod.rs:41-241`; `agent_tab/execution/mod.rs:903-909`); see bug 15.
- P4/P2 restore failures are folded twice; the coordinator's branch runs only in tests (`agent_updates/transaction.rs:26-48`, `192-212`; `crates/agent/src/update/mod.rs:642-683`).
- P3/P5 the pane copies the in-flight command block while callers only test presence (`pane_model/mod.rs:79-80`, `180-204`).
- P5/P1 usage refresh: `FetchError` mirrors `UsageFetchError`, a redundant cancel arm, `providers[0]`/`[1]` indices in nine places, sources away from consumers (`usage_refresh.rs`, `usage_sources.rs`, `agent_usage.rs`, `daily_usage.rs`).
- P1/P5 `terminal_launch.rs`, `terminal_layout.rs`, `terminal_status.rs` are compiled into the executable through an inline `mod terminal_tab {}` in `main.rs:61-67`, so two different modules share one folder; move them under `ui/`. `TerminalVisual` is `TerminalActivity` minus `Idle`, and the mapping is tested twice.
- P1 one-type pane_model files (`blocks.rs`, `scroll.rs`, `selection_geometry.rs`, `key_action.rs`, `mouse.rs`) and `frame_source/items.rs`; `GenerationMap` defined in `frame_cache.rs` and used by `frame/`.
- P2 a `cols` argument threads through block geometry that ignores it (`block_list/geometry.rs:14-26`, `63-80`; `reconcile.rs:40`, `54`; `list_mirror.rs:97-109`; `terminal_view/item.rs:28`, `98-107`).
- P2 unused dependencies `base64`, `lsp-types`, `raw-window-handle` [re-verified]; `tempfile` is test-only and belongs in `[dev-dependencies]`. `toml` is used in production (`ui/settings/theme.rs:19`) and stays.
- P6/P7 CLI flags are encoded into `nmt://` URLs only to be parsed back (`main.rs:148-154`, `256-273`; `cli.rs:31-40`, `108-131`).
- P6 translation catalogs are compiled twice, because both the library and the executable declare `mod i18n;` and run `rust_i18n::i18n!`.
- RULE test-only helpers and results: `affected_installation_indices` (`agent_updates/transaction.rs:127-157`), `DirtyState::begin_frame`'s bool (`terminal_tab/dirty.rs:21-29`), `GenerationStore::install`'s result (`graphics.rs:272-278`).
- RULE `keymap.rs:51-54` justifies a binding with "see the terminal-split-panes change" [re-verified]. Stale docs: remote sessions (`keymap.rs:66-67`), `RedrawRequested` (`dirty.rs:1-2`), a nonexistent byte-size field and "the block store" (`graphics.rs:78-80`, `250-262`). Unreachable color arms (`frame/colors.rs:115-121`). "Expired" is detected by comparing translated strings (`agent_usage.rs:350-360`, `401-404`).
- Not covered: `update/file_users.rs`, `workspace/roots.rs`, `platform_style/*.rs`, most test bodies, `examples/transcript_highlighting.rs`.

### crates/terminal

- P2 remote-session leftovers: `terminal_responses`, `output_sink`, `OutputSink`, `Msg::Checkpoint`, `CheckpointRequest`, `format_vt_state`, `Stage::Checkpoint` (`event.rs:97`, `328-343`; `termio/mod.rs:127-132`, `353-363`, `423`, `464-471`, `515-517`, `1159-1177`; `termio/session.rs:14-44`, `138-139`; `ghostty/mod.rs:922-927`; `session/mod.rs:199-201`; `crates/profiling/src/pty.rs:25`). Production always passes `true`/`None`. Move `assert_history_preserved` in `tests/conpty_typing.rs:258` to `Query::Text`.
- P6 hot path: frame capture reads the viewport twice, once through `render.update()` and again through per-cell grid refs with hand-rolled palette resolution (`ghostty/mod.rs:1105-1156`; `ghostty/grid_read.rs:83-255`; `render_state.rs:69-72`, `139-212`). Read cells through the render-state row and cell iterators and reuse clean rows; benchmark with vtebench.
- P3/P6 title and cwd are polled on every flush, mirrored in `TitleMirror` and in every frame, and published on two channels in different forms (`ghostty/mod.rs:103-136`, `271-304`, `1123-1126`; `render_buffer.rs:36-37`, `105-111`). The engine has TITLE_CHANGED and PWD_CHANGED callbacks.
- P6 scrollback lines are converted to bytes by a heuristic (`termio/mod.rs:250-262`, `286-290`; `ghostty/mod.rs:170-186`); use `SCROLLBACK_MAX_LINES` and confirm how it treats zero.
- P2 `engine_blocks = false` ("classic mode") exists only for tests (`session/config.rs:26-32`, `99`; `termio/mod.rs:145-151`, `484`, `921`; `termio/marks.rs:40`, `76`, `133`; `session/mod.rs:119-122`, `221`, `246-248`).
- P2/P5 command records carry unread `seq` numbers and placeholder fields; `launch_cwd: Option<Option<PathBuf>>` (`termio/marks.rs:49-131`; `event.rs:35-80`; `block_store.rs:41`, `106`; `prompt_sniffer.rs:291-298`, `388-395`).
- P3 the prompt sniffer runs two state machines in lockstep; the `forward` region and trust tags are unused in production; `ShellBoundaryTrust` wraps a bool (`termio/prompt_sniffer.rs:27-50`, `331-476`; `termio/mod.rs:486-490`).
- P6 hot path: page reads re-copy the 256-entry palette per row, bypass the acquired block, and do an O(scrollback) pin lookup per screen row (`termio/requests.rs:158-227`; `ghostty/mod.rs:835-899`, `987-1035`; `ghostty/block.rs:19-207`).
- P2 dead representations: grid `Hyperlink`, `CellFlags`, several `Mode` flags (seven written and never read, costing seven FFI calls per flush; DECSET 1004 and 1007 are therefore unimplemented), `UpdateQueues.pending`, `window_bg_override` (`grid.rs:33-36`, `306-375`, `491-535`; `vt_modes.rs:8-59`; `graphics.rs:72-81`; `render_buffer.rs:78-81`, `151-155`, `392`).
- P5/P6 two word/line selection algorithms (live vs frozen blocks) and `FrozenSelectionPiece` identical to `TextPiece` (`selection.rs:203-553`; `session/selection.rs:107-261`); the engine exposes `ghostty_terminal_select_word`/`select_line`.
- P5 `InteractiveState` and `AltScreen` report one edge twice (`termio/mod.rs:157-161`, `339-351`, `657-660`; `event.rs:121-127`).
- P6 paste and mouse encoding are hand-written beside the engine encoders; the X10 path drops clicks past column/row 222 and ignores UTF8/URXVT/SGR-pixel modes (`session/mod.rs:495-501`, `578-599`, `806-818`, `948-987`; `crates/input/src/lib.rs:233-282`). Add encoder tests before switching.
- P2 public API wider than its callers (`Termio`, `PtyState`, `SessionHandles`, `FrameStore`, many `GhosttyTerminal` methods); `BlockItem::handle()` always returns `Some`; forwarders `with_render_buffer`, `Termio::pty_write`, `Termio::channel`; `pub use nmt_platform::clipboard`.
- P2 twelve unused dependencies: bytemuck, cursor-icon, dirs, flate2, lazy_static, rapidhash, regex, serde, smallvec, toml, unicode-width, url [re-verified].
- Not covered: graphics.rs placeholder decoder, grid.rs operator impls, links.rs, powershell_compatibility.rs, vt_trace.rs, prompt_sniffer parse functions, test files, the two-step resize overflow workaround (`ghostty/mod.rs:500-528`).

### crates/platform, config, updater, input, profile, profiling, runtime, shell_extension, tree_sitter_bundle, version

- P2 about 430 lines of dead Unix PTY/process code, some unsound: execvp's argv lacks a NULL terminator (`unix/mod.rs:95`); `ptsname` is declared with the wrong parameter type and its result is unwrapped (`:1071-1089`). Items: `forkpty`, `ptsname`, `default_shell_command`, `create_pty_with_fork`, `kill_pid`, `command_per_pid`, `tty_ptsname`, `foreground_process_name`/`_path`, `spawn_daemon`, and the macOS `proc_*` helpers. The lint markers at 742, 915, 938 and 1145 are accurate; 864 is stale.
- P1/P2 updater-only modules live in `nmt_platform` (`windows/self_update.rs`, `restart_manager.rs` with 393 lines of tests, `file_version.rs`, `windows/process_exit.rs`); `unix/process_exit.rs` has no caller. Two stacked scripted fakes cover one Restart Manager session.
- P4/P6 the updater formats recovery names in English that the app already localizes (`crates/updater/src/windows/file_users.rs`; `mod.rs:44`, `217-223`; `status.rs:24-27`; app `update/file_users.rs:242`).
- P5 Restart Manager decodes six reboot reasons, service names, session ids and seven application kinds; callers use only `is_empty()` and `Explorer` (`restart_manager.rs:24-90`, `339-391`).
- P1 the shell-extension COM server lives in `nmt_platform` (`windows/shell_extension.rs`, 267 lines) and pulls `percent-encoding`/`windows-core` into every consumer; the CLSID is written twice.
- P3 two home-directory resolvers and two per-user app-directory derivations (`dirs::home_dir` vs `environment::home_dir`; `config_dir(home)` vs `data_dir()`), which diverge when LOCALAPPDATA is redirected.
- P2 remote-session leftovers: `windows/data_protection.rs` (plus `tests/windows_system.rs`) and `computer_name` on both platforms.
- P2 `Clipboard::new(RawDisplayHandle)` is unused, so the Wayland path never runs while `nmt_terminal`'s default feature still builds `copypasta/wayland`.
- P2/P6 the process-tree API is wider than its callers (`other_process_count`, Windows `attach_or_kill`/`attach`); `SoftReady` is a one-consumer module with unused `clear()`, `Clone` and `Arc`; PowerShell leftovers (`LEGACY_SHELL`, `newest_install`, three spellings of the default shell).
- P6 UTF-16 helpers duplicated (`win32_string` at `windows/mod.rs:264` vs `wide` in `file_version.rs:115`, `restart_manager.rs:260`, `ipc.rs:35`, plus inline copies); hook-command validation copied per backend (`unix/hook_command.rs:24-43`, `windows/powershell.rs:82-100`); macOS notification permission code split across `macos_notifications.rs` and `unix/notifier.rs`.
- RULE `create_termp(utf8: bool)` is always called with `true` (`unix/mod.rs:250`).
- config: the `CursorShape` <-> `char` conversions are unused (`config/src/lib.rs:45`, `93`); several `pub` items are used only inside their crate (`APP_ID`, `Winsize`, config `default_*` helpers, `local_state_file_path`).
- P2 unused dependency `iovec` in platform.
- Crate boundaries: fold `runtime` (40 lines) into platform, since all four consumers already depend on it. Keep `shell_extension` (Explorer loads it as a cdylib; it should own its COM code), `version` (build-dependency of four build scripts), `profile`, `profiling` (the gpui fork re-exports it), `tree_sitter_bundle`. `input` is borderline.
- `crates/web_client/` holds only the Aug 31 build output of the local web-client wasm port (untracked, covered by `dist/` in `.gitignore`, a 285 MB `.wasm`). Its sources and `docs/research/web-client-progress.md` are gone, and `nmt_remote_net`, which it reused, was removed on 2026-09-18. Whether to delete it depends on whether the web-client port continues.
- Not covered: config's colors/theme internals, input's key encoding correctness, the tree_sitter ABI mirror in `app/src/syntax.rs`, sparkle.rs, COM refcount soundness in `shell_extension.rs`, `macos/login_shell.rs`.

## 4. Suggested order

1. Small, contained bug fixes: bugs 1, 6, 7, 8, 9, 10, 13, 14, 16, 17, 28.
2. Team pause exits (bug 2) with one regression test per pause reason; decide the summary feature first.
3. Claude path resolution (bugs 3-4) and the background child read (bug 5).
4. Mechanical sweeps: unused dependencies (terminal, app, platform, agent), remote-session leftovers in terminal/agent/platform, dead Unix code.
5. Engine-capability adoption in terminal (callbacks, scrollback lines, render-state cells, encoders), measured with vtebench before and after.
6. Larger P3 consolidations: Team Room ledgers (needs a snapshot VERSION decision), `ControlState`, the TranscriptView write path, the shell session mirror, block-list geometry, settings row helpers.
