# Code Governance Audit: Test-Only Code, Over-Abstraction, Silent Errors, Platform Leakage

Baseline: commit `d8b4906d`, branch `dev`.
Scope: every Rust file under `crates/` (705 files, about 6.0 MB), excluding
`third_party/`.

Line numbers were taken from the baseline commit and will drift. Locate code by
item name (function, type, constant), never by line number alone.

## 0. What was searched for

| # | Class of problem |
|---|---|
| 1 | Tests whose assertions cannot fail, or that exercise logic with no possible branch |
| 2 | Functions that exist only so a test can call them, including a function whose body forwards to a second implementation used by the finished product |
| 3 | Chains where A is used only by B and B only by C, or an abstraction layer that adds no behaviour |
| 4 | Fallible operations whose error is discarded, so no caller can react |
| 5 | Platform-specific behaviour implemented outside `nmt_platform` |

The repository already forbids class 1 and class 2 in writing. `AGENTS.md`
states: "Do not extract a trivial expression (a bare `&&`, a single comparison)
into a free function merely to unit-test it, and do not write tests that only
exercise such a wrapper." Findings below are the places where that rule and its
siblings are not yet applied.

## 0.1 Method

Three passes were combined, and every reported location was read directly
before it was written down.

1. Whole-workspace name census. Every `fn` and every `struct`/`enum`/`trait`
   definition in a non-test file was indexed, then every occurrence of each name
   was classified by whether the containing file is a test file. A function
   whose only occurrences outside its own definition are inside test files has
   no finished-product caller, which is the mechanical signature of class 2.
2. Pattern scans for `#[cfg(test)]`-gated items in non-test files,
   `#[cfg(test)]` immediately preceding a `fn`, platform `cfg` predicates in
   non-test files, and the silent-error family (`let _ =`, `.ok()`,
   `unwrap_or_default()`, `Err(_) =>`).
3. Structured reading of every crate, module by module, judging each test and
   each abstraction against the five classes.

The name census is a filter, not a verdict: a function with no non-test caller
may be dead public surface rather than a deliberate test hook, and both readings
are recorded below where they differ.

## 0.2 Severity guide

- **High** — the tests pass while the shipped behaviour is wrong or unreachable,
  or a user-visible failure is invisible.
- **Medium** — dead or duplicated production surface that a reader must decode,
  or a failure that reaches no log.
- **Low** — a redundant assertion, a redundant accessor, or a naming-only layer.

## 0.3 Counts

| Class | High | Medium | Low | Total |
|---|---|---|---|---|
| 1. Unfalsifiable tests | 1 | 15 | 28 | 44 |
| 2. Test-only code | 9 | 24 | 21 | 54 |
| 3. Over-abstraction | 1 | 6 | 5 | 12 |
| 4. Silent errors | 9 | 16 | 6 | 31 |
| 5. Platform leakage | 6 | 6 | 2 | 14 |
| **Total** | **26** | **67** | **62** | **155** |

---

# 1. Unfalsifiable tests

## 1.1 High

### The only test covering three vtebench scenarios can pass with zero assertions run

- `crates/terminal/src/session/vtebench_tests.rs:190`, `:258`, `:364`; the
  spawn-failure early return is at `:301-308`.

```rust
fn ctrl_c_on_alt_screen_recovers_block_mode() {
    let Some((session, mut all)) = trusted_session() else {
        return;
    };
```

When `powershell.exe` cannot be spawned, each test returns before its first
assertion and the runner reports green. The assertions themselves are sound
where they execute, so the whole regression value of the file (alt-screen latch,
RIS tail, engine-block freeze) can disappear silently with no skip signal.

Suggested action: mark these `#[ignore]` with a run instruction, or return a
failure naming the missing prerequisite instead of returning early.

## 1.2 Medium

Each entry here is a test whose assertion is satisfied by construction, so it
detects only a coordinated edit of both the test and the code.

| Location | Assertion restates |
|---|---|
| `crates/app/src/agent_tab/transcript/tests.rs:41` | `AGENT_DISCLOSURE_DETAIL_INSET` compared to the sum of the three constants it is defined as (`disclosure_row.rs:48-49`) |
| `crates/app/src/agent_tab/transcript/render/working_indicator_tests.rs:34` | the cluster width, where `DOT_GAP` is derived from that same equation (`render/mod.rs:264-265`) |
| `crates/app/src/ipc_tests.rs:150` | `monitor.project(&routes)` called three times with no mutation between calls, then `tab == pane` and `workspace == pane` |
| `crates/app/src/terminal_tab/scrollbar/tests.rs:10` | `SCROLLBAR_AUTO_HIDE_DELAY` and `SCROLLBAR_FADE_OUT_DURATION` against their own definitions in `geometry.rs:3,5` |
| `crates/app/src/tabs_tests.rs:32` | `Tab::title()` before any title is set, so `default_title` is returned verbatim |
| `crates/app/src/terminal_tab/block_list/layout_tests.rs:255` | the three field additions performed by `offset_frozen_chrome` (`chrome.rs:131-140`) |
| `crates/app/src/terminal_tab/wake_tests.rs:8` | the payload the test's own closure stored; the code under test is `(self.0)(wake)` |
| `crates/agent/src/tests.rs:463` | `request_native_delivery`'s single `!=` (`monitor.rs:538-543`) |
| `crates/agent/src/session/backend/attachment_tests.rs:8` | the fields that a branch-free `map` copied (`backend/mod.rs:886-893`) |
| `crates/config/src/application/tests.rs:575` | `Ok(Config::default())` returned for an absent file against `Config::default()` (`application/mod.rs:219-222`) |
| `crates/config/src/application/tests.rs:587` | four comparisons of `f() == f()` through the same serde `default =` functions, with `default_cursor()` repeated on lines 587 and 589 |
| `crates/platform/src/windows/shell_integration_tests.rs:10` | the registry root list, re-spelled as literals; the test named `unregister_only_removes_owned_roots` never calls `unregister_shell_integration` |
| `crates/app/src/ui/shell/tests.rs:92` | `should_confirm_close(confirm, WarnBeforeTerminatingShell::Disabled, 0)`, where `Disabled.should_warn(_)` is unconditionally `false` (`config/src/system.rs:18-24`), so both assertions reduce to `confirm` and `!confirm`; the child-process count is inert and the `WhenChildProcessesRunning`/`Always` behaviour the setting exists for is never exercised |
| `crates/app/src/ui/settings/tests.rs:155` | the compositing relation `surface + (1 - surface) * image == w`, which holds for every input because `effective_background_image_layer_opacity` is defined as `w*i/(1-surface)` with `surface = w*(1-i)` (`settings/opacity.rs:61-72`) |
| `crates/app/src/ui/tab_bar/tests.rs:13` | the production formula re-derived as `tab_width - UI_RADIUS - bar_width == UI_RADIUS`, already implied by the concrete `px(134.0)` expectation above it |

Related, same shape, lower impact: `crates/config/src/local_state_tests.rs:14`
and `:111`, `crates/config/tests/agent.rs:11`, `crates/config/tests/system.rs:11`,
`crates/config/tests/appearance.rs:11` (whole-struct equality against the same
`default` function the deserializer calls), `crates/config/src/application/tests.rs:538`,
`crates/config/src/colors/term_tests.rs:9`, `crates/remote_net/src/client_tests.rs:75`,
`crates/profiling/src/frame_stats/disabled_tests.rs:7`,
`crates/profiling/src/transcript/disabled_tests.rs:6`,
`crates/tree_sitter_bundle/src/tests.rs:25`.

## 1.3 Low

- **Test-local values that never reach the code under test.**
  `crates/app/src/agent_tab/commands_tests.rs:231` asserts `history_dismissed`
  (bound to `true`) and a vector against an identical literal, where
  `palette.reset_discovery(false)` receives neither.
  `crates/agent/src/tests.rs:658` asserts `local_pane_id == 1` for a local bound
  to `1` on line 654.
