# Large Source File Split Plan

Baseline: commit `b871a9ca`, branch `dev`, 2026-08-10.
Audience: implementation agents executing one task at a time. Read section 2
completely before touching any file. Every task is a pure code move; the
compiled behavior of the application must be identical before and after.

Line numbers in this document are hints taken from the baseline commit and
will drift. Always locate code by item name (struct / enum / fn / impl /
const), never by line number alone. Files under `crates/app/src/agent_pane/`
are under active development; re-verify item names there before starting a
task.

## 1. Goal and non-goals

Goal: no first-party production file over ~800 lines, test modules in their
own files, and zero import cycles between modules inside
`crates/app/src/terminal` and `crates/terminal/src`.

Non-goals for every task in this plan:

- No behavior changes, no logic edits, no reformatting of moved code beyond
  what `cargo fmt` enforces.
- No renames of existing public items, files that other modules import, or
  method signatures.
- No new dependencies, no new abstractions, no trait extraction.
- Do not touch `third_party/`.

## 2. Execution protocol (mandatory for every task)

### 2.1 Standard procedure

1. Read `AGENTS.md` at the repository root first. Its rules override habit.
   In particular: comments and docs in English; never bypass git hooks; the
   pre-commit hook rejects newly added lines containing any word from its
   banned-word list (see `check_no_ai_slop_marker` in `.githooks/pre-commit`;
   the match is case-insensitive and whole-word). Never write those words in
   code, comments, or commit messages.
2. Confirm a green baseline before editing:
   `cargo test -p <crate>` for the crate you are about to touch
   (`nmt_terminal`, `nmt_agent_utils`, or `app`). If the baseline is red,
   stop and report; do not start the task.
3. Perform the moves listed in the task. Move code verbatim: keep doc
   comments, attributes, and `#[cfg(...)]` gates attached to the items they
   annotate. Delete nothing except the moved text at its old location.
4. Fix imports and visibility per sections 2.2 and 2.3. Let the compiler
   drive: `cargo check -p <crate>` repeatedly until clean.
5. Verify:
   - `cargo fmt --all`
   - `cargo clippy --workspace --all-targets --quiet` (must be warning-free;
     the pre-commit hook also runs a first-party pass with
     `-D clippy::absolute_paths`, so use `use` imports at the top of files
     instead of long inline paths in expressions)
   - `cargo test -p <crate>`
6. Commit with the message given in the task (Conventional Commit, ASCII,
   English, lowercase subject). Include a body bullet list naming what moved
   where, and the `Co-Authored-By` trailer per `AGENTS.md`. One task = one
   commit. The commit-msg hook also rejects literal escape spellings such as
   backslash-n / backslash-r / backslash-t inside the message; write the
   message to a temporary file with real line breaks and commit with
   `git commit -F <file>` instead of passing multi-line strings to `-m`.
   If a hook rejects the commit, fix the reported issue; never use
   `--no-verify`.
7. Do not push unless asked. If asked: `dev` pushes to the `private` remote,
   never to `origin`.

### 2.2 Module mechanics

All splits in this plan use the directory + `mod.rs` shape this repository
already uses (`ansi/mod.rs`, `terminal/mod.rs`, `app/src/terminal/mod.rs`):
the file being split becomes `<name>/mod.rs`, and extracted code goes into
sibling files inside that directory. Do not use the alternative layout where
`foo.rs` sits next to a `foo/` directory.

```
crates/terminal/src/ghostty/mod.rs    <- the former ghostty.rs
crates/terminal/src/ghostty/tests.rs  <- child module
crates/terminal/src/ghostty/types.rs  <- child module
```

Rules:

- The first task that splits a given file performs the conversion:
  `git mv crates/terminal/src/ghostty.rs crates/terminal/src/ghostty/mod.rs`.
  Use `git mv` so git records a rename and file history stays traceable.
  Later tasks on the same module only add child files. The module path does
  not change (`crate::ghostty` stays `crate::ghostty`), so no `use` line
  anywhere needs editing for the conversion itself.
- A module must have exactly one root: after the conversion the old
  `foo.rs` must be gone. `foo.rs` and `foo/mod.rs` together is compile
  error E0761.
- Exception: a crate root (`lib.rs`, `main.rs`) stays a file; its child
  modules are plain files directly under `src/` (relevant to T3.10).
- Declare each child in `mod.rs`: `mod types;` (private by default),
  `#[cfg(test)] mod tests;` for test files. If an inline module was
  previously declared `pub mod` (example: `ghostty::mode`), keep that
  visibility on the new declaration.
- Moving an inline test module: the new file receives the module BODY only
  (everything between the braces of `mod tests { ... }`). Rewrite its import
  lines to crate-root form per section 2.3: `use super::*;` becomes
  `use crate::<module path>::*;` (example: `use crate::ghostty::*;` in
  `ghostty/tests.rs`), and explicit `use super::{a, b};` becomes
  `use crate::<module path>::{a, b};`. Privacy is unaffected by the path
  spelling: the tests module is still a descendant of the parent, so private
  parent items stay reachable through the crate-root path. Leave any
  `#[cfg(test)]`-gated imports or helper items that live in the parent file
  untouched; child modules can still see them.
- Splitting an `impl Type` block across files is normal Rust: move whole
  methods into a new `impl Type { ... }` block in the child file. This
  repository already does this (`AgentPane` across four files, `Shell` in
  `ui/persistence.rs`, `TerminalPane` in `terminal/links.rs`).

### 2.3 Visibility rules

- Child modules can access every private item of their ancestors, including
  private struct fields. Moving methods into a child file therefore needs no
  field visibility changes on the parent type. This is the reason the plan
  places every split under the original file's own directory.
- The reverse direction is gated: a private item that moves INTO a child file
  is visible only inside that file and its descendants. When the parent or a
  sibling child still calls it, widen exactly that item to `pub(super)`. The
  compiler errors ("function/method/field ... is private") enumerate the
  complete list; widen only what it names.
- After moving items out of the parent, add re-imports in the parent so every
  existing path keeps compiling, matching the item's previous visibility:
  - previously private and used elsewhere in the parent (including its test
    module): `use crate::ghostty::types::CellText;`
  - previously `pub(crate)` / `pub(super)` / `pub` and used outside the
    parent module: `pub(crate) use crate::...;` /
    `pub(super) use crate::...;` / `pub use crate::...;`
  This keeps all external import sites (`nmt_terminal::ghostty::CellText`,
  `crate::ui::settings::AppSettings`, `super::shell::TabSurface`, ...)
  working with zero call-site edits. Prefer adding a re-export over editing
  call sites.
- Widen visibility minimally: private, then `pub(super)`, then `pub(crate)`,
  then `pub`. Never introduce `pub` where the item was narrower before.
- Import style is crate-root only: every `use` line written or edited during
  these tasks must start with `crate::` or an external crate name.
  `use super::...`, `use self::...`, and bare relative paths
  (`use types::...`) are forbidden. This applies to re-exports too
  (`pub use crate::...`). Import lines in files a task does not otherwise
  touch stay as they are. Visibility markers are a separate mechanism and
  keep using `pub(super)` / `pub(in ...)` as specified above. The clippy
  `absolute_paths` lint checks paths in expressions and types and leaves
  `use` declarations alone, so crate-root imports pass the hook.

### 2.4 What "no behavior change" means

