# `crates/` Optimization Review

Static review of every first-party crate under `crates/` (126,159 lines of Rust
across 16 crates; ~26% of that is test code). Items are grouped by the five
angles of the tidy pass — efficiency, altitude, reuse, quality, and test
discipline — and ordered by payoff, not by angle.

Each item names the file and line it lived at when the review was written, what
the code cost there, and the change that removes the cost. The Status section
below records what was applied and what was dropped; line numbers throughout
refer to the tree as it stood before that work.

**Scope note.** The working tree is clean, so this is a whole-tree read rather
than a diff review. Items were verified by reading the code; none were measured
with a profiler. Where an item claims a per-frame cost, the call path from
`impl Render` down to the allocation is stated so it can be checked.

## Status

Every item below has been applied, except where this section says otherwise.
The work landed as thirteen commits on `dev`; each names the behavior it was
checked against.

Three items did not survive contact with the code and were dropped rather than
forced:

- **Item 16** (a detached `HEAD` costing a second `git` call) has no correct
  single-command form: `rev-parse` resolves one revision per invocation, so
  nothing reports both the symbolic name and the short commit at once. The
  shared read from item 4 stops each watcher paying for it separately, which is
  the part that was worth fixing.
- **Item 20** (`crates/web_client/` lacking a README) was a misread of a
  filesystem listing. The directory is untracked local build output --
  `dist/` is gitignored and nothing under it is committed -- so a fresh clone
  has no such directory and there is nothing for a reader to be confused by.
- **The `unix` tree** (item 10's `unix/notifier.rs` entry, item 12's
  `platform/src/unix/mod.rs` row, and the test gap named under Test discipline)
  was left alone at the repository owner's direction.

Item 12 is applied for every file the table below marks splittable or
borderline. The four marked as covered by the state-machine exception were left
whole, and `app_agent/src/session/mod.rs` came down from 1801 lines to 1064,
where what remains is the pane's own lifecycle.

For the record, the largest remaining production files are
`deepseek/session.rs` (1533), `claude_code/stream_json/mod.rs` (1453),
`codex/app_server/mod.rs` (1133), `terminal/src/pty_pipe/mod.rs` (1120), and
`app_agent/src/session/mod.rs` (1064) -- each a protocol state machine or a hot
loop, which is the case the guideline exempts.

## Crate sizes

| Crate | Lines | Files |
| --- | ---: | ---: |
| `agent_utils` | 33,603 | 96 |
| `app` | 27,273 | 105 |
| `app_agent` | 21,407 | 50 |
| `terminal` | 14,047 | 49 |
| `app_terminal` | 12,426 | 63 |
| `platform` | 6,509 | 41 |
| `config` | 4,853 | 26 |
| `remote_net` | 3,023 | 20 |
| `input` | 1,309 | 4 |
| `remote_session_hub` | 594 | 3 |
| `tree_sitter_bundle` | 526 | 3 |
| `version` | 324 | 3 |
| `agent_hook_cli` | 106 | 3 |
| `i18n` | 95 | 2 |
| `shell_extension` | 64 | 2 |

---

## P0 — Per-frame work in the agent pane

### 1. The composer document is materialized into a `String` on every frame

`crates/app_agent/src/composer/palette.rs:132`

`AgentPane::render` calls `render_command_palette` unconditionally
(`crates/app_agent/src/view/mod.rs:83`), which calls `palette_model`, whose
third statement is:

```rust
let input = self.input.read(cx);
let text = input.text().to_string();
```

`TextInput::text()` returns `&Rope`
(`third_party/gpui-component/ui/src/input/state.rs:1231`). `to_string()` walks
every chunk and copies the entire composer document into a fresh heap `String` —
on every frame, whether or not the palette is open, and whether or not the text
begins with `/`. A pane with an animating working indicator (`CYCLE_DURATION` is
1100 ms, `crates/app_agent/src/transcript/working_indicator/mod.rs:9`) repaints
continuously, so this runs at frame rate while an agent is answering, and the
cost grows with however much prose the user has typed.

Both consumers of `text` — `parse_skill_prefix` and `parse_slash_command` — only
inspect the leading token. The fix is to read a bounded prefix off the rope
instead of the whole document, and to bail before that when the first character
can start neither a `/` command nor a `$` skill reference.

### 2. The whole slash-command catalog is rebuilt and deep-cloned per frame

`crates/app_agent/src/composer/palette.rs:142` → `crates/app_agent/src/composer/mod.rs:641`

Once the composer text does start with `/`, `palette_model` calls
`self.command_catalog()`, which is not cached anywhere:

- `local_commands()` (`crates/app_agent/src/commands/mod.rs:44`) constructs a
  fresh `Vec<SlashCommandInfo>` of ~20 entries, each built from `i18n(...)`
  results via `String` allocation.
- `Backend::adapter_commands` adds the harness's own list.
- `self.palette.provider_commands.clone()` deep-clones the provider list.
- `merge_catalog` (`:156`) then allocates a `HashSet<String>` and pushes a
  `command.name.clone()` into it for every entry.

`filter_palette_catalog` (`:216`) then clones every *matching*
`SlashCommandInfo` and `SkillInfo` a second time into rank buckets, and the
`.map(...)` at `palette.rs:211` allocates a third time per row:
`format!("/{}", command.name)`, `command.description.clone()`,
`command.argument_hint.clone()`, plus an `i18n(...).to_string()` for any
disabled reason.

For a ~30-command catalog that is on the order of 300 heap allocations per frame
while the user types a slash command.

Cache the merged catalog as an `Rc<[SlashCommandInfo]>` on `self.palette`,
invalidated when `provider_commands` or the backend changes, and have
`filter_palette_catalog` return borrowed entries or indices rather than clones.
`PaletteRow`'s `String` fields can become `SharedString` at the same time, which
makes the row-level clones refcount bumps.

### 3. `PaletteRow`/`PaletteModel` string fields are `String` where the value is a catalog constant

`crates/app_agent/src/composer/rewind.rs` (36 sites), `palette.rs` (14),
`fork.rs` (11) — 146 `i18n(...).to_string()` calls across the tree.

`i18n` already returns a borrow that outlives the caller for literal keys (the
catalogs live in a `OnceLock` and are never dropped —
`crates/i18n/src/lib.rs:19`), and every call site passes a `&'static str` key.
The allocation exists only because the receiving struct field is typed `String`.

Two changes make the whole class free:

1. Declare the signature as `pub fn i18n(key: &'static str) -> &'static str`.
   All 948 call sites already pass literals or `&'static str` variables; the six
   non-literal sites were checked (`transcript/format.rs:120`,
   `context_usage/mod.rs:447`, `commands/mod.rs:119`,
   `composer/response_annotations.rs:68`, `composer/mod.rs:438`) and all pass
   static keys.
2. Type UI model fields that hold catalog text as `SharedString`.
   `From<&'static str>` for `SharedString` does not allocate.

`rewind_palette_model` is on the same per-frame path as items 1 and 2, so its 36
allocations recur at frame rate whenever the rewind picker is open.

### 4. Each agent pane runs its own `git` subprocess poll

`crates/app_agent/src/session/mod.rs:335-358` starts a per-pane loop that calls
`refresh_git_branch` (`:506`) on `AgentSettings::git_status_refresh_interval`.
That reaches `nmt_agent_utils::git::current_branch`
(`crates/agent_utils/src/git.rs:43`), which spawns `git branch --show-current`,
and on a detached `HEAD` spawns a second process for `git rev-parse`.

`crates/app/src/ui/git_status/mod.rs:329` runs a second, independent poll on the
same setting for the title-bar status.

So *N* agent panes pointed at one repository plus the title bar produce *N+1*
process spawns per interval against the same directory, all returning the same
answer. Process creation on Windows is in the millisecond range, and each one
also inherits and tears down handles.

Give the query a process-global cache keyed by directory, with the poll owned by
one place and panes subscribing to it. `agent_utils::git`'s own module docs
already state that the branch query lives there because both the pane and the
git chrome ask it — the caching belongs at that same level.

---

## P1 — Altitude

### 5. `dispatch_cli_action` matches the same value twice and needs three `unreachable!()`

`crates/app/src/main.rs:545-638`

The function peels `FocusNotification` off with an `if let`, then runs a first
`match &action` to extract a path (with
`CliAction::FocusNotification { .. } => unreachable!("handled above")`), then a
second `match action` to act on it (with two more `unreachable!()`). Three
impossible arms is the tell that the control flow was split for a reason that no
longer holds.

A single `match action` with the path validation moved into the two arms that
need it removes all three, and removes the `path.clone()` at `:563` as well.

### 6. Backend capability decisions are expressed in two places

`crates/app_agent/src/session/backend.rs`

`crates/app_agent/src/capabilities.rs` is a well-built answer to "what can this
harness do": one `Capabilities` table per kind, deliberately without a `Default`
so a new harness must answer every question. Composer call sites gate on it
correctly (`composer/mod.rs:398-409`).

`backend.rs` then re-derives several of the same answers by matching on the
variant:

- `rewind_files` (`:328`) rejects Codex and DeepSeek — `caps().file_rewind`
  already says this, and `composer/mod.rs:398` already gates on it.
- `search_sessions` (`:306`) no-ops for Codex and Claude — `caps().session_search`.
- `request_history` (`:466`) no-ops for Claude and DeepSeek —
  `caps().filesystem_session_history`.
- `resume_thread` (`:452`) returns `false` for Claude.

The risk is drift: adding session search to a harness means editing the table
*and* remembering the `match` arm. Where the branch exists purely to reject, the
capability should be the single source and `backend.rs` should assume the caller
already gated. Where it must stay (the method genuinely needs the session
object), a comment pointing at the capability field keeps the two readable as one
decision.

### 7. Provider-key filtering repeats six times inside two methods

`crates/app_agent/src/session/backend.rs:355-410`

`load_background_task_transcript` and `interrupt_background_task` each contain a
`match self` whose every arm is a second `match key.provider` that discards keys
from other harnesses. Six nested matches expressing one rule: *a key only reaches
the backend that minted it.*

A `Backend::provider(&self) -> Option<BackgroundTaskProvider>` accessor turns
both methods into one guard at the top followed by a flat dispatch.

### 8. Duplicated palette-navigation branch, twice, each with an `unreachable!()`

`crates/app_agent/src/composer/palette.rs:319-323` and `:391-395`

Identical eight-line block in two functions:

```rust
PaletteControl::Previous | PaletteControl::Next => {
    let direction = match control {
        PaletteControl::Previous => PaletteDirection::Previous,
        PaletteControl::Next => PaletteDirection::Next,
        _ => unreachable!(),
    };
```

An `impl PaletteControl { fn direction(self) -> Option<PaletteDirection> }`
removes the duplication and both `unreachable!()`s, and lets each site match
`PaletteControl::Previous | PaletteControl::Next` without the inner re-match.

A third `unreachable!()` at `:224` has the same shape: a nested
`match self.runtime.status` inside an arm whose outer condition already narrowed
the status to `Starting | Exited`.

---

## P2 — Repository convention compliance

These are violations of rules stated in `CLAUDE.md`/`AGENTS.md`, listed so they
can be cleared in one mechanical pass.

### 9. `attachments.rs` sits next to `attachments/`

`crates/app_agent/src/composer/attachments.rs` (261 lines) coexists with
`crates/app_agent/src/composer/attachments/tests.rs`. The rule is explicit:
"Never keep `foo.rs` next to a `foo/` directory; when splitting an existing file,
`git mv foo.rs foo/mod.rs` first so file history stays traceable."

This is the only occurrence in the tree — a scripted check over every
`crates/**/*.rs` directory turned up no others.

### 10. 42 relative imports across 23 files

`use super::` / `use self::` are forbidden in new or edited code. Current
holdouts, by crate:

- `app_terminal`: `frame/extract.rs` (5), `surface/*` (7 across six files),
  `links/mod.rs` (2), `frame/colors.rs`, `frame/cache.rs`, `session/proxy.rs`,
  `scrollbar/mod.rs`, `vtebench_repro.rs`
- `agent_utils`: `claude_code/sessions/fork.rs` (4), `sessions/index.rs` (2),
  `claude_code/stream_json/mod.rs` (3), `stream_json/parse.rs`
- `app`: `ui/persistence.rs` (4)
- `platform`: `windows/spsc.rs`, `windows/shell_integration/mod.rs`,
  `unix/notifier.rs` (2)
- `app_agent`: `lib.rs` (2), `context_usage/mod.rs`

Mechanical to fix, and each file only needs touching when it is next edited for
another reason — the rule exempts files a change does not otherwise touch.

### 11. `use crate::*;` in 15 files of `app_agent`

`profile.rs`, `workflows.rs`, `capabilities.rs`, `transcript/view.rs`,
`transcript/virtual_code.rs`, `transcript/working_indicator/mod.rs`,
`view/mod.rs`, `view/attachments.rs`, `view/banners.rs`, `view/history.rs`,
`view/last_response.rs`, `view/session_state.rs`, `view/settings_row.rs`,
`session/backend.rs`, and others.

This is crate-root-anchored, so it does not break the letter of the import rule,
but it hides which items a file actually depends on and makes a crate split (the
kind `app` already went through) far harder to plan. It also defeats the "widen
visibility one step at a time" rule in practice, because nothing at the call site
records what was widened for whom.

Worth converting file-by-file as each is next touched, starting with the ones
that would move first in any further split of `app_agent`.

### 12. Fourteen production files exceed the ~800-line guideline

The guideline allows an exception for "a cohesive state machine or hot loop".
Sorted by how well each fits that exception:

| File | Lines | Reads as |
| --- | ---: | --- |
| `terminal/src/pty_pipe/mod.rs` | 1123 | Hot loop — exception applies |
| `agent_utils/src/claude_code/stream_json/mod.rs` | 1453 | Protocol state machine — exception plausibly applies |
| `agent_utils/src/codex/app_server/mod.rs` | 1133 | Same |
| `agent_utils/src/deepseek/session.rs` | 1533 | Same |
| `app_agent/src/session/mod.rs` | 1754 | Mixed: lifecycle, git polling, history loading, timers — splittable |
| `app_agent/src/transcript/render.rs` | 1442 | View code — splittable by row kind |
| `agent_utils/src/claude_code/tasks/mod.rs` | 1022 | Splittable |
| `platform/src/unix/mod.rs` | 982 | Splittable |
| `app/src/ui/shell/mod.rs` | 877 | Splittable |
| `app_agent/src/view/settings_row.rs` | 860 | View code — splittable |
| `agent_utils/src/chat/mod.rs` | 846 | Vocabulary types — splittable |
| `agent_utils/src/codex/app_server/host/mod.rs` | 819 | Borderline |
| `app_agent/src/composer/mod.rs` | 816 | Borderline |
| `app/src/ui/background_tasks/mod.rs` | 804 | Borderline |

`app_agent/src/session/mod.rs` is the clearest candidate: it already has five
sibling files (`backend`, `conversation`, `events`, `update_recovery`, `tests`),
and the git-branch polling, recent-session loading, and start-overlay timing it
carries are three unrelated concerns that would each read better next to their
own state.

### 13. Orphaned doc comment in `pty_pipe`

`crates/terminal/src/pty_pipe/mod.rs:209-211`

```rust
/// Convert an OSC 7 working-directory value to a path. Strips a `file://host`
/// prefix when present (`file://host/path` → `/path`); otherwise uses the
/// value verbatim.
/// Convert `scrollback-history-limit` (in **lines**) to the engine's
```

The OSC 7 helper was removed and its doc block was left glued to the front of
`scrollback_bytes`'s, so `cargo doc` renders one function documented as two. No
OSC 7 conversion function remains anywhere in `crates/terminal/src`.

### 14. A comment cites a patch id as its rationale

`crates/terminal/src/ghostty/tests.rs:1261` opens with
``/// `0003-pwd-store-osc7-headless.patch` routes `report_pwd` to ...``.

The repo rule is to replace patch and ADR identifiers with the actual technical
reason. This is also exactly what the pre-commit comment check rejects on newly
added lines, so it will block the next edit that touches this block.

---

## P3 — Smaller reuse and quality items

### 15. `active_colors()` copies a 51-field struct under a lock to read one field

`crates/config/src/lib.rs:396`, used at
`crates/app_terminal/src/frame/colors.rs:123, 127, 132`

`Colors` has 51 `pub` fields, mostly `ColorArray` (`[f32; 4]`), so it is roughly
800 bytes. `theme_default_foreground`, `theme_default_background`, and
`theme_selection_background` each take an `RwLock` read guard, copy the entire
struct out, and then read a single field.

Every current caller correctly hoists these out of its loop, so this is not on a
per-cell path — but it is on a per-render path, and the fix is small: have the
three helpers read their field inside the guard rather than copying the struct.

### 16. `current_branch` spawns a second process on detached `HEAD`

`crates/agent_utils/src/git.rs:43`

`git branch --show-current` returns empty on a detached `HEAD`, which then costs
a second `git rev-parse --short HEAD` spawn. A single
`git rev-parse --abbrev-ref --short HEAD` (or `git symbolic-ref --short -q HEAD`
falling through in one call) answers both cases. Compounds with item 4.

### 17. `String → SharedString` conversions per sidebar row

`crates/app/src/ui/workspace_sidebar/rows.rs:225, 242, 243`

```rust
.child(SharedString::from(display_path.clone()))
let drag_name: SharedString = display_label.clone().into();
let drag_cwd: SharedString = display_path.clone().into();
```

Most of the 31 `.clone()` calls in this file are `SharedString` or `Entity`
clones and cost a refcount bump. These three clone a `String` before converting.
Building `display_label` and `display_path` as `SharedString` up front removes
the copies; the same shape appears in `app_agent/src/view/settings_row.rs`.

### 18. Repeated Noise builder construction

`crates/remote_net/src/protocol/noise/mod.rs:32, 49, 60, 68, 76`

`Builder::new(PATTERN_IK.parse().expect("valid pattern"))` appears five times
across two patterns. The `expect` is sound (the patterns are compile-time
constants), but a two-line `fn builder(pattern: &str) -> Builder` removes the
repetition and gives the invariant one place to be stated.

### 19. `remote_session_hub` is a library listed in `default-members`

`Cargo.toml:57`

It exposes no binary and `remote_net` already depends on it
(`crates/remote_net/Cargo.toml:28`), so it is built regardless. Listing it adds
nothing; harmless, but it makes `default-members` read as if the crate were a
build target of its own.

### 20. `crates/web_client/` holds only `dist/` and is not a workspace member

No `Cargo.toml`, no `.rs` files, no mention in the root `Cargo.toml`. This is the
in-progress browser client (see `docs/research/web-client-progress.md`), so it is
not dead — but a reader running `cargo metadata` or scanning `crates/` will not
locate it, and there is no README in the directory saying why. One line of prose
in `crates/web_client/README.md` would close the gap.

---

## Build configuration

`Cargo.toml` release profile:

```toml
[profile.release]
strip = "symbols"
split-debuginfo = "packed"
debug = "full"
```

`debug = "full"` emits complete debug info for every crate in the workspace *and*
all 283 workspace dependencies, then `strip = "symbols"` discards the symbol
table from the binary while `split-debuginfo = "packed"` keeps the separate file.
If the goal is symbolicated crash reports, `debug = "line-tables-only"` gives
file-and-line for backtraces at a fraction of the codegen and link time, and
shrinks the shipped debug artifact substantially. Keep `"full"` only if someone
is actually loading these builds into a debugger and inspecting locals.

`[profile.dev]` sets `opt-level = 0` with no per-package override. For a terminal
emulator this makes debug builds unusable for anything perf-adjacent, because the
VT engine, the rope, and the color math all run unoptimized. Adding:

```toml
[profile.dev.package."*"]
opt-level = 2
```

optimizes dependencies (including the vendored GPUI and `ghostty-vt`) while
leaving first-party crates at `opt-level = 0` for debuggability. Dependencies are
compiled once and cached, so the incremental loop does not get slower.

---

## Test discipline

Test code is 32,534 of 126,159 lines (~26%), which is a healthy ratio, and the
tree is unusually clean on the usual markers: **one** `TODO` in the whole of
`crates/` (`platform/src/unix/mod.rs:659`, and it names a real gap — a forked
process whose failure is never observed), **zero** `#[allow(dead_code)]` outside
`platform`, no commented-out code blocks, and no stray `dbg!`/`println!` debug
residue.