- **Assertions on state the call cannot change.**
  `crates/agent/src/team/context_tests.rs:143` (`input` is passed by shared
  reference), `:207` (`prepare_context` takes `&self`),
  `crates/agent/src/team/storage/tests.rs:241`, `crates/agent/src/team/tests.rs:59`,
  `crates/agent/src/workspace_tests.rs:26-27`,
  `crates/agent/src/team/session/tests.rs:99-100`.
- **Identity arms re-asserted.**
  `crates/terminal/src/input_tests.rs:13` asserts `WheelDelta::Rows(3.0).lines() == 3`
  for a mapping whose `Rows` arm is the identity, where the neighbouring
  rounding cases already carry the check.
  `crates/terminal/tests/render_buffer.rs:75` compares `style_table()[sid]` with
  `style(sid)`, two accessors over the same slice (`render_buffer.rs:233`,
  `style.rs:114-127`).
- **Production expressions rewritten in the test.**
  `crates/terminal/src/pty_pipe/scrollback_tests.rs:8` writes
  `scrollback_bytes(10_000, 80) == 10_000 * 80 * 16` for a function whose body is
  exactly that product (`pty_pipe/mod.rs:217-223`).
  `crates/version/src/tests.rs:70` restates the two-arm `matches!` of
  `Version::same_channel`.
- **Field copies asserted back.** `crates/agent/src/claude_code/compaction_tests.rs:4`
  and `:39`, `crates/agent/src/claude_code/update_tests.rs:39`,
  `crates/agent/src/codex/app_server/title_generation_tests.rs:31`,
  `crates/agent/src/codex/app_server/host/router/early_tests.rs:31`,
  `crates/agent/src/dsh/tests.rs:760` (`commands::agent_args` re-implemented with
  the same literal).
- **Fixtures that make the named property untestable.**
  `crates/app/src/agent_tab/composer/attachments/tests.rs:123`: `png(4, 4)` gives
  all three attachments identical bytes, so `a_link_resolves_to_the_image_its_placeholder_names`
  and `moving_a_placeholder_reorders_the_attachments` pass whichever index
  resolves.
- **A test asserting a default its input cannot move, under a name describing a
  different scenario.** `crates/terminal/src/prompt_sniffer_tests.rs:204`:
  `boundary_trust` starts `Untrusted` and the `;A ;B ;B` stream never reaches the
  arm that sets it.
- **Literal-against-literal comparisons.**
  `crates/app/src/ui/sidebar_resize_tests.rs:10` builds two `ResizeDrag` values
  from string literals and asserts `is_from` against those same literals, so all
  four assertions are `"a" == "a"` / `"a" != "b"`. The failure the doc comment
  describes (one gesture resizing both columns) is never exercised — no
  `DragMoveEvent`, no listener. `is_from` itself is live
  (`workspace_sidebar/mod.rs:335`, `right_panel.rs:184`), so only the test is
  worthless. The same file's `:27`
  (`assert_ne!(WORKSPACE_SIDEBAR_HANDLE, RIGHT_PANEL_HANDLE)`) compares two
  `&'static str` constants, which the compiler already settles.
- **Duplicated blocks.** `crates/config/tests/colors.rs:91` repeats the block
  above it verbatim; `:118-128` repeats it again; `:23` and `:50` are the same
  test over the same inputs.
- **A duplicated assertion inside one test.** `crates/config/src/application/tests.rs:587`
  and `:589`.

---

# 2. Functions that exist only for tests

## 2.1 High — tests drive a path the product never runs

These are the findings that matter most, because the test suite reports success
while the code path a user reaches is a different one.

### `AgentTab::on_event` is a test-only second event path

- `crates/app/src/agent_tab/mod.rs:3444-3457` (test-only) versus
  `crates/app/src/agent_tab/execution/mod.rs:600-659` (production).

```rust
// test-only, agent_tab/mod.rs
#[cfg(test)]
pub(crate) fn on_event(&mut self, event: SessionEvent, cx: &mut Context<Self>) {
    self.prepare_ready_defaults(cx);
    let effect = { ... state.apply_event(state.runtime.epoch(), event) };
    self.present_session_effect(effect, cx);
}
```

The production entry point additionally checks `is_closed()` and
`runtime.is_current(epoch)`, rewrites `Event::HostExited` into a reconnect plus a
fatal error, diverts `SessionEffect::Branch` to `on_branch_update` with an early
return, handles `SessionEffect::StatusDetail`, and calls `publish_activity`.
None of that runs in the roughly thirty call sites in
`composer/branch/tests.rs`, `session/tests.rs` and `input_history_tests.rs`.

Suggested action: give the tests a way to call the production path (for example
by exposing the execution state's `on_event` to the pane's test module) and
delete the pane-level copy.

### `prepare_ready_defaults` duplicates `AgentSession::prepare_defaults`

- `crates/app/src/agent_tab/mod.rs:3884-3909` (test-only) versus
  `crates/app/src/agent_tab/execution/mod.rs:1465-1482`.

The three `SettingsSeed` arms are identical apart from how the kind and profile
are obtained. The test-only copy is reached only from the test-only `on_event`
above, so a change to the production seeding leaves the tested copy green.

### `note_replayed_response` is a test-only replay implementation

- `crates/app/src/agent_tab/mod.rs:4966-4978`, reached only from the test-only
  `on_replay` at `:3864`.

The production replay arm (`SessionEffect::Replay`, `mod.rs:3591-3604`) never
back-dates the response stamp, so the cold-cache behaviour the method's doc
comment describes is verified only against code that does not ship.

### `BackgroundTaskTranscriptUpdate::apply_to` is a second fold, not the one used

- `crates/agent/src/background_task/transcript.rs:195-215` versus
  `crates/agent/src/session/children.rs:92-156`.

Production folds an update through `ChildTranscript::apply`, which tracks
`revision` and compares entries before replacing. `apply_to` performs a simpler
fold and has no production caller; its eleven call sites are in
`background_task/tests.rs` and `app/src/ui/background_tasks/tests.rs`. The same
tests therefore validate a parallel implementation.

Related: the whole `BackgroundTaskTranscript` accumulator
(`background_task/transcript.rs:45`) has no production consumer — it appears
only in its own definition, a re-export, and three test modules — while
`ChildTranscript` re-implements its fields. `is_empty` has no caller at all.

### `complete_summary` is the only code that can finish a summary attempt, and it is test-gated

- `crates/agent/src/team/session/mod.rs:1663-1710`.

Its parameter type `SummaryText` is itself `#[cfg(test)]`
(`team/session/mod.rs:7-8`), its only call site is
`team/session/planning_tests.rs:204`, and line 1702 is the sole producer of
`AttemptState::Summarized` anywhere in the workspace. The app cannot reach it:
`crates/app/src/agent_tab/team/mod.rs:644` rejects every non-`MemberConversation`
attempt, and `complete_reply` returns `Ok(None)` for `TurnPurpose::Summary`
(line 1235).

So in a real session a public-summary attempt can never settle, and the execution
slot released by `self.slots.update(...)` inside this function is never released.

Suggested action: either give the app a real summary-completion path, or remove
the summary invocation from the production state machine and let its tests cover
only reachable code.

### The only writer of room attachments is test-gated

- `crates/agent/src/team/storage/tests.rs:404` (`RoomStore::save_attachment`).

Nothing outside that test module writes `<room>/attachments`; the product only
reads the directory (`team/session/attachments.rs:11`, reached from
`TeamSession::read_attachment` at `team/session/mod.rs:1502`), and the app builds
team input with no attachments (`app/src/agent_tab/team/view.rs:163-165`). The
file layout the read path validates is therefore produced only by tests.

### `handshake_step` claims production sharing it does not have