- `git diff` should show only: the rename of the split file to
  `<name>/mod.rs`, moved text, `mod` declarations, `use` lines,
  visibility markers the compiler forced, and the one explicitly allowed
  piece of structural glue when a task names it (example: a constructor
  replacing struct-literal construction across a module boundary).
- No test may be deleted, renamed, or weakened. Test counts before and after
  must match (`cargo test -p <crate>` summary lines).
- If a task turns out to require a real code change to compile, stop and
  report instead of improvising.

### 2.5 Ordering and parallel execution

- Tasks on different crates are independent and can run in parallel on
  separate worktrees.
- Tasks touching the same source file must run in the listed order
  (T1.x before T3.x/T4.x on the same file; T2.1-T2.3 before T4.10-T4.12).
- Within a phase, tasks on different files are independent.

## 3. Task index

| Task | File | Action | Depends on |
|---|---|---|---|
| T1.1 | terminal/src/ghostty.rs | tests out | -- |
| T1.2 | terminal/src/pty_pipe.rs | tests out | -- |
| T1.3 | terminal/src/prompt_sniffer.rs | tests out | -- |
| T1.4 | app/src/terminal/frame.rs | tests out | -- |
| T1.5 | app/src/terminal/session.rs | tests out | -- |
| T1.6 | app/src/terminal/block_list.rs | tests out | -- |
| T1.7 | agent_utils/src/codex/app_server.rs | tests out | -- |
| T1.8 | agent_utils/src/claude_code/stream_json.rs | tests out | -- |
| T1.9 | agent_utils/src/claude_code/sessions.rs | tests out | -- |
| T2.1 | app/src/terminal/ | new theme.rs | -- |
| T2.2 | app/src/terminal/ | new paint_text.rs | T2.1 |
| T2.3 | app/src/terminal/ | new layout.rs | -- |
| T2.4 | terminal/src/ | new pwd.rs, drop empty dir | -- |
| T3.1 | ghostty.rs | types + error + mode | T1.1 |
| T3.2 | ghostty.rs | callbacks | T3.1 |
| T3.3 | ghostty.rs | finished blocks | T3.2 |
| T3.4 | ghostty.rs | grid reads | T3.3 |
| T3.5 | ghostty.rs | kitty graphics | T3.4 |
| T3.6 | ghostty.rs | render state + format | T3.5 |
| T3.7 | pty_pipe.rs | conpty_realign | T1.2 |
| T3.8 | pty_pipe.rs | marks + write queue + session | T3.7 |
| T3.9 | prompt_sniffer.rs | osc_parse + command_echo | T1.3 |
| T3.10 | agent_utils/src/lib.rs | root split | -- |
| T3.11 | codex/app_server.rs | protocol + compaction + skills + options | T1.7 |
| T3.12 | claude_code/stream_json.rs | parse + launch + control | T1.8 |
| T3.13 | claude_code/sessions.rs | full split | T1.9 |
| T3.14 | app/src/terminal/frame.rs | full split | T1.4 |
| T3.15 | app/src/terminal/session.rs | config + proxy | T1.5 |
| T3.16 | app/src/terminal/surface.rs | full split | -- |
| T4.1 | ui/settings.rs | inline pages to fns | -- |
| T4.2 | ui/settings.rs | state + opacity + theme | T4.1 |
| T4.3 | ui/settings.rs | card + fields | T4.2 |
| T4.4 | ui/settings.rs | page files + dialog | T4.3 |
| T4.5 | ui/shell.rs | tab_surface + actions | -- |
| T4.6 | ui/shell.rs | agent notifications + update layer | T4.5 |
| T4.7 | ui/shell.rs | close + workspaces | T4.6 |
| T4.8 | ui/shell.rs | tabs_open + pump + panes | T4.7 |
| T4.9 | ui/shell.rs | render helpers | T4.8 |
| T4.10 | app/src/terminal/view.rs | pane split | T2.1-T2.3 |
| T4.11 | app/src/terminal/terminal_view.rs | element split | T2.1-T2.3 |
| T4.12 | app/src/terminal/block_list.rs | full split | T1.6, T2.1-T2.3 |
| T4.13 | agent_pane/transcript.rs | full split | -- |
| T4.14 | agent_pane/composer.rs | full split | -- |
| T4.15 | agent_pane/session.rs | full split | -- |
| T4.16 | agent_pane/view.rs + updates.rs | full split | -- |

## 4. Phase 1 -- move inline test modules to child files

Mechanics for all nine tasks are identical (section 2.2). Each task:
`git mv` the listed file to `<name>/mod.rs`, create the listed child file(s)
with the test-module body, replace the inline module in `mod.rs` with a
`#[cfg(test)] mod <name>;` declaration, verify, commit.
`#[cfg(test)]` items that live OUTSIDE the test module (helper structs,
imports, test-only methods) stay in `mod.rs`.

| Task | Parent file | Inline module(s) | New file(s) | Commit subject |
|---|---|---|---|---|
| T1.1 | `crates/terminal/src/ghostty.rs` | `mod tests` (~1445 lines, starts near line 2947) | `ghostty/tests.rs` | `refactor(terminal): move ghostty tests to child file` |
| T1.2 | `crates/terminal/src/pty_pipe.rs` | `mod scrollback_tests` (~20 lines), `mod ghostty_mirror_tests` (~920 lines) | `pty_pipe/scrollback_tests.rs`, `pty_pipe/ghostty_mirror_tests.rs` | `refactor(terminal): move pty pipe tests to child files` |
| T1.3 | `crates/terminal/src/prompt_sniffer.rs` | `mod prompt_sniffer_tests` (~617 lines) | `prompt_sniffer/tests.rs` (declare as `#[cfg(test)] mod tests;` and keep the module body unchanged) | `refactor(terminal): move prompt sniffer tests to child file` |
| T1.4 | `crates/app/src/terminal/frame.rs` | `mod tests` (~578 lines), `mod full_frame_profile` (~180 lines incl. its doc comment) | `frame/tests.rs`, `frame/full_frame_profile.rs` | `refactor(app): move frame tests to child files` |
| T1.5 | `crates/app/src/terminal/session.rs` | `mod tests` (~649 lines) | `session/tests.rs` | `refactor(app): move terminal session tests to child file` |
| T1.6 | `crates/app/src/terminal/block_list.rs` | `mod tests` (~599 lines, mid-file), `mod layout_tests` (~226 lines, end of file) | `block_list/tests.rs`, `block_list/layout_tests.rs` | `refactor(app): move block list tests to child files` |
| T1.7 | `crates/agent_utils/src/codex/app_server.rs` | `mod tests` (~698 lines) | `codex/app_server/tests.rs` | `refactor(agent-utils): move app server tests to child file` |
| T1.8 | `crates/agent_utils/src/claude_code/stream_json.rs` | `mod tests` (~568 lines) | `claude_code/stream_json/tests.rs` | `refactor(agent-utils): move stream json tests to child file` |
| T1.9 | `crates/agent_utils/src/claude_code/sessions.rs` | `mod tests` (~538 lines) | `claude_code/sessions/tests.rs` | `refactor(agent-utils): move sessions tests to child file` |

Notes:

- T1.3: renaming the module from `prompt_sniffer_tests` to `tests` is
  allowed here because the module is private and test-only; the body moves
  verbatim.