No test-deletion candidates were identified from names and structure alone;
judging whether a given test earns its place needs the failure it prevents, and
that is a per-test read rather than a scan. The one gap worth naming:

- **`crates/platform/src/unix/mod.rs:659`** — the TODO says the fork's failure is
  never observed. Nothing currently fails if the forked child dies immediately. A
  test that forks a nonexistent executable and asserts the caller learns about it
  would prevent silent launch failures on Unix.

---

## What is already right

Worth stating, because several of these are the reason the items above are as
small as they are:

- **`app_agent/src/capabilities.rs`** is the right shape for per-harness
  behavior: one table, one field per question, no `Default`, and a comment on
  each field explaining what breaks if it is wrong. Item 6 is only about the
  places that bypass it.
- **`agent_utils::hook_store`** already absorbs the Claude/Codex hook
  install-uninstall-status logic; the per-provider modules are thin bindings over
  it rather than two copies.
- **`config`'s per-section `patch_document`** functions look like duplication to
  a name scan and are not: five sections, five small writers, one shape.
  Collapsing them would cost more than it saves.
- **`app_terminal/src/frame/extract.rs`** already caches per row by version and
  selection state, reuses a shared empty `Arc` for image-free frames, and
  documents why. This is the hot path and it has clearly been worked on.
- The **frame-scheduling rule** in `AGENTS.md` (`cx.notify` alongside
  `on_next_frame` for anything deferred from outside a frame) is a real,
  hard-won invariant that is not discoverable from GPUI's own docs.

---

## Suggested order

1. Items 1–3 together — one pass over `composer/palette.rs`, `commands/mod.rs`,
   and the `i18n` signature. Highest measurable payoff, and they touch the same
   code.
2. Items 4 + 16 — shared cached git query. Removes *N* process spawns per
   interval.
3. Items 5, 7, 8 — small, self-contained, remove six `unreachable!()`.
4. Items 9 + 13 + 14 — mechanical convention cleanup, one commit.
5. Build profile changes — measure the release build before and after.
6. Items 6, 11, 12 — larger structural work, best done as each file is next
   touched rather than as a sweep.