- `crates/remote_net/src/protocol/noise.rs:172-188`.

```rust
/// Drive `initiator` and `responder` to completion in lockstep. Shared by the
/// host/client connection setup (over real sockets they exchange the same
/// messages asynchronously) and by tests.
pub fn handshake_step(...) -> Result<Option<Vec<u8>>, NoiseError>
```

Callers are `noise_tests.rs:11,13` and `tests/loopback.rs:14,16` only. The real
connection setup steps the handshake inline while awaiting socket reads
(`host.rs:371-403`, `channel.rs:208-249`), so the doc comment is wrong and the
lockstep driver — including its `is_finished()` early return, which production
does not have — validates a sequencing that no shipped path uses.

### `block_gutter_hit` is unreachable behind a statically false flag

- `crates/app/src/terminal_tab/pane_model/selection_geometry.rs:6-11`, called
  from `pane_model/mod.rs:621` inside `if BLOCK_GUTTER_SELECTION_ENABLED && ..`
  where `const BLOCK_GUTTER_SELECTION_ENABLED: bool = false` (`mod.rs:72`).

The only live consumer is the unit test `block_gutter_hit_band`
(`pane_model/tests.rs:277-294`). This is the exact shape `AGENTS.md` prohibits: a
trivial range expression extracted into a free function so a test can call it.

### `#[cfg(test)] {set_output_sink, set_terminal_responses_enabled}` and `without_thread_for_test`

- `crates/terminal/src/pty_pipe/mod.rs:320-330`,
  `crates/terminal/src/pty_pipe/session.rs:69-77`.

Both setters write one field that production assigns directly
(`session.rs:125-126`), and `without_thread_for_test` builds a `SessionWorker`
with no worker thread, bypassing the single construction entry point
`start_session` whose doc comment calls itself "the single construction entry
point".

## 2.2 Medium — one-line forwarders and test-only accessors

Each of these has zero occurrences outside its own definition in non-test files,
and at least one test caller. Grouped by the crate that owns them.

### `crates/app`

| Location | Shape |
|---|---|
| `terminal_tab/frame/mod.rs:96,101,181` | `from_render_buffer` → `from_render_buffer_with_selection` → `from_render_buffer_reusing`, plus `extract_row` → `extract_row_with_colors`; three chained test-only constructors filling in `None`/defaults |
| `terminal_tab/frame/line.rs:19-20,66-69,78,90-93` | a `#[cfg(test)] cursor_col` field on the production line type, an ignored `cursor_col` parameter (`let _ = cursor_col;`), a test-only reader, and `ptr_eq` forwarding to `Arc::ptr_eq` |
| `terminal_tab/graphics.rs:288-291,302-305,311-314` | `get`, `drain_released`, `is_empty` as test-only views of private state; they also force a `#[cfg(test)] use std::mem;` |
| `terminal_tab/dirty.rs:32-34` | `is_pending` reading one private bit |
| `ui/settings/state.rs:36-41` | `default_shell_for_tests()` whose body is `default_shell()`, which is itself a one-line forward to `nmt_platform::default_shell()` (`state.rs:80`) |
| `ui/settings/opacity.rs:44-50` | `effective_main_view_background_opacity(effect_on_content_area, opacity)` returning `if effect { opacity } else { 1.0 }`, lifted to `pub(super)` so `settings/tests.rs:142-143` can pass a literal flag; its only production caller is `main_view_background_opacity` on the next line |
| `ui/settings/theme.rs:350-352` | `tab_background_opacity(opacity)` as a `pub(super)` one-expression function with one production call site (`:398`) and a `#[cfg(test)]` import added for `settings/tests.rs:118-119`; visibility was widened for the test, not for a second caller |
| `agent_tab/session/mod.rs:43-46` | `conversation_title_request` forwarding to `build_title_request` with a pre-bound fallback |
| `agent_tab/transcript/reveal.rs:480-504` | five accessors (`expanded_rows`, `expanded_groups`, `toggled_turns`, `expanded_annotations`, `measured_parts`) |
| `agent_tab/transcript/code/mod.rs:123-127` | `invalidate_all` reimplementing the existing `invalidate_from(0)` |
| `agent_tab/mod.rs:4424,4910,4923,2733` | `active_workspace`, `install_started_session`, `stop_for_output_failure`, `restore_question_drafts`: each forwards to a `pub`/`pub(crate)` method on the upgraded host |
| `agent_tab/team/mod.rs:201` | `member_settings`, whose one caller asserts the same value through `room().member(id)` on the next line |
| `agent_tab/profile/mod.rs:1` | re-exports plus five `#[cfg(test)] use` constants carried by a production module |
| `agent_tab/composer/branch/fork.rs:12` | `#[cfg(test)] pub(crate) use ...checkpoint_at_depth` forwarded again through `composer/mod.rs:2-3` |

### `crates/terminal`

| Location | Shape |
|---|---|
| `session/config.rs:77-83` | `has_trusted_prompt_integration()` whose body is `self.prompt_integration().is_some()`; the doc comment states it "is the same question asked without applying the answer" |
| `input.rs:36-48` | `pty_bytes_for_key` collapsing `key_action` to `Option<Vec<u8>>` and pinning `Mode::empty()`, so tests cannot distinguish `CopyOrWrite`, `Paste` and `Ignore`, and the app-cursor case has to bypass it |
| `prompt_sniffer.rs:129-131` | `feed` forwarding to `feed_hooked` with a no-op mark hook |
| `ghostty/mod.rs:1107-1120` and `ghostty/render_state.rs:86` | three test-gated layers around one `SEMANTIC_PROMPT` probe |
| `render_buffer.rs:233-235` | `style_table`, whose only readers compare it against `style(id)` over the same slice |
| `pty_pipe/ghostty_mirror_tests.rs:38-40` | a `u16 as usize` cast-forward wrapper inside a test file |

Dead production functions kept alive by their tests (real logic, no
non-test caller):

- `selection.rs:66` `visible_rows_clamped`, whose doc comment records it as a
  mirror of a retired `TermDamageState::damage_selection`.
- `selection_search.rs:242` `vpos`, a convenience constructor for `Pos::new`.
- `pty_pipe/mod.rs:184`… `submit`, `session.rs:71`
  `without_thread_for_test` (also listed above).

### `crates/agent`

| Location | Shape |
|---|---|
| `subprocess/input.rs:30-38,121-134,183-186` | `is_cancelled`, `queued_for_test` (forwarding to `new`), `try_recv` (a second non-blocking receiver duplicating `recv`), `submit` (forwarding to `submit_tracked` and discarding the ticket) |
| `dsh/events.rs:389-401` plus `events.rs:369` | `pump_for_test` forwarding to `pump`, and a `read_started: Option<&mpsc::Sender<()>>` parameter on the production `pump` that only that wrapper ever passes, making its branch unreachable in the product |
| `subprocess/mod.rs:54-70` | `spawn` forwarding to `spawn_with_stdout_closed` with a no-op callback |
| `codex/app_server/control.rs:113-121` | `next_id` and `is_empty` forwarding to `PendingRequests` |
| `session/workflows.rs:35` | `revision`, whose field is bumped on every update and read only by `ui_split_tests.rs:221` |
| `session/children.rs:84,88` | `revision` (maintained, never read) and `is_empty` (no caller anywhere) |
| `team/room.rs:109-111` | `input_history`, whose only callers are two assertions in `planning_tests.rs` |
| `launcher.rs:247-258` | `for_test` constructing a `ProcessResult` |
| `claude_code/update.rs:46` | `with_base_url`, a test-only constructor |
| `team/mod.rs:120` | `data_directory`, an accessor with no caller at all |

### `crates/config`, `crates/platform`, `crates/profiling`, `crates/version`

- `crates/config/src/local_state.rs:252-257` `save_to`, a test-only variant of
  `update_state` that replaces the whole file.