- T1.6: the parent has production code between its two test modules. Only
  the test modules move; production code order stays untouched.

## 5. Phase 2 -- break module cycles

### T2.1 -- `crates/app/src/terminal/theme.rs` (new)

Move into the new file, then declare `pub(crate) mod theme;` (alphabetical
position) in `crates/app/src/terminal/mod.rs`:

- from `terminal_view.rs`: `BLOCK_SUCCESS_COLOR`, `BLOCK_FAILURE_COLOR`,
  `BLOCK_RUNNING_COLOR`, `BLOCK_INPUT_COLOR`, `BLOCK_SELECTED_TINT`
- from `block_list.rs`: `SEPARATOR_COLOR`
- from `view.rs`: `BLOCK_GUTTER_WIDTH`, `BLOCK_GUTTER_GAP`

Give each const the narrowest visibility that satisfies existing users
(`pub(super)` inside the `terminal` module tree). Update the `use` lines in
`view.rs`, `block_list.rs`, `terminal_view.rs`.

Commit: `refactor(app): gather terminal block colors and gutter metrics into theme module`

### T2.2 -- `crates/app/src/terminal/paint_text.rs` (new)

Move from `terminal_view.rs`: `shape_lines`, `paint_glyph_rows`,
`paint_line_backgrounds_at`, `terminal_text_runs`, `block_separator_bounds`.
Declare `pub(crate) mod paint_text;` in `terminal/mod.rs`; adjust callers in
`terminal_view.rs` and `block_list.rs`.

Commit: `refactor(app): extract shared text shaping and painting helpers`

### T2.3 -- `crates/app/src/terminal/layout.rs` (new)

Move from `terminal_view.rs`: `frame_content_rows`, `bottom_anchor_offsets`,
`live_frame_text`, `terminal_line_plain_text`, `terminal_line_has_content`,
`row_y_offset`, `terminal_row_at_y`, `truncate_command`, plus the test
`bottom_anchor_offsets_pin_content_to_the_floor` from the file's test module.
Declare `pub(crate) mod layout;`.

While there: delete the orphan doc comment about the gutter hit band that
sits inside `terminal_view.rs`'s test module without an owning test (its test
moved to `view.rs` long ago).

Commit: `refactor(app): extract frame layout helpers from terminal view element`

### T2.4 -- `crates/terminal/src/pwd.rs` (new) + cleanup

- Move `pwd_to_path` from `pty_pipe.rs` into a new `crates/terminal/src/pwd.rs`
  (`pub(crate) fn`). Declare `mod pwd;` in `lib.rs`. Update the two callers
  (`pty_pipe.rs`, `ghostty.rs`) and remove `pty_pipe` from `ghostty.rs`'s
  imports -- after this, `ghostty.rs` no longer depends on `pty_pipe.rs`.
- Delete the empty directory `crates/terminal/src/error/` if it is still
  empty.

Commit: `refactor(terminal): move pwd path helper to its own module`

## 6. Phase 3 -- extract self-contained units

Each task follows section 2.2/2.3. "Move" lists name the items; attached
impl blocks, `From`/`Display` impls, and doc comments travel with their
items.

### T3.1 -- `ghostty/types.rs`, `ghostty/error.rs`, `ghostty/mode.rs`

- `types.rs`: `Color` alias, `color_from_vt`, `SnapshotStyle`, `Underline`,
  `CellWide`, `Palette`, `CellText` + `CellTextRepr` + its five impls,
  `RowCell` (test-gated), `ScreenRowMeta`, `ScreenRowRead` (test-gated),
  `SnapshotCursor`, `SnapshotColors`, `SnapshotPlacement`,
  `PlacementScreenPos`, `ScrollbarInfo`.
- `error.rs`: `Result` alias, `Error`, `Error::from_code`, `Display` and
  `std::error::Error` impls.
- `mode.rs`: the body of the inline `pub mod mode` const table; declare
  `pub mod mode;` in the parent.
- Parent adds `pub use` re-exports for every moved public name so
  `nmt_terminal::ghostty::CellText` etc. keep resolving (consumers live in
  `crates/app/src/terminal/`, `crates/remote_session_hub/`, and
  `crates/terminal/tests/`).
- This move should shrink the parent's giant `use libghostty_vt_sys::{...}`
  import block; remove the names that only `types.rs` needs.

Commit: `refactor(terminal): extract ghostty value types, error, and mode tables`

### T3.2 -- `ghostty/callbacks.rs`

Move: `Callbacks`, `write_pty_cb`, `bell_cb`, `vt_string_bytes`,
`clipboard_write_cb`, `decode_png_cb`, `register_png_decoder`,
`KITTY_IMAGE_STORAGE_LIMIT_BYTES`. The parent constructs `Callbacks` in
`GhosttyTerminal::new` and drains its fields in `take_pty_writes` /
`take_bell` / `take_clipboard_writes`; mark the struct and its fields
`pub(super)`.

Commit: `refactor(terminal): extract ghostty callback plumbing`

### T3.3 -- `ghostty/block.rs`

Move: `BlockRef` + its impl + `Drop`, `AcquiredBlock`, and these
`GhosttyTerminal` methods as a new `impl GhosttyTerminal` block:
`finish_block`, `clear_blocks`, `remove_block`, `block_count`, `block_at`,
`block_row_count`, `block_cols`, `block_bytes`, `blocks_bytes`,
`set_block_budget_bytes`, `reflow_block`, `block_acquire`,
`acquire_block_snapshot`, `read_block_row_visit`, `read_block_row`
(test-gated). `BlockRef`'s private fields (`raw`, `cols`) are constructed and
read from the parent (`block_placements`) -- mark them `pub(super)`. Parent
re-exports `pub use crate::ghostty::block::{AcquiredBlock, BlockRef};`.

Commit: `refactor(terminal): extract ghostty finished-block interface`

### T3.4 -- `ghostty/grid_read.rs`

Move: `grid_ref_at`, `viewport_grid_ref`, `viewport_top_screen`,
`read_screen_row` (test-gated), `read_screen_row_visit`, `visit_row_cells`,
plus free fns `style_color_resolve`, `grid_ref_graphemes`,
`grid_ref_hyperlink_uri`. `visit_row_cells` is called from `block.rs` --
mark it `pub(super)`.

Commit: `refactor(terminal): extract ghostty grid read helpers`

### T3.5 -- `ghostty/kitty.rs`

Move: `placements`, `block_placements`, `block_image_pixels`,
`take_image_deltas` (as an `impl GhosttyTerminal` block), plus free fns
`placement_scalar`, `placement_geometry`, `kitty_image_graphic_data`.

Commit: `refactor(terminal): extract ghostty kitty graphics interface`

### T3.6 -- `ghostty/render_state.rs` + `ghostty/format.rs`

- `render_state.rs`: `snapshot_into`, `consume_render_damage`, `snapshot`,
  `cursor`, `colors`, and the test-gated `has_prompt_tagged_row` /
  `row_semantic_prompts`.
- `format.rs`: `format_text`, `format_vt_state`, `format_terminal`,
  `format_screen_range`.
- The parent keeps: struct + `Drop`, lifecycle (`new`), identity drains,
  geometry/scrollbar/mode probes, color and cursor configuration, viewport
  scrolling. Target size after T3.1-T3.6: under 900 lines.

Commit: `refactor(terminal): extract ghostty render state and text export`

