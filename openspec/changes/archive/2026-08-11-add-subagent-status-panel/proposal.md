## Why

Codex and Claude Code can run child agents in the background, but NiumaTerm gives users no persistent place to monitor their progress. Provider-specific child traffic is also either applied to the parent session or discarded, so reliable status display requires child activity to be reduced separately from the parent conversation.

## What Changes

- Add a `Background Tasks` button at the upper-right of the application title bar to show a read-only right-side view for active Codex and Claude Code Agent tabs.
- Show running and finished subagents with provider-qualified identity, objective or latest status, lifecycle state, and relative timing.
- Show the active-task count and an unseen-activity indicator on the title-bar `Background Tasks` button.
- Let Git and `Background Tasks` select the same resizable right-side area so opening both never consumes two independent columns.
- Route Codex app-server notifications by thread ID and restore descendant state after session resume or reconnect.
- Reduce Claude Code Task launches, task lifecycle events, linked sidechain activity, tool results, and matched hook events without inserting child content into the parent transcript.
- Restore Claude Code subagent summaries from the selected session history and reconcile them with later live updates.
- Keep child transcript navigation, child messaging, approvals, stop controls, and non-agent background work outside this initial change.

## Capabilities

### New Capabilities

- `subagent-status-panel`: Defines provider-aware child-agent discovery, lifecycle tracking, parent-session isolation, activity signaling, and the read-only `Background Tasks` view for Codex and Claude Code.

### Modified Capabilities

None.

## Impact

- Codex app-server event handling in `crates/agent_utils/src/codex/app_server.rs`.
- Claude Code live-stream, history, and hook handling in `crates/agent_utils/src/claude_code/`.
- Provider-independent Agent events and task models in `crates/agent_utils/src/chat.rs`.
- Agent pane state projection in `crates/app/src/agent_pane/`.
- Right-side area selection, rendering, resizing, and header controls in `crates/app/src/ui/shell.rs` and `crates/app/src/ui/git_sidebar.rs`.
- No new external dependency or persisted-data migration is expected.