- `crates/config/src/colors/mod.rs:257-259` `hex_to_color_arr`, whose only
  callers are test assertions (also a class-4 finding below).
- `crates/platform/src/windows/powershell.rs:52-63` `preferred_shell`: PowerShell
  7 discovery plus a `which` lookup plus `LEGACY_SHELL`, compiled into the
  library solely for `terminal/tests/conpty_typing.rs:41`. The product resolves
  its shell through `default_shell()` (`:65`).
- `crates/platform/src/windows/spsc.rs:65,71,126,132,138`: five ring-buffer
  accessors carrying `#[allow(unused)]` because only the inline test in the same
  file calls them.
- `crates/profiling/src/allocation/enabled.rs:91-95`
  `ProfilingAllocator::set_enabled`, a self-receiver forward to the free
  function, used only by two test modules.
- `crates/version/src/lib.rs:46-52` `Version::same_channel`, a two-arm `matches!`
  with no caller outside its own test; the release comparison in
  `app/src/update/releases.rs:199-260` does its own tag matching instead.

### `crates/remote_net`

- `protocol/noise.rs:167-169` `SecureChannel::remote_static`, a one-expression
  accessor whose only callers are tests; the production pinning check uses
  `Handshake::remote_static` (`channel.rs:253-261`).
- `hub.rs:446` `child_process_count` and `hub.rs:61` `SessionEvent::seq`, both
  with no caller anywhere in the workspace.

## 2.3 Judged intentional — recorded so they are not re-opened

- `crates/app/src/agent_tab/transcript/view/mod.rs:1912`
  (`build_row_specs`) and `:2157` (`sync_transcript_list`) are `#[cfg(test)]`,
  and `build_row_specs` rewrites the loop that `refresh_rows`
  (`view/mod.rs:1595-1606`) performs incrementally. It is not a stray helper:
  `incremental_tests.rs:54-63` uses the pair as a full-rebuild reference and
  asserts the incremental result equals it, which is a genuine differential
  check on `dirty_from` and the `row_cache.turns` partition points.
  One caveat worth recording: the reference always calls
  `sync_transcript_tail(0, ..)` while production passes `row_start`, so the
  nonzero-start splice is never the reference. Placing the two functions in a
  test module rather than in the tests file is the remaining tidiness item.
- `crates/terminal/src/session/mod.rs:345-348` `mark_read_only` is gated on
  `#[cfg(any(test, feature = "test-support"))]`, an explicit opt-in feature for
  cross-crate integration tests rather than a hidden test hook.
- `crates/agent/src/session/test_support.rs` and
  `crates/app/src/terminal_tab/pane_model/test_session.rs` are dedicated test
  support modules; their contents are in scope by design.
- `crates/agent/src/launcher.rs:247` `for_test` and
  `crates/platform/src/windows/restart_manager.rs:273` build values in tests;
  they construct rather than forward, which is the acceptable form.
- `crates/terminal/src/pty_pipe/mod.rs:900,1004` `read_block_row` and
  `read_screen_row` carry "test-only" doc comments but are called from
  `pty_pipe/requests.rs:212` and `:184`. The comments are stale; the functions
  are live.
- The four `*_in` methods on `session/mod.rs:687,749,776,798` are not test-only;
  they have production callers in `pty_pipe/requests.rs`,
  `session/interaction/mod.rs` and `app/src/terminal_tab/`.
- `crates/terminal/src/render_buffer.rs` `style_table` is reported above, but the
  `Dimensions` trait beside it is a real removal (class 3 below) rather than a
  test hook.

---

# 3. Over-abstraction

## 3.1 High

### `Dimensions` and `Selection::rotate` are a dead trait chain retained from the retired grid engine

- `crates/terminal/src/terminal/grid/mod.rs:14-62`, `crates/terminal/src/selection.rs:131`.

```rust
// grid/mod.rs
// The `Grid<T>`/`Storage`/resize engine and iterators were deleted with the
// `Crosswords` VT engine ... What remains is the cell-geometry surface the live
// code still uses: the `Dimensions` trait and `Row` (in `row`).
pub trait Dimensions { fn total_lines(&self) -> usize; ... }

#[cfg(test)]
impl Dimensions for (usize, usize) { ... }
```

The trait's only production implementor is the `#[cfg(test)]` tuple impl, and
its only consumer is `Selection::rotate`, which has no caller in any crate.
Seven trait methods, one `cfg(test)` impl, and a 58-line generic method are
maintained for nothing. `row::Row`, also re-exported here, is live and should
stay.

Suggested action: delete `terminal/grid/mod.rs`, the `Dimensions` import in
`selection.rs`, and `Selection::rotate`; keep `row::Row`.

## 3.2 Medium

- **`block_list_live_chrome` is a middle layer over `live_chrome`.**
  `crates/app/src/terminal_tab/block_list/chrome.rs:114-121` converts
  `Option<&InFlightBlock>` plus `has_open_prompt` into the `running` bool that
  `live_chrome` (`:66`) re-derives the accent from. `live_chrome`'s only
  production caller is this wrapper, and the wrapper's only production caller is
  `LiveItemState::layout` (`block_list/live.rs:28`).
- **Two names for one chrome computation, and three test-only opacity helpers.**
  See the `crates/app` table in 2.2 for `effective_main_view_background_opacity`
  and `tab_background_opacity`: both are single expressions whose second consumer
  is a unit test rather than a second call site.
- **`ProtocolSessionInfo` restates `hub::SessionInfo` for a path nothing calls.**
  `crates/remote_net/src/protocol/types.rs:22-28` plus
  `impl From<SessionInfo> for ProtocolSessionInfo` (`:73-83`),
  `HostBound::ListSessions` (`:33`), `ClientBound::SessionList` (`:58`),
  `client.rs:473-518`, `host.rs:676-685`, `hub.rs:454`. The only caller of
  `list_remote_sessions` is the `#[ignore]`d `tests/host_e2e.rs:253`, and
  `ProtocolSessionInfo` appears nowhere else in the workspace.
- **`BackgroundTaskTranscript` duplicates `ChildTranscript`.** See 2.1; the
  accumulator, its six accessors and its fold are a parallel copy of the type
  the app actually reads.
- **`request_authorization` is a three-hop no-op.**
  `crates/platform/src/lib.rs:126-132` → `unix/mod.rs:10` →
  `unix/notifier.rs:26`. No crate calls it, its non-macOS body is empty, and
  because `show_notification` never requests authorization either, macOS is
  never asked for notification permission.
- **`KeyEncodeFlags::APP_KEYPAD` is plumbed and never read.**
  `crates/input/src/lib.rs:33`, written by `terminal/src/input.rs:100-102`, and
  absent from every reader in `crates/input` (compare `APP_CURSOR`,
  `DISAMBIGUATE_ESC_CODES`, `REPORT_ASSOCIATED_TEXT`, `REPORT_ALTERNATE_KEYS`).
- **`Session::shutdown` forwards to `detach_with` with an identical signature.**
  `crates/agent/src/codex/app_server/mod.rs:299-302`; `detach_with` has no other
  caller, so one of the two names is redundant.
- **`title_line` aliases `provisional_title_from_prompt`.**
  `crates/agent/src/claude_code/sessions/titles.rs:397-399`, a pure alias for a
  function 28 lines above in the same file, with two call sites.

## 3.3 Low

- `crates/app/src/agent_tab/session/history/mod.rs:22-28`: two wrappers over
  methods of the `pub(crate) data` field, which the same module already reaches
  directly (`mod.rs:3954`).
- `crates/app/src/agent_tab/execution/registry.rs:29-34` `SessionRegistry::get`
  and `execution/mod.rs:278` `AgentSession::id()`: no caller; the registry map is
  written and read through `sessions()`, which ignores the keys, so the
  `SessionId` newtype key exists for a lookup nothing performs.