### T3.7 -- `pty_pipe/conpty_realign.rs`

Move these free fns (all pure, `&[u8]` in): `is_conpty_resize_echo_input`,
`for_each_csi`, `cup_row_col`, `is_cup`,
`rewrite_conpty_resize_echo_cup_rows`, `last_cup_row`, `max_cup_row_col`,
`su_realign_count`, `first_cup_row`, `contains_csi_erase_display`,
`is_conpty_resize_repaint`. Mark `pub(super)` only the ones the parent still
calls; the rest stay private to the new module. The parent re-imports them
(`use crate::pty_pipe::conpty_realign::{...};` in `mod.rs`) so the
`crate::pty_pipe::...` paths used by `ghostty_mirror_tests.rs` keep
compiling.

Commit: `refactor(terminal): extract conpty resize realign helpers`

### T3.8 -- `pty_pipe/marks.rs`, `pty_pipe/write_queue.rs`, `pty_pipe/session.rs`

- `marks.rs`: `engine_blocks_live_list`, `apply_sniffer_mark`,
  `BLOCK_BOUNDARY_CLEAR`. Both fns are fully parameterized (no `self`) and
  move as-is.
- `write_queue.rs`: `PtyState` + impl, `Writing` + impl. The parent pushes
  to `PtyState.write_list` from `drain_recv_channel` -- mark the needed
  fields `pub(super)`. `PtyState` is `pub`; parent re-exports
  `pub use crate::pty_pipe::write_queue::PtyState;`.
- `session.rs`: `OutputSink`, `SessionOptions`, `SessionHandles`,
  `start_session`. These touch `PtyPipe`'s private fields, which is legal
  from a child module; parent re-exports the four public names.

Commit: `refactor(terminal): split pty pipe support modules`

### T3.9 -- `prompt_sniffer/osc_parse.rs` + `prompt_sniffer/command_echo.rs`

- `osc_parse.rs`: `OSC133_PREFIX`, `OSC_PROGRESS_PREFIX`, `OSC133_MAX`,
  `SniffedOsc`, `region_for`, `parse_exit_code`, `parse_sniffed_osc`. The
  test file imports `SniffedOsc` and `parse_sniffed_osc` by name --
  re-import them in the parent
  (`use crate::prompt_sniffer::osc_parse::{SniffedOsc, parse_sniffed_osc};`)
  so its `crate::prompt_sniffer::...` paths keep compiling.
- `command_echo.rs`: `render_command_echo` (single caller: `apply`).
- Everything else (state machine, `PromptSniffer`, `SnifferMark`, lifecycle
  types) stays in the parent. Do not split the state machine.

Commit: `refactor(terminal): extract prompt sniffer parsing helpers`

### T3.10 -- `crates/agent_utils/src/lib.rs` root split

Create four child modules and slim `lib.rs` to declarations, shared items,
and re-exports:

- `hook_command.rs`: `HookInstallStatus`, `build_windows_hook_command`,
  `build_windows_hook_command_for`, `hook_command_contains`,
  `decode_powershell_command`.
- `event.rs`: `AgentRuntimeStatus`, `AgentEventKind`, `RawAgentHookMessage`
  + `into_event`, `AgentOwner`, `AgentEvent`, `AgentEventInput`,
  `AgentValidationError`, `validate_identity`, `constant_time_eq`,
  `normalize_title`, `normalize_body`, `normalize_presentation`, and the
  `MAX_*` limit consts.
- `process.rs`: `AgentProcess` + impl, `agent_process()`, `hex`, the
  `AGENT_*_ENV` consts, `AGENT_HOOK_PROTOCOL_VERSION`.
- `monitor.rs`: `PendingCompletion`, `AgentPaneState`, `AgentNotification`,
  `MonitorMutation`, `AgentProjection`, `AgentMonitor` + its entire impl,
  `higher_status`, `COMPLETION_QUIET_WINDOW`, `ACTIVE_STATE_STALE_AFTER`,
  `request_native_delivery`, `exact_window_is_active`. Keep `AgentMonitor`'s
  impl in one piece.
- Stays in `lib.rs`: `AgentRoute` + impl, `LaunchConfig`, the existing `pub
  mod` declarations, and a `pub use` block re-exporting every moved public
  name at the crate root (external users import from the root:
  `nmt_agent_hook_cli`, `app/src/main.rs`, `app/src/ipc.rs`,
  `app/src/workspace.rs`, `app/src/cli.rs`, `app/src/ui/shell.rs`,
  `app/src/ui/settings.rs`, `app/src/terminal/view.rs`,
  `app/src/agent_pane/`).
- Move the root `#[cfg(test)] mod tests` body to `src/tests.rs` in the same
  commit (`#[cfg(test)] mod tests;` in `lib.rs`).

Commit: `refactor(agent-utils): split crate root into topical modules`

### T3.11 -- `codex/app_server/` split

- `protocol.rs` (pure request builders and response parsers, no `Session`
  access): `codex_command_request`, `skills_list_request`, `codex_user_input`,
  `codex_command_response`, `delta_event`, `add_provider_config`,
  `thread_start_params`, `initial_thread_request`, `thread_resume_params`,
  `thread_list_params`, `parse_thread_settings`, `resumed_thread_events`,
  `parse_models`, `parse_thread_summaries`, `parse_replay`,
  `user_input_text`, `parse_item`, `tool_output`, `stringify_command`,
  `file_change_paths`, `file_change_diff`, `tool_title`,
  `parse_context_window_usage`, `parse_token_usage_breakdown`.
- `compaction.rs`: `ActiveCompaction`, `CompactionState` + impl,
  `is_legacy_compaction_notification`, `compaction_started`,
  `compaction_completed`.
- `skills.rs`: `SkillRefreshState` + impl, `skill_catalog_from_response`,
  `parse_skill_catalog`.
- `options.rs`: `APPROVAL_OPTIONS`, `SANDBOX_OPTIONS`, `EFFORT_OPTIONS`
  (re-export `pub use` from the parent -- the app imports them).
- `ThreadProfile` is read by both `spawn` (parent) and the builders -- keep
  it in the parent, mark `pub(super)` if the compiler asks.
- Parent keeps `Session`, its impl, the RPC-id consts, and the private
  `TurnOutputUsage` struct + impl (turn-scoped output-token accumulation
  used by `process_notification`; its two unit tests move with the test
  module in T1.7 and keep seeing it as a parent-private item).

Commit: `refactor(agent-utils): split codex app server protocol helpers`

### T3.12 -- `claude_code/stream_json/` split

- `parse.rs` (pure): `parse_claude_usage`, `update_claude_output`,
  `context_window_usage` (the free fn), `compaction_progress`,
  `claude_context_window`, `slash_command_text`, `ui_owns_slash_command`,
  `claude_result_error`, `initialize_command_catalog`,
  `legacy_command_catalog`, `parse_slash_commands`, `approval_description`,
  `parse_models`.
- `launch.rs`: `ANTHROPIC_MODEL_ENV`, `FILE_CHECKPOINTING_ENV`,
  `launch_model`, `initial_ready_model`, `enable_file_checkpointing`,
  `file_rewind_request`, `configured_permission_mode`.
- `control.rs`: `PendingControlOperation`, `PendingApproval`,
  `resolve_pending_control_operation`, `fail_pending_control_operations`.
