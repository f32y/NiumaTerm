## Why

NiumaTerm workspaces currently own one working directory, so related repositories or documentation trees must be split across workspaces and an Agent Tab cannot receive the user's complete working set. The application now has three agent integrations with different directory and permission mechanisms, so multi-directory behavior needs one application-owned model with explicit per-harness translation.

## What Changes

- Let each normal workspace retain one primary directory and zero or more ordered additional directories, with add, remove, and make-primary actions.
- Persist additional directories while keeping the existing `cwd` field as the primary directory for backward-compatible local state.
- Match opened paths against every workspace directory while keeping shells, Git discovery, generated labels, and default relative-path resolution anchored to the primary directory.
- Keep each top-level terminal Profile entry in the shared New Tab menu anchored to the primary directory, and add a `More` submenu that offers every terminal Profile and workspace-directory combination when additional directories exist.
- Pass one immutable workspace access description from the shell through `AgentPane` and `Backend::spawn` instead of passing a bare cwd.
- Give Codex all selected roots through its App Server request and workspace-write policy, and launch Claude Code in the primary directory with one `--add-dir` entry per additional directory.
- Keep DeepSeek Harness usable without silently widening access: its current workspace-write policy receives only the primary directory, and the Agent Tab clearly reports that additional directories are unavailable until the installed harness exposes a multi-root policy.
- Scope provider session history to the primary directory and reapply the current workspace access description when a conversation resumes.
- Keep all provider-specific reduction in exhaustive backend branches and add a named multi-root capability so a future harness must choose its behavior deliberately.

## Capabilities

### New Capabilities

- `workspace-multi-directory`: Workspace directory ownership, editing, persistence, path matching, and primary-directory behavior.
- `agent-workspace-access`: Shared Agent Tab workspace input and the Codex, Claude Code, and DeepSeek Harness translations.

### Modified Capabilities

- `workspace-sidebar-information-hierarchy`: Show the primary directory and additional-directory count without losing the existing compact hierarchy or full-path accessibility.
- `agent-session-history`: Define history and resume behavior for an Agent Tab opened from a multi-directory workspace.

## Impact

- Workspace domain and lifecycle code under `crates/app/src/workspace.rs` and `crates/app/src/ui/shell/`.
- Session persistence in `crates/config/src/local_state.rs` and `crates/app/src/ui/persistence.rs`.
- Workspace creation, terminal launch, and shared New Tab UI under `crates/app/src/ui/shell/`, `crates/app/src/ui/tab_bar/menu.rs`, and `crates/app/src/ui/workspace_sidebar/`.
- Shared Agent Tab construction, input history, links, and backend launch under `crates/app/src/agent/`.
- Harness adapters under `crates/agent_utils/src/codex/`, `claude_code/`, and `deepseek/`.
- Localization strings and focused regression coverage for migration, root editing, path matching, launch arguments, App Server requests, DeepSeek reduction, and resume.
