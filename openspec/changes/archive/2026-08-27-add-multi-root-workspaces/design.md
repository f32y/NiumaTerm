## Context

See `proposal.md` for motivation and the delta specs for required behavior.

The current application model stores `Workspace { cwd: String }`, persists that value as `WorkspaceState.cwd`, and passes `Option<String>` through `AgentPane` to `Backend::spawn`. All three adapters set one process or session directory:

- Codex starts `codex app-server` in that directory, starts a thread without a root list, and sends a sandbox type on each turn.
- Claude Code starts one stream-json process in that directory.
- DeepSeek Harness creates one host session whose immutable `cwd` becomes its sandbox workspace root.

The horizontal tab bar and workspace sidebar both build their `+` button from `ui/tab_bar/menu.rs::new_tab_menu`. That menu currently lists each terminal Profile at the top level and calls `Shell::open_profile_tab`, which always reads `WorkspaceManager::active_cwd()`. The menu component already supports a nested submenu, but `new_tab_menu` currently discards the `Window` argument that submenu construction needs.

Current external behavior is not uniform:

- OpenAI's desktop Project model uses one primary directory plus additional roots; the primary directory owns `cwd`, Git, and configuration discovery, while all roots remain available for selected-root access. OpenAI App Server documents `cwd` and full workspace-write policies with `writableRoots`. [OpenAI Projects](https://learn.chatgpt.com/docs/projects), [Codex App Server](https://learn.chatgpt.com/docs/app-server)
- Claude Code exposes `--add-dir` for additional read and edit directories and deliberately keeps most `.claude/` discovery anchored outside those additions. [Claude Code CLI](https://code.claude.com/docs/en/cli-usage)
- DeepSeek Harness currently resolves exactly one workspace root from `SessionHeader.cwd`; its documented workspace-write policy has no additional writable roots. [DeepSeek sandbox policy](https://github.com/deepseek-ai/deepseek-harness/blob/master/packages/sandbox/sandbox-policy/README.md)

The DeepSeek adapter files already have unrelated local modifications. Implementation must preserve those edits and avoid mixing them into this change.

## Goals / Non-Goals

**Goals:**

- Give Workspace ownership, persistence, UI, routing, and Agent Tab launch one shared meaning for primary and additional directories.
- Preserve single-directory snapshots and current default-directory behavior.
- Give Codex and Claude Code full selected-root access without constructing shell command strings.
- Keep DeepSeek usable and truthful under its current one-root restriction.
- Make future harness behavior an explicit compile-time choice.
- Let terminal users select any Profile and attached directory while preserving one-click primary-directory launches.

**Non-Goals:**

- Restrict ordinary terminal processes to the selected directories.
- Merge several repositories into one Git repository or select a separate Git root per tab.
- Change provider-owned history identity from its primary-directory basis.
- Modify user-owned Claude Code or DeepSeek configuration files.
- Build a NiumaTerm filesystem MCP server in this change.
- Automatically grant danger-full-access or replace selected roots with a broader common ancestor.
- Add per-directory Agent Profile entries; agents receive their workspace roots through the agent adapter instead.

## Decisions

### 1. Model normal workspace location as a primary path plus ordered additions

Introduce an owned value with a non-empty normal-workspace invariant:

```rust
pub struct WorkspaceRoots {
    primary: String,
    additional: Vec<String>,
}
```

`Workspace` stores `Option<WorkspaceRoots>` because the Settings pseudo workspace has no filesystem location. Existing `active_cwd()` and summary `cwd` accessors remain as primary-path views while callers migrate to `active_roots()` where they need the full set.

An explicit primary field is preferable to encoding primary status as vector index zero in the Rust domain. It keeps the invariant visible at call sites and lets persistence retain its existing `cwd` field. `WorkspaceRoots::ordered()` provides primary-first iteration for adapters and UI.

Alternative considered: replace `cwd` with `Vec<String>` and treat index zero as primary. This is compact, but it makes every vector reorder semantically significant and adds unnecessary migration work.

### 2. Canonicalize new selections, but restore saved paths without requiring the filesystem

The path picker validates and canonicalizes newly attached directories on a background executor. Exact duplicates use platform-aware normalized identity: case-insensitive on Windows, case-sensitive elsewhere, with separators and trailing components normalized.

Persisted paths restore even when unavailable. This preserves workspace and tab state across disconnected drives or temporarily missing directories. Availability is presentation state, not identity, and can be refreshed asynchronously.

`WorkspaceRoots` owns pure mutations and returns a small result enum for duplicate, missing member, would-be-empty, and applied outcomes. The shell decides how to report each result.

Alternative considered: call `canonicalize` inside every comparison. That would make path routing perform filesystem I/O and would cause missing directories to lose identity during restore.

### 3. Route paths across every root with deterministic precedence

Refactor `exact_match` and `best_match` to inspect all roots. Candidate rank is:

1. longest normalized ancestor path;
2. primary root before additional root for an equal path;
3. existing workspace order.

This retains the current longest-prefix behavior while making Open Folder and open-in-best-workspace find a workspace through any attached directory.

### 4. Preserve local-state compatibility by appending `additional_cwds`

`WorkspaceState` keeps:

```rust
pub cwd: Option<String>
pub additional_cwds: Vec<String>
```

The new field uses `#[serde(default, skip_serializing_if = "Vec::is_empty")]`. Old snapshots therefore restore as one-root workspaces. New snapshots still put the primary path in `cwd`, so an older NiumaTerm build can restore the workspace and tabs while ignoring additions.

There is no one-time data rewrite and no new persistence dependency.

### 5. Separate configured workspace roots from an active agent-session snapshot

Add a provider-neutral `AgentWorkspace` value in `nmt_agent_utils`:

```rust
pub struct AgentWorkspace {
    pub primary: Option<String>,
    pub additional: Vec<String>,
}
```

The shell passes it to `AgentPane`, and `Backend::spawn` receives it beside `LaunchConfig`. Profile launch settings remain independent from workspace state.

`AgentPane` retains two meanings:

- configured roots, updated by the shell when the parent workspace is edited;
- the immutable snapshot cloned into the current backend start.

An edit does not mutate a running process. A later `/new`, retry, or restored start clones the newest configured roots. Input-history scope uses a normalized primary-first signature so workspaces with the same primary but different additions remain isolated.

Alternative considered: store additional roots in `LaunchConfig`. That would mix user profile configuration with one workspace's mutable state and would also change the DeepSeek shared-host key for data that belongs to sessions, not hosts.

### 6. Translate Codex roots at the App Server boundary

The Codex process starts in the primary directory. Initial thread parameters include the primary `cwd` and primary-first `runtimeWorkspaceRoots`, matching the desktop application behavior already inspected. Workspace-write turn parameters use the full policy shape and set `writableRoots` to the same ordered list rather than sending only `{ type: "workspaceWrite" }`.

The public App Server page documents `cwd` and `writableRoots` but does not currently list `runtimeWorkspaceRoots`. NiumaTerm will therefore:

- cover the field with focused request-shape tests;
- exercise it against the minimum supported Codex CLI during integration validation;
- surface a provider incompatibility if the server rejects the request;
- never retry by widening access.

Read-only and danger-full-access retain their existing provider meaning. Selected roots describe the workspace but do not narrow a mode that is intentionally broader or stricter.

Alternative considered: concatenate root instructions into the user's first prompt. That would alter provider history and make replay show text the user did not submit.

### 7. Translate Claude roots into native argument boundaries

Claude Code keeps `Command::current_dir(primary)`. When additions exist, launch builds:

```text
--add-dir <additional-1> <additional-2> ...
```

Each path is one `Command` argument. The arguments are applied to new and resumed processes, after profile executable arguments and before the prompt process begins. Claude retains primary-directory session storage and configuration discovery.

No settings file is edited. Persisting `permissions.additionalDirectories` would leak one NiumaTerm workspace into unrelated Claude sessions, so it is not used.

### 8. Represent DeepSeek's current limit as primary-only capability

Extend the existing no-default `Capabilities` table with a named multi-root mode:

```rust
pub enum MultiRootAccess {
    Full,
    PrimaryOnly,
}
```

Codex and Claude declare `Full`; DeepSeek declares `PrimaryOnly`. DeepSeek continues to call `session.create` with the primary `cwd`. A non-dismissible Agent Tab banner lists the unavailable additional-directory count and explains that the installed Harness accepts one workspace root.

NiumaTerm does not choose a common ancestor, create escaping links, edit the user's Harness profile, or switch permission presets. Those alternatives either expose unselected paths, are rejected by canonical sandbox checks, or change user-owned configuration.

The capability value is the future extension point. When DeepSeek publishes a per-session multi-root policy, its version classifier and session payload can change behind the same `AgentWorkspace` input.

### 9. Keep provider history anchored to primary cwd

Claude transcript directories, Codex `thread/list`, and DeepSeek session filtering remain keyed by primary cwd. Additional directories affect access, not provider session identity.

Resume passes the current `AgentWorkspace` snapshot. This means a conversation retains its provider id and transcript while additional access follows the workspace's current configuration. The history UI continues to offer All Directories as the explicit cross-directory view.

### 10. Keep UI state and domain mutations separate

Move workspace-root editor rendering and asynchronous path selection into a dedicated child module under `ui/shell/`. The Workspace manager performs root mutations and returns outcomes; the dialog owns focus, notifications, availability labels, and confirmation.

Because `workspace.rs` will gain new production behavior and already carries an inline test module, implementation first moves it to `workspace/mod.rs` with tests in `workspace/tests.rs`, preserving public import paths through the module root.

The sidebar continues to show its two-tier row. The primary path remains the secondary line and a compact `+N` token reports additions. The tooltip carries the complete ordered list.

### 11. Project terminal launch choices from the active workspace snapshot

Keep `new_tab_menu` as the single builder used by the horizontal tab bar and workspace sidebar. Change its call shape to receive `&mut Window`, which lets it use the existing `PopupMenu::submenu` API without changing `gpui-component`.

Menu construction takes one snapshot of:

- configured terminal Profiles with non-empty commands;
- active workspace roots in primary-first order;
- root availability at the moment the menu opens.

The top level retains one terminal entry per Profile. Those entries call `open_profile_tab`, which remains the primary-directory convenience path. If the root snapshot contains more than one directory, a localized `More` submenu is inserted after terminal Profiles and before the existing separator and Agent Profile entries.

The submenu is a flat Profile-major projection. For Profiles P1, P2 and roots A, B, C it produces P1-A, P1-B, P1-C, P2-A, P2-B, P2-C. Each visible label uses `<Profile name> — <full directory path>` so duplicate directory basenames remain distinguishable and accessible. Combinations for unavailable restored directories are disabled rather than omitted.

Split terminal startup into two command-style paths:

```rust
open_profile_tab(profile, window, cx)
open_profile_tab_in_directory(profile, cwd, window, cx)
```

The first reads the active primary directory and delegates to the second. `More` calls the explicit-directory path, and the existing `open_dir_tab` flow can reuse it after choosing its target workspace. The explicit path starts only the new process; it does not make that directory primary or mutate Workspace state.

A small pure menu-projection helper returns the Profile/root combinations and enabled state. Unit tests cover count, ordering, omitted empty-command Profiles, full-path labels, and unavailable roots without driving GPUI pointer events.

Alternative considered: one submenu per Profile or per directory. That would add a third menu depth or change the requested single `More` level, so the flat combination list is retained.

## Risks / Trade-offs

- **[Codex root field changes across CLI versions]** → Add request-shape coverage, validate the minimum supported CLI, and fail with a provider-specific message instead of dropping roots.
- **[DeepSeek users expect every selected root to work]** → Show the limitation before first prompt and keep the primary path visible; do not imply selected-root isolation in broader permission modes.
- **[Canonical path aliases differ across platforms]** → Centralize path identity, test drive-letter case, separators, trailing components, and unavailable restore paths.
- **[Workspace edits surprise running tabs]** → Display that edits apply to new or restarted sessions and keep the active snapshot unchanged.
- **[Two workspaces own overlapping roots]** → Use deterministic longest-root and primary-root ranking without rewriting either workspace.
- **[Additional roots increase launch and policy payload size]** → Deduplicate at attachment time and preserve a practical UI limit if measurements show a need; no limit is introduced without evidence.
- **[Many Profiles and roots produce a long More submenu]** → Keep deterministic ordering and scrolling behavior from `PopupMenu`; do not hide valid combinations or add another menu level.

## Migration Plan

1. Add default-empty `additional_cwds` deserialization and round-trip coverage before any UI writes the field.
2. Introduce `WorkspaceRoots` and adapt existing one-cwd constructors through single-root helpers.
3. Update routing, summaries, and shell tab creation while single-root behavior remains unchanged.
4. Add the editor, sidebar summary, explicit-directory terminal launch path, and shared New Tab `More` submenu.
5. Introduce `AgentWorkspace`, update `AgentPane`, and translate one adapter at a time with focused request or argument tests.
6. Add DeepSeek primary-only disclosure after the shared capability exists.
7. Validate old local-state fixtures, multi-root round trips, terminal Profile/root combinations, each adapter's start and resume path, and an application launch using `--testing`.

Rollback requires no data conversion. An older build reads `cwd`, ignores `additional_cwds`, and restores the primary directory and existing tabs.