- Parent keeps `Session`, its impl (including all `process_*` message
  handlers -- the streaming assembly methods share too much state to move),
  `PERMISSION_OPTIONS` / `INIT_REQUEST_ID`, and the private
  `TurnOutputUsage` struct + impl (turn-scoped output-token accumulation
  read and reset by the streaming handlers).

Commit: `refactor(agent-utils): split claude stream json support modules`

### T3.13 -- `claude_code/sessions/` split

- `paths.rs`: `munge_cwd`, `project_dir`, `session_path`.
- `index.rs`: `TRANSCRIPT_ENTRY_TYPES`, `TranscriptIndex` + impl,
  `is_transcript_entry`, `active_chain_indices`. `TranscriptIndex` fields
  are read by `fork.rs` and the parent -- mark fields `pub(super)`.
- `titles.rs`: `TITLE_SCAN_BYTES`, `head_title`, `user_prompt_text`,
  `is_compaction_summary`, `compaction_summary_text`, `record_text`,
  `clean_prompt`, `title_line`, `count_sessions`, `list_sessions`.
  Note `user_prompt_text` is also called from the fork path -- `pub(super)`.
- `replay.rs`: `parse_replay`, `replayed_compaction_id`,
  `complete_replayed_tools`, `load_replay`, `load_checkpoints`.
- `fork.rs`: `ClaudeFork`, `fork_session_before`, `build_fork_records`,
  `write_fork_file`.
- Parent keeps `FileRestoreAvailability`, `ClaudeCheckpoint`, and `pub use`
  re-exports of the public API (`count_sessions`, `list_sessions`,
  `load_replay`, `load_checkpoints`, `fork_session_before`, types).

Commit: `refactor(agent-utils): split claude session transcript modules`

### T3.14 -- `app/src/terminal/frame/` split

- `images.rs`: `FrameImage` + impl, `FrameImageKind`, `ZLayer`,
  `empty_images`, `extract_frame_images`, `image_pixel_size`,
  `extract_ordinary_images`, `normalized_source_rect`,
  `extract_virtual_images`, `push_virtual_run`.
- `line.rs`: `TerminalLine` + impl, `TerminalLineData`, `TerminalCell`,
  `StyleRun`, `LineBuilder` + impl, `line_from_parts`, `hash_line`,
  `display_char`.
- `colors.rs`: `BackgroundColors` + impl, `theme_default_foreground`,
  `theme_default_background`, `theme_selection_background`,
  `cell_is_selected`. Note `theme_default_background` is imported by
  `agent_pane/view.rs` as `crate::terminal::frame::theme_default_background`
  -- re-export it from the parent.
- `extract.rs`: `TerminalLineState`, the `TerminalFrame` impl block holding
  `from_render_buffer_reusing` and the test-gated ctors,
  `extract_row_with_colors`, `extract_row` (test-gated), `frame_cursor`,
  `cursor_for_row`.
- `cache.rs`: `TerminalFrameCache` + impl, `GenerationMap`.
- Parent keeps `TerminalFrame` struct, `TerminalCursor`, accessors, and
  re-exports.

Commit: `refactor(app): split terminal frame into topical modules`

### T3.15 -- `app/src/terminal/session/` split

- `config.rs`: `TerminalSessionConfig` + all three impls, `default_shell`,
  `shell_is_powershell`, `encode_powershell_command`,
  `ENCODED_POWERSHELL_INTEGRATION`.
- `proxy.rs`: `TerminalEventProxy` + its impl + the `EventListener` impl.
  Allowed structural glue: add `TerminalEventProxy::new(...)` taking the ten
  field values, and replace the struct-literal construction in
  `new_internal` with a call to it (a parent cannot see a child's private
  fields).
- Parent keeps the aliases, `HostEvent`, `InFlightBlock`, `TerminalSession`
  + impls, `Drop`, `engine_init_error`, and re-exports
  (`pub use crate::terminal::session::config::TerminalSessionConfig;` and
  `pub use crate::terminal::session::proxy::TerminalEventProxy;`).

Commit: `refactor(app): split terminal session config and event proxy`

### T3.16 -- `app/src/terminal/surface/` split

Convert `surface.rs` to `surface/mod.rs` first (section 2.2), then add new
`impl TerminalSurface` blocks per child file:

- `selection.rs`: `apply_screen_selection`, `frozen_selection_range`,
  `selection_range`, `selection_range_at`, `clear_selection`,
  `apply_selection_at`, `begin_selection`, `update_selection`,
  `finish_selection`, `selection_text`, `copy_selection`, plus free fns
  `selection_screen_range`, `block_selection_range`.
- `mouse.rs`: `SurfaceCell`, `SurfaceScreenCell`, `SurfaceCellSide`,
  `SurfaceMouseButton`, `SurfaceMouseEventKind`, `apply_mouse`, `modes`,
  `mouse_mode`, `app_mouse_mode`, `report_mouse`, `mouse_button_code`,
  `mouse_motion_code`, `mouse_report_mods`.
- `input.rs`: `TerminalKeyAction`, `TerminalKeyResult`, `write_text`,
  `write_bytes`, `apply_key_action`, `paste`, `paste_text`, `paste_payload`.
- `reads.rs`: `block_store`, `engine_blocks`, `acquire_block`,
  `viewport_top_screen_row`, `pointer_screen_row`, `pointer_block_row`,
  `frozen_image`, `insert_frozen_image`, `frozen_image_generation`,
  `has_live_images`, `drain_released_images`, `live_history_lines`,
  `PointerRow`, `push_pointer_cell`, `pointer_row`.
- `scroll.rs`: `apply_scroll`, `scroll_lines`,
  `scroll_viewport_bottom_before_input`,
  `should_scroll_to_bottom_before_input`.
- Parent keeps the struct, ctors, session forwarders, `frame`,
  `with_render_buffer`, resize, flags, `viewport_top`, `screen_pos`,
  `tab_state_with_cwd`, `normalize_osc7_pwd`, and the test module (move it
  to `surface/tests.rs` in this same task). `write_bytes`, `viewport_top`,
  and `screen_pos` are called across child files -- `pub(super)` as the
  compiler demands.

Commit: `refactor(app): split terminal surface into interaction modules`

## 7. Phase 4 -- split the remaining giants

These tasks are larger; do each in a single session, running
`cargo check -p app` after every sub-move.

### T4.1 -- `ui/settings.rs`: extract the three inline pages

Inside `settings_view`, the Terminal, Appearance, and System pages are built
inline while Profiles/Agent/Remote/About already have their own `fn ... ->
SettingPage` functions. Extract the three inline `.page(SettingPage::new(...))`
arguments into `fn terminal_page(...)`, `fn appearance_page(...)`,
`fn system_page(...)` in the same file, passing the same precomputed locals
the existing extracted pages receive as parameters. No file split yet.

Commit: `refactor(settings): extract inline settings pages into functions`

### T4.2 -- `ui/settings/state.rs`, `opacity.rs`, `theme.rs`

Convert `settings.rs` to `settings/mod.rs` first (section 2.2).

- `state.rs`: the `DEFAULT_*` consts, `initial_font_family`,
  `input_style_label`, `input_style_from_value`, `cursor_shape_from_value`,
  `builtin_profile`, `agent_kind_label`, `builtin_agent_profile`,
  `builtin_agent_profiles`, `AppSettings` struct + `Default` + `Global` +
  the full `impl AppSettings` (load, profile CRUD, `appearance_config`,
  `save`), and all nine `clamp_*` / `*_or_default` validators.