- `crates/agent/src/claude_code/update_tests.rs:20`: `FakeReleases` is the only
  reason `ClaudeReleaseChannel` exists as a trait, and the test asserts the
  double's own constant return rather than driving `ClaudeMaintenance::probe`.
- `crates/app/src/agent_tab/team/mod.rs:120` `data_directory`, listed in 2.2.
- `crates/agent/src/claude_code/sessions/titles.rs:397`, listed above.

## 3.4 Judged intentional

- `AgentCapabilities` (`crates/agent/src/session/capabilities.rs:157`) and
  `AgentKindExt` (`crates/app/src/agent_tab/profile/mod.rs:47`) each have one
  implementor, but the implemented type `AgentKind` belongs to `nmt_profile`.
  An extension trait on a foreign type is the sanctioned shape and is exempted
  in `AGENTS.md`.
- `ProcessReadWrite` / `EventedPty` (`crates/platform/src/lib.rs:59,111`) have
  six implementors across three crates and are consumed generically by
  `terminal::pty_pipe`; the Windows and Unix implementations are genuinely
  parallel rather than one relaying the other.
- `crates/agent/src/monitor.rs`, `request_policy.rs`, `message_memory.rs`,
  `workflow.rs` and `team/storage/` were each consumer-checked and carry
  behaviour rather than forwarding.
- `WorkflowSource` has two implementors (one real, one in-memory test double),
  which is the shape a trait is for.

---

# 4. Errors discarded so no caller can react

The scans matched 471 sites in non-test source. Most are correct: a send on a
channel whose receiver has already gone, a best-effort `Drop` cleanup, a
telemetry write, or a panic-path log write where no better option exists. The
entries below are the ones where the failure is meaningful and invisible.

## 4.1 High

### The Unix notification path reports success for every delivery failure

- `crates/platform/src/unix/notifier.rs:78-93`, `:111`.

```rust
pub(crate) fn show(notification: &NativeNotification) -> Result<(), String> {
    let Ok(connection) = Connection::session() else { return Ok(()); };
    let Ok(proxy) = Proxy::new(&connection, "org.freedesktop.Notifications", ...) else { return Ok(()); };
    ...
    let _: Result<u32, _> = proxy.call("Notify", &(...));
    Ok(())
}
```

Three failure modes — no session bus, no notification service, `Notify` rejected
— plus the unimplemented `remove` all become `Ok(())`. The only caller's error
branch (`app/src/ui/shell/mod.rs:3010`) is therefore unreachable, so a user whose
desktop never shows notifications receives no signal at all.

### macOS notification delivery errors are dropped and authorization is never requested

- `crates/platform/src/unix/notifier.rs:62-66`: the completion handler is `None`,
  so a rejected delivery produces no error and `show` returns `Ok(())`. Combined
  with `request_authorization` having no caller (3.2), nothing ever asks macOS
  for permission.

### Unregistering shell integration cannot fail, so the settings toggle lies

- `crates/platform/src/windows/shell_integration.rs:45-52`, `:99`, `:118`.

```rust
pub fn unregister_shell_integration() -> Result<()> {
    for path in context_menu_owned_registry_roots() {
        let _ = CURRENT_USER.remove_tree(path);
    }
    Ok(())
}
```

The register half propagates its errors with `?`, so
`app/src/ui/settings/system_page.rs:99` (`if let Err(err) = result { warn!(...) }`)
is dead for the unregister half. Turning system notifications off can leave
`Software\Classes\nmt` registered while the app reports success.

### The shell-extension COM entry point returns S_OK when the launch failed

- `crates/platform/src/windows/shell_extension.rs:146-155`: the method's only
  effect is spawning the process; the spawn result is discarded, and a failed
  `folder_path` read silently yields an empty path and a wrong start directory.
  Explorer is told the command succeeded and nothing is logged.

### A failed process-count query is reported as "no processes"

- `crates/platform/src/windows/process.rs:205-209`,
  `crates/platform/src/unix/process.rs:166`,
  `crates/platform/src/unix/macos/mod.rs:162`.

`ProcessTree::process_count()` cannot distinguish an empty job or group from a
failed query. Its consumers (`terminal/src/session/mod.rs:301` →
`app/src/ui/shell/mod.rs:3704` `warn.should_warn(child_process_count)`) use the
count to decide whether to confirm before closing a tab, so a failed query
suppresses the "processes are still running" confirmation.

### A failed rollback rename leaves a half-swapped installation with no trace

- `crates/platform/src/windows/self_update.rs:113-119`, also `:102`.

`swap` guarantees that a failed update restores every moved file — asserted by
`self_update_tests.rs::failed_swap_restores_moved_files` — but both compensating
renames discard their errors, and `discard_incoming` then deletes the
`.nmt-incoming` copies while the returned error names only the original
`Replace` failure. `app/src/update/install.rs:124` only deletes `.nmt-previous`,
so the install directory can be left without its executable and no layer reports
it.

### A failed image write silently drops the attachment from a sent message

- `crates/agent/src/session/backend/mod.rs:904-921`.

```rust
if attachments.peek().is_none() || fs::create_dir_all(scratch).is_err() {
    return Vec::new();
}
attachments.enumerate().filter_map(|(index, attachment)| {
    let path = scratch.join(format!("image-{}.png", index + 1));
    fs::write(&path, attachment.bytes).ok().map(|()| path)
}).collect()
```

A failed directory creation or write makes the image vanish from a Codex message
while `SendOutcome` still reports success, so neither the composer nor the user
can learn that part of the attachment never reached the model.

### Oversized input frames are dropped while the writer reports a full write

- `crates/remote_net/src/client.rs:388-397` (`let Ok(bytes) = frame.encode() else { continue };`).

`Frame::encode` rejects any `Input` over `MAX_DATA_LEN` (32 KiB,
`frames.rs:118-121`), and `NetWriter::write` (`net_pty.rs:188-195`) sends a whole
buffer in one frame and returns `Ok(buf.len())`. Terminal input is queued as one
item (`pty_pipe/mod.rs:784-786`, written whole at `:920`), so a paste larger than
32 KiB reaches this branch and is discarded with no log, no error frame and no
signal to the engine, which has already recorded the bytes as written. The host
chunks output (`host.rs:618`) but input is never chunked.

### A config read failure is indistinguishable from a missing file

- `crates/config/src/application/mod.rs:219-222`.

```rust
let Some(content) = fs::read_to_string(path).ok() else { return Ok(Config::default()); };
```

`PermissionDenied` or a non-UTF-8 file takes the same branch as `NotFound`: the
user's settings silently revert to defaults and the caller receives `Ok`. The app
reports only TOML parse failures (`main.rs:586-589`), so the failure is
invisible.

### A known-hosts read error erases every pinned host key

- `crates/app/src/remote.rs:138-149`.

```rust
Err(_) => Vec::new(),
```