- `opacity.rs`: `effective_background_opacity`,
  `effective_surface_background_opacity`, `surface_background_opacity`,
  `effective_main_view_background_opacity`, `main_view_is_transparent`,
  `main_view_background_opacity`,
  `effective_background_image_layer_opacity`,
  `background_image_layer_opacity`, `window_background_appearance_for`,
  `window_background_appearance`.
- `theme.rs`: `apply_ui_theme`, `apply_ui_constants`, `select_theme`,
  `load_theme_choices`, `reload_themes`, `watch_themes`, `preview_color`,
  `theme_preview`, `theme_list`, `tab_background_opacity`,
  `apply_window_translucency`.
- Parent re-exports every previously `pub` / `pub(crate)` name
  (`AppSettings`, `AgentProfile`, the `pub use nmt_config...` lines,
  `apply_ui_theme`, `watch_themes`, opacity fns, ...) so `ui/mod.rs`'s
  re-export list and all callers compile unchanged.
- Move the test module to `settings/tests.rs` in this task; it exercises
  items from all three new files through its module glob (rewritten to
  `use crate::ui::settings::*;` per section 2.2), which keeps working via
  the parent's re-imports.

Commit: `refactor(settings): split state, opacity, and theme modules`

### T4.3 -- `ui/settings/card.rs` + `ui/settings/fields.rs`

- `card.rs`: `CardInputState`, `card_text_input`, `card_row`.
- `fields.rs`: `OpacityTarget` + impl, `OpacitySliderState` + `Global` impl,
  `opacity_slider_field`, `background_opacity_field`,
  `background_image_opacity_field`, `background_image_field`.

Commit: `refactor(settings): extract card and field widget helpers`

### T4.4 -- `ui/settings/` page files + dialog

- `pages.rs` (or one file per page if any single page file would exceed ~400
  lines): `terminal_page`, `appearance_page`, `system_page`,
  `profiles_page`, `terminal_profiles_group`, `terminal_profile_card`,
  `agent_profiles_group`, `agent_page`, `agent_hook_item`,
  `agent_update_check_item`, `agent_update_status_item`,
  `installation_update_title`, `installation_version_text`,
  `remote_session_page`, `remote_host_status`, `remote_client_status`,
  `reconcile_remote_host` (keep both `#[cfg]` arms together), `about_page`.
- `agent_profile_dialog.rs`: `AgentProfileDraft` + `Global` impl,
  `open_agent_profile_dialog`, `save_agent_profile_draft`,
  `kind_choice_button`, `agent_profile_dialog_content`.
- Parent keeps `settings_view` (now a thin page assembler) and re-exports.
  `reconcile_remote_host` is called from `shell.rs` as
  `ui::settings::reconcile_remote_host` -- re-export it.

Commit: `refactor(settings): split settings pages and profile dialog`

### T4.5 -- `ui/shell/tab_surface.rs` + `ui/shell/actions.rs`

Convert `shell.rs` to `shell/mod.rs` first (section 2.2).

- `tab_surface.rs`: `TerminalPaneTree` alias, `TabSurface` + its impl.
  `persistence.rs`, `tab_bar.rs`, and `workspace.rs` import it via
  `super::shell::TabSurface` / `crate::ui` -- parent re-export
  (`pub(crate) use crate::ui::shell::tab_surface::{TabSurface,
  TerminalPaneTree};`).
- `actions.rs`: the 21-action `actions!` block. `ui/mod.rs` re-exports the
  action types and `main.rs` binds them -- parent re-export keeps paths.

Commit: `refactor(shell): extract tab surface model and action definitions`

### T4.6 -- `ui/shell/agent_notifications.rs` + `ui/shell/updates_layer.rs`

- `agent_notifications.rs`: `AgentRouteLocation`, `AgentRouteTarget`, and an
  `impl Shell` block with `register_agent_pane`, `register_agent_tab`,
  `remove_agent_route`, `remove_native_notifications`,
  `exact_window_active`, `acknowledge_notification`,
  `process_native_notifications`, `agent_routes_in_surface`,
  `owns_agent_route`, `locate_agent_route`, `focus_notification`,
  `apply_agent_event`, `reschedule_agent_timer`, `watch_agent_tab`.
- `updates_layer.rs`: `ClaudeUpdateIcon`, `CodexUpdateIcon`,
  `AgentUpdateNotification`, and an `impl Shell` block with
  `update_notification_card`, `render_update_notification_layer`,
  `ensure_update_notification_timer`.
- Child modules read `Shell`'s private fields directly; no field changes.
  Methods called from outside `ui::shell` (`focus_notification`,
  `apply_agent_event` from `main.rs`) keep their `pub(crate)` markers.

Commit: `refactor(shell): split agent notification and update layer code`

### T4.7 -- `ui/shell/close.rs` + `ui/shell/workspaces.rs`

- `close.rs`: `should_confirm_tab_close`, `should_confirm_close`,
  `on_close_tab`, `request_close_pane`, `close_pane_now`,
  `processes_running`, `workspace_process_count`, `open_close_confirm`,
  `close_process_count`, `request_close_tab`, `close_tab_now`,
  `request_close_workspace`, `confirm_close_last_workspace`,
  `doom_workspace`, `replace_last_workspace`, `confirm_window_close`,
  `close_workspace_now`. Move the file's test module to `shell/tests.rs`
  in this task (it covers the two pure predicates plus
  `TabSurface::agent_kind`).
- `workspaces.rs`: `rename_input`, `start_workspace_rename`,
  `finish_workspace_rename`, `start_tab_rename`, `finish_tab_rename`,
  `set_workspace_pinned`, `reorder_workspaces`, `on_new_workspace`,
  `create_workspace`, `on_next_workspace`, `on_prev_workspace`,
  `on_next_tab`, `on_prev_tab`.
- Methods already `pub(super)` for `tab_bar.rs` / `workspace_sidebar.rs`
  keep those markers.

Commit: `refactor(shell): split close cascade and workspace management`

### T4.8 -- `ui/shell/tabs_open.rs` + `ui/shell/pump.rs` + `ui/shell/panes.rs`

- `tabs_open.rs`: `default_profile`, `on_new_window`, `on_new_tab`,
  `open_profile_tab`, `on_new_remote_tab` (windows-gated), `open_agent_tab`,
  `on_new_agent_tab`, `open_dir_tab`.
- `pump.rs`: `tab_for_pane`, `pump_pane`, `watch_pane`.
- `panes.rs`: `PANE_RESIZE_STEP`, the four `on_split_*`, `split_pane`, the
  four `on_resize_pane_*`, `resize_pane`, `focus_pane`,
  `apply_pending_ratios`, `render_active_tree`, `render_pane_node`.

Commit: `refactor(shell): split tab creation, event pump, and pane layout`

### T4.9 -- `ui/shell/render.rs`

- First, inside `impl Render for Shell::render`, extract two helpers in the
  parent: `fn render_title_bar(&mut self, ...) -> impl IntoElement` (the
  TitleBar construction block) and a helper for the `.on_action(...)` chain.
- Then move `impl Render for Shell`, `impl Focusable for Shell`,
  `SideBarIcon`, `GitIcon`, and the two new helpers into `render.rs`.
- `shell/mod.rs` keeps: `Shell` struct, `Drop`, `new`, `alloc_id`,
  `sync_git_target`, `on_toggle_git_sidebar`, `on_toggle_sidebar`,
  `on_show_settings`, the active-surface accessor group (`active_pane`,
  `try_active_pane`, `active_agent`, `active_agent_route`,
  `ensure_active_tab_live`, `sync_active_terminal_title`, `focus_active`,
  `acknowledge_visible`, `projected_workspace_summaries`,
  `tab_agent_indicators`, `active_tab_title`), and `explicit_cwd`.

Commit: `refactor(shell): extract root render implementation`

### T4.10 -- `app/src/terminal/view/` split

- Convert `view.rs` to `view/mod.rs` first (section 2.2).
- First hoist the block-list branch of `TerminalPane::render` (the ~150-line
  chunk that builds specs and reconciles the gpui list) into
  `fn render_block_list_content(...)` in `mod.rs`. Commit-internal step.
- Then create child files, each with an `impl TerminalPane` block:
  - `input.rs`: `on_key_down`, `feed_terminal_key`, `on_send_tab`,
    `on_send_shift_tab`, `on_file_drop`, the whole
    `impl EntityInputHandler for TerminalPane`, `should_scroll_to_latest`,
    `dropped_paths_text`, `show_text_copied`, `TextCopiedNotification`.
  - `mouse.rs`: `on_mouse_down`, `on_mouse_up`, `on_mouse_move`,
    `on_scroll_wheel`, `apply_mouse_event`, free fns
    `selection_drag_started`, `selection_type_for_click_count`,
    `terminal_cell_at_position`, `surface_mouse_button`,
    `terminal_scroll_lines`, `WHEEL_LINES_PER_STEP`, `block_gutter_hit`,
    `BLOCK_GUTTER_SELECTION_ENABLED`.
  - `scroll.rs`: `mark_scroll_activity`, `scrollbar_fraction`,
    `scroll_to_latest`, `scroll_thumb_to`.
  - `blocks.rs`: `block_list_mode`, `block_chrome_enabled`, `content_cols`,
    `block_list_total_px`, `live_history_rows`, `list_offset_for_px`,
    `begin_block_list_frame`, `record_frozen_view`, `record_frozen_chrome`,
    `block_list_point_at`, `try_select_frozen_item`, `jump_to_frozen_item`,
    `selected_frozen_command`, `selected_frozen_output`,
    `expanded_frozen_selection`, `frozen_selection_to_text`,
    `format_block_range`, `on_copy_block_command`, `on_copy_block_output`,
    `on_rerun_block`, `on_previous_block`, `on_next_block`,
    `render_block_list_content`.
  - `events.rs`: `drain_host_events`, `refresh_blocks`,
    `terminal_surface_for_tab`.
- Parent keeps: struct, `actions!`, ctor group (`spawn`, `spawn_remote`,
  `from_surface`), accessors, metrics/bounds/frame-cache group,
  `current_row_offsets`, `impl Render`, `impl Focusable`, event emitter
  impls. Move the test module to `view/tests.rs` (its six tests cover free
  fns now living in `mouse.rs`/`input.rs`; the glob rewritten to
  `use crate::terminal::view::*;` plus parent re-imports keeps them
  compiling).

Commit: `refactor(app): split terminal pane into interaction modules`

### T4.11 -- `app/src/terminal/terminal_view/` split

After T2.1-T2.3 the file holds the three elements plus image/cursor/frame
painting. Convert `terminal_view.rs` to `terminal_view/mod.rs` first
(section 2.2).

- `item.rs`: `SharedBlockStore`, `BlockListItem`, `BlockListItemPrepaint`,
  its `IntoElement` / `Element` impls, and the `impl BlockListItem` block.
- `paint.rs`: `shape_frame`, `paint_frame`, `paint_frame_images`,
  `paint_frozen_images`, `paint_generation`, `paint_image_clipped`,
  `paint_cursor`, `cursor_bounds`, and the remaining test
  (`cursor_bounds_cover_block_beam_and_underline`) moved to
  `terminal_view/tests.rs`.
- Parent keeps `TerminalView` and `BlockListView` with their impls.

Commit: `refactor(app): split terminal element painting helpers`

### T4.12 -- `app/src/terminal/block_list/` split

- `geometry.rs`: `ITEM_PAD_ROWS`, `item_rows`, `item_px`, `live_item_px`,
  `visible_rows`, `nav_item_top`, `block_pad_rows`, `block_list_alignment`,
  `block_list_active_top_px`.
- `reconcile.rs`: `BlockListState` + impl, `BlockListMeasureKey`,
  `BlockListRenderMetrics`, `block_list_render_metrics`,
  `shift_selected_item_for_eviction`, `ListReconcile`,
  `plan_list_reconcile`, `RemeasureScope`, `plan_remeasure`.
- `chrome.rs`: `FrozenItemChrome`, `item_accent`, `item_header`,
  `command_header`, `live_chrome`, `format_duration`,
  `block_list_live_chrome`, `offset_frozen_chrome`, `paint_frozen_chrome`,
  `paint_frozen_separators`.
- `rows.rs`: `EngineRowBuilder` + impl, `HandleItemInfo`,
  `handle_item_info`, `frozen_block_view`, `live_history_view`,
  `block_row_shape_key`.
- `images.rs`: `FrozenImage`, `frozen_block_images`.
- `selection.rs`: `FrozenPoint`, `BlockListPoint`, `FrozenHitInfo` + impl,
  `selected_span`, `expand_wide_span`, `FrozenSelectionPiece`,
  `frozen_selection_pieces`.
- `paint.rs`: `shape_frozen_rows`, `paint_frozen`.
- Parent keeps the shared hub types `FrozenRow` and `FrozenView`, plus
  re-exports for `surface.rs` (`EngineRowBuilder`) and `terminal_view/`
  (13 symbols) and `view/`.

Commit: `refactor(app): split block list into topical modules`

### T4.13 -- `agent_pane/transcript/` split

Re-verify item names first; this file changes often. Convert `transcript.rs`
to `transcript/mod.rs` first (section 2.2). Children keep the
`use crate::agent_pane::*;` glob at the top where they need it.

- `format.rs` (pure helpers): `should_show_jump_to_latest`, `working_label`,
  `working_status_label`, `worked_status_label`, `interrupted_status_label`,
  `timed_token_label`,
  `is_work_row`, `hidden`, `truncated_user_prompt`, `fenced_code_block_as`,
  `detect_output_language`, `strip_read_gutter`, `file_extension_lang`,
  `command_execution_detail`, `COMMAND_EXECUTION_HEADING`,
  `entry_copy_text`, `compaction_label`, `compaction_row_is_expandable`,
  `compaction_accounting`, `relative_time`, `compact_token_count`,
  `permission_icon`. Move the three small test modules
  (`prompt_truncation_tests`, `read_gutter_tests`, `fence_tests`) to
  `transcript/tests.rs` in the same task.
- `virtual_code.rs`: the `VIRTUAL_TRANSCRIPT_*` consts,
  `should_virtualize_transcript`, `TranscriptSourceKey`,
  `transcript_source_key`, `transcript_segments`, `VirtualTranscriptState`
  + impl, `normalized_virtual_transcript`, `code_transcript_format`.