The decode arm deliberately warns ("Losing the pinned host keys silently would
look like the pairing never happened"), but the I/O error arm does not. The empty
list is then written back by `save_known_hosts` (`forget_host`:164,
`pair_with_code`:186), so a permission or sharing error destroys the pinned keys.

Suggested action: treat only `ErrorKind::NotFound` as empty, and warn for the
rest as the decode arm already does.

## 4.2 Medium

### `crates/terminal` — FFI reads that fail are published as defaults

- `ghostty/grid_read.rs:127` and `:177`: `let _ = ghostty_row_get_multi(...)`
  and `let _ = ghostty_cell_get_multi(...)`. Multi-get stops at the first error
  (comment at `:150-155`), so a rejected key leaves `cp = 0` (a blank cell),
  `has_styling = false` (styles dropped), and at row level `wrapped = false`,
  `has_link = false`, `virtual_placeholder = false`. `wrapped` is read by
  `session/selection.rs:126,137` for copy and line selection, and by kitty image
  geometry.
- `ghostty/render_state.rs:314-319` and `:229`: failed foreground, background,
  cursor-colour and cursor-style reads become `ColorRgb::default()` (black) and
  `CursorShape::Block`. The neighbouring `CURSOR_VISIBLE` read two lines above
  uses `Error::from_code(...)?`, which is what makes these silences the anomaly.
  A terminal configured for a bar cursor silently shows a block; a failed colour
  read publishes black-on-black instead of leaving the theme in place.
- `ghostty/block.rs:48,55,76,148`: `handle()` returns `BlockHandle::default()`,
  `row_count()` returns 0, `bytes()` returns 0, and `format_range_clamped`'s
  `.ok()` flattens a formatter error. `row_count()` bounds block page reads
  (`pty_pipe/requests.rs:208`), so a failed read makes a real block look empty —
  the block list renders nothing and a copy yields no text. `handle()`'s default
  instead fails the freshness check at `requests.rs:25` and surfaces as
  `Unavailable`, so the two failures are also misattributed.
- `pty_pipe/requests.rs:184-192`: an engine read error ends the scrollback page
  loop and the partial page is returned as `Ok` and cached as `Ready(Some(page))`.
  The `PageSource::Block` branch 25 lines below maps the identical failure to
  `RequestError::Engine` (`:213`), which is what makes the screen branch's
  silence the anomaly.
- `pty_pipe/mod.rs:198-206` and `:553`: `publish_render_buffer` turns the engine
  capture error into a bare `false`, the caller emits no `TerminalDamaged`, and
  the UI keeps showing the previous frame — while the exit path logs the same
  failure (`warn!("failed to publish final terminal frame: {error}")`, `:683`).

### `crates/agent`

- `update/mod.rs:841-856` `write_cache`: create, serialize, write and rename
  failures all `return`, with no result. The comment justifies losing the
  re-probeable status cache, but the same path persists `dismissed_target`
  written by `dismiss_available` (`:771`), so a failed write silently drops the
  user's dismissal and the notification returns next launch.
- `git.rs:118-131` `read_current_branch`: both `run_git` errors are `.ok()`, and
  the resulting `None` is cached for the caller's whole `max_age` window
  (`:93-109`). A missing `git`, a permission failure or a corrupt index is
  indistinguishable from "not a repository" and is remembered as that answer.
- `hook_store.rs:44-49` `status`: every `io::Error` from `read` collapses to
  `HookInstallStatus::NotInstalled`, so a settings file that exists but cannot be
  parsed is presented as "hooks not installed" — contrary to the module's own
  rule that an unparseable file is never rewritten.
- `dsh/session/loads.rs:447-450`: after a successful settings write, a failed
  model-catalog re-read leaves the picker on the old catalog with no message,
  while the neighbouring `Err(message)` arm warns and records a refusal.
- `dsh/history.rs:283-285`: `from_timestamp_millis(millis as i64).unwrap_or_default()`
  publishes an out-of-range or wrapped timestamp as a real 1970 date in the
  branch-point picker, where `None` was already available to mean "unknown".
- `claude_code/stream_json/mod.rs:1087`: the error reply written for unsupported
  `can_use_tool`/`hook_callback` requests goes through the fire-and-forget `send`,
  whose comment justifies the silence with "the reader-side EOF is the single
  exit-detection path". That reason does not cover this call: the reply exists to
  stop the CLI waiting, so a failed write hangs the turn with no log. The
  sibling codex path (`codex/app_server/mod.rs:812`) warns and delivers.
- `app/src/agent_tab/team/mod.rs:711`: `Ok(Submission::NotReady) | Err(_) => SendOutcome::NotReady`
  flattens `SubmissionBlock` (`agent/session/controller/mod.rs:116-127`:
  `QuestionResponse`, `ConversationChange`, `CommandStarting`) into a state
  documented as "the handshake has not produced a thread yet"
  (`agent/chat/mod.rs:606`). A member blocked by an open question panel is
  indistinguishable from one whose backend is still starting.

### `crates/config`

- `colors/mod.rs:257-259`: `hex_to_color_arr` maps a failed hex parse to
  `Rgba::default()` (all channels zero) instead of reporting it or falling back
  to the theme colour. The strict sibling `deserialize_to_arr` (`:412-422`) shows
  the intended handling.
- `application/mod.rs:288-292`: `pub fn init(config: Config) { set_active_colors(config.colors); let _ = CONFIG.set(config); }`.
  On a second call the process keeps the first `Config` while the global palette
  has already switched to the new one. One call site today (`main.rs:591`), so
  the inconsistency is latent.

### `crates/platform` and `crates/app`

- `app/src/ui/tab_bar/menu.rs:207`: `if let Ok(rooms) = TeamSession::saved_rooms(&config_dir_path())`
  drops the `TeamError` that `saved_rooms` returns for a failed `read_dir`,
  `entry` or `file_type` (`agent/src/team/session/mod.rs:1394-1419`). Every
  "Reopen team" entry disappears from the new-tab menu, so the user's saved rooms
  become unreachable and no log records why.
- `app/src/ui/git_status.rs:87`: each `git diff --numstat` invocation is wrapped
  in `if let Ok(out) = run_git(root, args)`, while the enclosing `fetch_snapshot`
  error is logged by its caller (`:534`). When numstat fails but `git status`
  succeeds, every changed file is reported as `+0/-0` and the titlebar total is
  blank — a wrong result that reads as a clean tree.
- `platform/src/process_lifetime.rs:9-14` `attach_or_kill`: the kill and wait that
  implement the function's own guarantee are discarded, so a child that survived
  the kill keeps running and no caller learns (`agent/src/launcher.rs:321`,
  `agent/src/subprocess/mod.rs:92`, `agent/src/dsh/host.rs:200`,
  `platform/src/unix/mod.rs:449`). Propagating would be wrong for an already
  exited child, so a `warn!` is the right weight.
- `agent_hook_cli/src/main.rs:77`: `let _ = send(&message, Duration::ZERO, testing);`
  and the surrounding silent `return`s at `:41-43,52-55,57-59,67-69`. A failed
  connect or write drops an agent event and the process still exits 0 with no
  log, so neither the agent nor the user can tell an event was lost. A short
  retry plus one observable channel (a non-zero exit status or a stderr line,
  never the payload) would cover it.
- `app/src/terminal_tab/frame/full_frame_profile.rs:214`: `let _ = fs::write(...target/frame_profile.txt...)`
  inside the ignored profiling test. When the write fails, the previous run's
  file stays and the numbers are silently stale — the one artifact that test
  exists to produce.
- `platform/src/unix/notifier.rs:26` `request_authorization` and
  `platform/src/lib.rs:129`, listed under 3.2.

## 4.3 Judged harmless — recorded so they are not re-opened

- Sends on a channel whose receiver is already gone: `agent/src/dsh/events.rs`,
  `agent/src/dsh/host.rs`, `remote_net/src/host.rs:196,210,295,313,383,437,530,570`,
  `remote_net/src/hub.rs:269,408`, `remote_net/src/client.rs:180,213,235,253,466,489`,
  `terminal/src/pty_pipe/session.rs:81` (`Msg::Shutdown` in `Drop`).
- Cancelled oneshot replies: `let _ = reply.send(...)` in `terminal`.
- `terminal/src/vt_trace.rs` best-effort trace writes;
  `terminal/src/session/page.rs:141` stale-revision `None`;
  `terminal/src/pty_pipe/marks.rs:24,92` `unwrap_or(0)`.
- `app/src/logging.rs:70-71` (panic path: the comment explains that aborting
  skips the appender guard, so a direct write is the only remaining option) and
  `:97` (removing an absent rotation file is normal).
- `profiling/src/allocation/enabled.rs:101` `let _ = COUNTS.try_with` fails only
  during TLS teardown, where recording is impossible and panicking inside
  `GlobalAlloc` is not an option.
- `agent/src/subprocess/mod.rs` `let _ = self.shutdown(...)` in `Drop`.
- `platform/src/windows/self_update.rs` `discard_previous`, whose failure is
  documented as expected at `app/src/update/install.rs:121`.
- `platform/src/windows/child.rs:41`, `windows/conpty.rs:172`,
  `unix/macos/login_shell.rs:150` (send failures once the listener is gone);
  `windows/pipes.rs:67,98` (`thread.join()` after a surfaced `BrokenPipe`);
  `unix/shell.rs:199` and `unix/mod.rs:730` (the error is already returned).
- `platform/src/windows/powershell.rs` `prompt_integration` returning `None` for a
  failed materialization: `unix/shell.rs:154` warns, so the platform answer stays
  distinguishable from "this shell has no integration".
- `agent/src/update/mod.rs` `current_version_fallback`'s `.ok()` is a named,
  visible fallback; `agent/src/input_history/store.rs` `unwrap_or_default` covers
  a pre-epoch clock and an absent scope.

---

# 5. Platform-specific behaviour outside `nmt_platform`

The scan matched 267 `cfg(target_os | windows | unix | target_family | target_env)`
sites, 149 of them in files that are not tests. Most are legitimate: choosing a
GPUI backend at the composition root, selecting a keybinding table, picking a
font name, gating a test expectation, or excluding a whole artefact crate. The
entries below carry real operating-system behaviour.

## 5.1 High

### Linux PTY errno and Unix hang-up detection in the terminal event loop

- `crates/terminal/src/pty_pipe/mod.rs:27` (`#[cfg(target_os = "linux")] use libc::EIO;`)
  used at `:1105-1108`, and `:1042-1043`, `:1061-1064`, `:1090-1094`.

```rust
#[cfg(target_os = "linux")]
if err.raw_os_error() == Some(EIO) {
    continue;
}
```

```rust
#[cfg(unix)]
let mut hup = false;
...
    #[cfg(unix)]
    if event.is_read_closed() { hup = true; }
...
#[cfg(unix)]
let skip_io = hup;
#[cfg(not(unix))]
let skip_io = false;
```

What the platform behaviour is: a Linux master-side PTY `read` returns `EIO`
when the client side hangs up, and a Unix `POLLHUP`-style read-closed event gates
whether the loop performs any PTY I/O at all. Both are operating-system rules
expressed in the terminal crate, and the second forces a `let skip_io = false`
stub for Windows. The crate links `libc` directly for the errno constant.

Suggested action: extend the `ProcessReadWrite`/`EventedPty` surface in
`nmt_platform` with a "read side closed" report and a PTY-hangup predicate on
`io::Error`, the way `drain_ready` and `has_ready` already carry Windows
soft-readiness. The loop then needs no `cfg` and the crate can drop `libc`.

### Dynamic library names chosen in the application crate

- `crates/app/src/syntax.rs:13-20`.

```rust
#[cfg(windows)]
const BUNDLE_FILE: &str = "tree_sitter.dll";
#[cfg(target_os = "macos")]
const BUNDLE_FILE: &str = "libtree_sitter.dylib";
#[cfg(all(unix, not(target_os = "macos")))]
const BUNDLE_FILE: &str = "libtree_sitter.so";
```

The file's whole purpose is loading a dynamic library, and it already depends on
`nmt_platform::library::ResidentLibrary` for the load. The per-platform filename
convention is part of the same concern and belongs beside the loader.

Suggested action: add a `nmt_platform::library` entry point that resolves and
loads a bundle by base name, and delete the three constants.

### Per-platform default shell, including the Unix `--login` convention

- `crates/config/src/defaults.rs:15-33`, with the import gated at `:3-4`.

```rust
#[cfg(not(target_os = "windows"))]
{ Shell { program: "".into(), args: vec!["--login".into()] } }

#[cfg(target_os = "windows")]
{ Shell { program: DEFAULT_CONFIG_SHELL.into(), args: vec![] } }
```

`DEFAULT_CONFIG_SHELL` is imported from `nmt_platform::windows::powershell`, so
the platform crate already owns half of the answer while the argument list and
the empty-program-means-default-shell convention are decided here. This is the
only non-UI platform branch in `nmt_config`.

Suggested action: expose one per-platform default shell (program plus arguments)
from `nmt_platform` and call it unconditionally.

### Unix directory fsync for atomic replace

- `crates/agent/src/team/storage/mod.rs:255-266`.

```rust
temporary.as_file().sync_all()?;
replace_file(temporary.path(), path)?;

#[cfg(unix)]
File::open(directory)?.sync_all()?;
```

`nmt_platform::filesystem` owns `replace_file` but not the accompanying directory
sync that makes the rename survive a crash, so the durability rule is split
across two crates and the POSIX-specific half sits in a domain module.

Suggested action: move the directory sync into `nmt_platform::filesystem` as part
of an atomic-replace helper.

### Windows path-spelling convention in the agent history module

- `crates/agent/src/input_history/mod.rs:66-76`.

```rust
#[cfg(windows)]
let spelling = spelling.replace('\\', "/");
```

The separator convention that decides which persisted history key a directory
maps to is a platform rule, and the spelling helper it builds on
(`installation_path_spelling`) already lives in `nmt_platform::filesystem`. Two
places now own path spelling, so a change to one silently changes history
identity.

Suggested action: fold the normalization into `nmt_platform::filesystem` and call
it unconditionally.

### macOS-only key normalization inside a platform-neutral encoder

- `crates/input/src/lib.rs:88-99`.

```rust
// Backspace whenever pressed with Fn(Globe button) on MacOS
// will produce `\u{f728}` as text_with_all_modifiers
// however it should act as a Delete key.
#[cfg(target_os = "macos")]
let text = if key.logical_key == Key::Named(NamedKey::Delete) {
    None
} else {
    key.text_with_all_modifiers.as_deref()
};
```

The crate documents itself as a frontend-neutral encoder over `KeyInput` and has
no other `target_os` branch. The macOS behaviour is real: the associated text of a
Delete key is discarded so Fn(Globe)+Backspace encodes as Delete rather than
emitting `U+F728`.

Suggested action: move the normalization into the layer that fills `KeyInput`
(`terminal/src/input.rs:312-336`) or into `nmt_platform`, or carry it as an
explicit field the platform layer sets.

## 5.2 Medium

- **Windows shell command construction in the Claude adapter.**
  `crates/agent/src/claude_code/usage_fetcher.rs:339-361`: one branch builds
  `cmd.exe /D /C <cli> --settings <override>` (the `.cmd`/`PATHEXT` resolution
  rule), the other builds `default_shell()` with `-c "exec <cli> --settings
  '<override>'"` and hand-written single quoting. `nmt_platform` already owns
  `default_shell()` and the launch-environment helpers, so the argv rule belongs
  there.
- **The ConPTY host stack is Windows-gated inside `remote_net`.**
  `crates/remote_net/src/lib.rs:15-34` gates `host`, `hub`, `keys` and `net_pty`.
  `hub.rs` runs ConPTY sessions and Windows job-object process trees
  (`create_managed_pty_with_env`, `ProcessTree::other_process_count`), and
  `keys.rs` stores the identity key as a DPAPI blob, so the gate selects real
  platform behaviour while the boundary is drawn in this crate rather than in
  `nmt_platform`. `net_pty` contains no OS call and is gated only to match
  (`client.rs:88`).
- **Windows thread priority and drag-cursor controls driven from `main.rs` and
  the settings page.** `crates/app/src/main.rs:276-280` and `:193-194`, with
  `#[cfg(windows)] pub(crate) struct PlatformHandle` at `:94-98`. The handle is
  republished as a Windows-only GPUI global and read back by
  `ui/settings/system_page.rs:171-176`, which calls
  `set_ui_thread_priority(value)` straight from the switch handler. What runs
  underneath is `set_thread_priority(GetCurrentThread(), ..)` on the UI and vsync
  threads (`third_party/gpui/gpui_windows/src/platform.rs:239`) plus the Explorer
  drop description. A UI toggle reaches a Win32 thread-priority call through a
  cfg-typed global instead of through `nmt_platform`.
- **Win32 handle extraction and the activation-source choice in the shell.**
  `crates/app/src/ui/shell/mod.rs:2917-2943`. The `#[cfg(windows)]` arm downcasts
  the window to `RawWindowHandle::Win32` and asks
  `nmt_platform::window::is_foreground_and_not_minimized(handle.hwnd)`; the
  `#[cfg(not(windows))]` twin asks GPUI's activation bit instead. The OS call is
  delegated, but the per-OS activation source and the raw-handle knowledge — the
  part that decides whether the user is looking at the window — sit in the UI
  layer.
- **PowerShell and cmd command lines embedded in Windows-only tests.**
  `crates/app/src/terminal_tab/view/tests.rs:77-121` launches `pwsh.exe` with a
  `$Host.UI.RawUI.WindowTitle` expression;
  `terminal_tab/frame_source/profile_tests.rs:20-33` runs a `cmd.exe /D /Q /C`
  batch loop; `terminal_tab/remote_tests.rs:78` opens `cmd.exe`.
  `crates/agent/src/launcher_tests.rs:48-54` selects `cmd.exe /D /C` versus
  `/bin/sh -c` and then writes per-platform variable syntax and
  `ping -n`/`sleep` bodies (`:14-33`, `:78-82`, `:101-105`, `:121-125`);
  `agent/src/subprocess/tests.rs:57,209` repeats it with
  `powershell.exe -NoLogo -NoProfile -NonInteractive -Command`;
  `agent/src/update/tests.rs:84,104` selects a `.cmd` extension plus
  `PermissionsExt::from_mode(0o755)` and an `@echo off` body versus a `#!/bin/sh`
  body.

## 5.3 Judged acceptable — recorded so they are not re-opened

- `crates/app/src/main.rs` module gating and backend selection
  (`gpui_macos::MacPlatform` versus `gpui_windows::WindowsPlatform`), the
  `update`/`remote`/`sparkle` module gates, and `await_predecessor`: this is the
  composition root choosing which backend to build, and `nmt_platform` cannot own
  GPUI's own platform types.
- `crates/app/src/keymap.rs`, `crates/app/src/window.rs`,
  `crates/terminal/src/links.rs:176`, `crates/terminal/src/input.rs:240`: these
  select a keybinding table, traffic-light geometry, or the Command-versus-Control
  chord for a link or clipboard action. That is input convention, not operating
  system work, and it has no syscall, path, environment variable or process.
- `crates/config/src/appearance.rs:8-26,181-208`: font family names per platform.
  Two of the six branches have identical bodies.
- `crates/app/src/ui/mod.rs` (CJK font constant, `TITLE_BAR_HEIGHT`),
  `ui/shell/mod.rs` (`MIN_SIDEBAR_WIDTH`, title-bar insets, the `NewRemoteTab`
  binding, the check-updates menu item, `on_new_remote_tab`),
  `ui/settings/about_page.rs` (updater UI),
  `ui/settings/mod.rs` and `ui/settings/state.rs` (remote-session imports, fields
  and setter), `ui/settings/remote_session_page.rs` (host and client panels),
  `ui/terminal_launch.rs` (`attach_remote`): each gate selects UI text, layout, a
  binding or a panel, and the OS work underneath is already delegated to
  `nmt_platform`.
- `crates/sparkle/src/lib.rs:21` (`#![cfg(target_os = "macos")]`) and
  `crates/shell_extension/src/lib.rs:8` (`#![cfg(windows)]`): whole-artefact
  gates. `nmt_sparkle` wraps the macOS framework; `nmt_shell_extension`'s three
  exports are the COM entry points Explorer looks up by name and all behaviour
  lives in `nmt_platform::windows::shell_extension`. These are build boundaries,
  not pass-through layers.
- `crates/agent_hook_cli/src/main.rs`'s `#![cfg]`-style gating: the binary
  contains no OS call — the named-pipe send, `MAX_MESSAGE_BYTES` and the
  `NMT_AGENT_*` names all come from `nmt_platform` and `nmt_agent`.
  One correction the audit turned up: the crate's module comment says the
  consuming agent panes are Windows-gated with it, but `crates/app/src/ipc.rs`
  and `on_ipc_agent_hook` are not gated and `nmt_platform::ipc` has a Unix
  `send`, so the comment overstates the gate. `docs/research/macos-release-ci.md`
  already records the macOS hook as unresolved.
- `crates/remote_net/src/hub.rs:532`, `src/protocol/types.rs:1,73`,
  `src/client.rs:88`: these carry no platform behaviour; they exist only because
  the hub module is Windows-gated.
- `crates/terminal/src/session/config.rs:106` `is_windows_powershell` has no
  `cfg` and is a cross-platform name predicate.

---

# 6. Cross-cutting patterns

Three patterns account for most of the volume above.

**P1. A test-shaped copy of a production path.** The most damaging findings are
not one-line forwarders but whole second implementations that tests drive while
the product drives the original: the pane-level `on_event` and
`prepare_ready_defaults`, `apply_to` against `ChildTranscript::apply`,
`handshake_step` against the inline handshake, and `complete_summary` as the only
way to finish a summary attempt. In each case the suite can stay green while the
shipped path regresses. Removing the copy is worth more than any of the smaller
items.

**P2. `#[cfg(test)]` on a production item to reach private state.** The cheap
form is one accessor reading one field (`is_pending`, `cursor_col`, the five
`reveal.rs` accessors, the five `spsc.rs` accessors). The fix that keeps the test
working is nearly always available: assert through the entry point that already
returns the transition (`begin_frame`/`mark`, `sync_transcript_tail`,
`with_shell_integration`, `key_action`), or place the accessor in the tests file
where the module privacy already permits it.

**P3. `.ok()` and `let _ =` on an FFI or engine result.** The terminal crate's
`ghostty` facade and the platform crate's Windows paths both translate engine or
COM failures into plausible defaults — black, block cursor, empty block, no
processes, success. Where the neighbouring branch already propagates or warns
(`requests.rs:213`, `render_state.rs` `CURSOR_VISIBLE`, the codex unsupported-request
reply), the silent branch is an inconsistency rather than a decision.

## 6.1 Suggested order of work

1. Delete the four test-only copies in P1 (class 2, High). This changes what the
   suite proves, so land it before the mechanical cleanups and watch for tests
   that only passed because they drove the copy.
2. Close the class 4 High list. Each one is a small edit to return, log, or map
   an error, and each currently hides a user-visible failure. The
   `config/src/application/mod.rs:219` read and `app/src/remote.rs:147` known-hosts
   arms are the two a user is most likely to hit.
3. Move the class 5 High items into `nmt_platform`. The PTY errno and hang-up
   work is the largest and pays for itself by removing the `libc` dependency and
   the `skip_io` stub; the library-name, default-shell, directory-fsync and
   path-spelling items are each a single helper.
4. Delete the class 3 dead chains. `Dimensions` with `Selection::rotate` and
   `ProtocolSessionInfo` with the whole listing path are self-contained
   removals.
5. Sweep the class 2 Medium forwarders and the class 1 Medium assertions in one
   pass per crate. These are mechanical, and the class 1 entries in `crates/config`
   and `crates/profiling` can be deleted outright.
6. Add a guard so the classes do not return. A checker that rejects a `fn` whose
   only non-test callers are test files, and a `#[cfg(test)]` item in a non-test
   file outside an allowlist, would have caught most of class 2 mechanically —
   the same shape as the existing implementation-locality checker described in
   `docs/archive/type-governance-2026-09.md`.