- `disclosure_row.rs`: the `AgentDisclosureRow` widget, its layout consts,
  and its builder impl. It is used by `view.rs` and other siblings -- widen
  to `pub(in crate::agent_pane)` or re-export from the parent.
- `rows.rs`: `Entry`, `RowSpec`, `TurnSummary`, `turn_summary`,
  `entry_fingerprint`, and the spec-building `impl AgentPane` methods
  (`entry_spec`, `work_spec`, `build_row_specs`, `turn_specs`,
  `stream_specs`, `sync_transcript_list`, `push`, the scroll queries).
- `render.rs`: the row-rendering `impl AgentPane` methods (`render_row`
  through `render_run_toggle`).
- Items other `agent_pane` files reference through the parent keep working
  via parent re-imports; check `view.rs` and `session.rs` compile.

Commit: `refactor(agent-pane): split transcript into model and render modules`

### T4.14 -- `agent_pane/composer/` split

Convert `composer.rs` to `composer/mod.rs` first (section 2.2).

- `rewind.rs`: `RewindAction`, `RewindState` + impl, `FileRestoreNext`,
  `file_restore_next`, `rewind_blocks_submission`, `rewind_prompt_label`,
  `rewind_timestamp`, and the rewind/fork `impl AgentPane` methods
  (`open_rewind`, `cancel_rewind_picker`, `rewind_palette_model`,
  `activate_rewind_action`, `start_file_restore`,
  `start_conversation_fork`, `replace_with_conversation_fork`). Move the
  `rewind_state_tests` module to `composer/tests.rs`.
- `palette.rs`: `PaletteAction`, `PaletteRow`, `PaletteModel`,
  `PaletteControl`, and the palette `impl AgentPane` methods
  (`palette_model`, `handle_palette_control`, `dismiss_command_palette`,
  `handle_recent_sessions_control`, `activate_palette_index`,
  `render_command_palette`, `open_recent_sessions`).
- Parent keeps the send/submit and slash-command execution methods
  (`send_user_message` through `command_choices`, `show_status`,
  `set_command_feedback`), `PendingSlashCommand`, `CommandFeedbackKind`,
  `CommandFeedback`, `ComposerAction`, `composer_action`,
  `restored_input_after_interruption`.

Commit: `refactor(agent-pane): split composer rewind and palette modules`

### T4.15 -- `agent_pane/session/` split

Convert `session.rs` to `session/mod.rs` first (section 2.2).

- `backend.rs`: the `Backend` enum + its dispatch impl, `RecoveryIdentity`.
- `update_recovery.rs`: `RecoverySnapshot`, `RecoveryReadiness`,
  `RestorationReadiness`, `UpdateSuspension`, and the provider-update
  `impl AgentPane` methods (`installation_key` through
  `start_new_after_update_failure`).
- `events.rs`: `apply_event` (keep the whole method in one piece),
  `apply_replay`, `start_item`, `complete_item`, `append_delta`,
  `publish_queued_user_messages`.
- Parent keeps `Status`, construction/lifecycle (`new`, `start_session*`,
  `resume_session`, `reset_conversation`, `refresh_git_branch`,
  `emit_lifecycle`, `latest_agent_message`, `agent_route`), turn control
  (`send_text` family, `interrupt*`, `respond_approval`, `start_working`,
  `finish_working`, `note_visible_agent_output`), and the small helpers
  (`defaults_key`, `profile_model`, `remember_thread_defaults`).

Commit: `refactor(agent-pane): split session backend, recovery, and events`

### T4.16 -- `agent_pane/view/` + `agent_pane/updates/` split

Convert `view.rs` to `view/mod.rs` and `updates.rs` to `updates/mod.rs`
first (section 2.2).

`view.rs` children:

- `settings_row.rs`: `render_settings_row`, `render_claude_settings_row`,
  `render_codex_settings_row`, `settings_group`, `setting_picker`.
- `history.rs`: `render_history`, `render_history_row`,
  `queued_message_label` + its test (to `view/tests.rs`).
- `banners.rs`: extract from `render` two helpers
  (`render_approval_panel`, `render_update_banner`) in the parent first,
  then move them here together with `render_composer_status`.
- Parent keeps `StopResponseIcon`, the trait impls, and the slimmed
  `Render::render` (its mutating prologue must stay inside `render`).

`updates.rs` children (this file has no glob import; it is self-contained):

- `notification.rs`: `UpdateNotificationTone`, `NotificationPrimaryAction`,
  `NotificationProgress`, `FocusedVisibleLifetime` + impl,
  `UpdateNotificationView`, `notification_view`, `progress_view`,
  `bounded_error`.
- `transaction.rs`: `UpdateMode` + impl, `PreflightResolution`,
  `resolve_preflight`, `combine_transaction_error`, `request_update`,
  `matching_panes`, `affected_installation_indices`, `start_transaction`,
  `finish_preflight_failure`, `restore_tabs`, `provider_for_profile`.
- `doubles.rs`: `UnavailableMaintenance`, `FakeMaintenance` + impls.
- Parent keeps `AgentUpdates` + registry fns (`initialize`,
  `update_cache_path`, `reconcile_profiles`, `distinct_installation_keys`,
  `installations_for_profiles`, `installation`, `manual_check_profiles`,
  `schedule_startup_checks`). Move the test module to `updates/tests.rs`.
  `notification_view` and `request_update` are `pub(crate)` and used from
  `ui/` -- re-export from the parent.

Commit: `refactor(agent-pane): split pane view and update orchestration`

## 8. Code that must stay intact

Do not split, regardless of size. These are cohesive state machines or hot
paths whose pieces share most of their fields; splitting them would only add
accessors and hide the control flow:

- `PromptSniffer` and its impl (`prompt_sniffer.rs`) -- only the two pure
  helper groups in T3.9 leave.
- `PtyPipe::process_pty_chunk`, `flush_engine_state`, `drain_recv_channel`,
  `pty_read`, `pty_write`, `run_event_loop` -- all stay in `pty_pipe.rs`.
- `AgentMonitor` and its impl -- moves whole into `monitor.rs` (T3.10),
  never split internally.
- `AgentPane::apply_event` -- moves whole (T4.15), never split internally.
- `Shell::new` and the `Shell` struct -- stay together in `shell.rs`.
- `GhosttyTerminal`'s struct and `Drop` impl -- stay together in
  `ghostty.rs`.
- The streaming assembly methods of `claude_code/stream_json.rs`
  (`process_stream_event`, `process_assistant`, `process_tool_results`,
  `process_result`, and their shared open-block state) -- stay in the
  parent.

## 9. Final acceptance

After all tasks:

1. `cargo test --workspace` green; test counts match the baseline.
2. No production file over ~800 lines except `pty_pipe.rs` (the retained
   hot path justifies up to ~950). Check with:

   ```bash
   git ls-files '*.rs' | grep -v third_party | grep -v tests | xargs wc -l | sort -rn | head -20
   ```

3. `grep -rn "pty_pipe" crates/terminal/src/ghostty/` returns
   nothing (cycle broken), and `block_list.rs` / `terminal_view.rs` /
   `view.rs` reference each other only through `theme`, `paint_text`,
   `layout`, or parent re-exports.
4. Every external import path that existed at the baseline still compiles
   (guaranteed by re-exports; verified by the workspace build).
