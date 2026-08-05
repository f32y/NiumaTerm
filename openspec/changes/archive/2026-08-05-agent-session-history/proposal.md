# Proposal: agent-session-history

## Why

Every Agent Tab currently starts from an empty state. Its process ends with the application, existing Claude Code and Codex sessions cannot continue, and all prior context is lost. Both backends already persist complete sessions. Claude writes `~/.claude/projects/<munged-cwd>/*.jsonl`, while Codex writes rollouts under `~/.codex/sessions/` and exposes `thread/list` and `thread/resume`. Only the Client implementation is missing.

## What Changes

- In a newly opened Agent Tab with an empty transcript and no started session, show historical sessions for the current cwd above the composer. Show 10 by default and load more through a scrolling virtual list styled like the composer hierarchy in t3code.
- Restore a session when the user selects its row, append future messages to the original session, and prefill the transcript with the prior conversation.
- For Claude Code, add `--resume <session-id>` support to `Session::spawn`, scan and parse `~/.claude/projects/<munged-cwd>/*.jsonl` for the history list and transcript replay, and capture `session_id` from the init message.
- For Codex, request history through `thread/list` filtered by cwd, use `thread/resume` instead of `thread/start`, and replay the transcript from `thread.turns` in the resume response.
- Hide the history list after the user sends the first new message or selects a session to restore.

## Capabilities

### New Capabilities

- `agent-session-history`: historical session listing in Agent Tab, including data sources, display, scrolling, and hiding, plus restoration for both backends, transcript replay, and failure handling.

### Modified Capabilities

None. The existing specifications directory was empty, and Agent Tab had no archived specification.

## Impact

- `crates/agent_utils/src/claude_code/stream_json.rs`: add a resume argument to spawn and capture session_id.
- `crates/agent_utils/src/claude_code/sessions.rs`: new JSONL directory scanning, title extraction, and transcript parsing.
- `crates/agent_utils/src/codex/app_server.rs`: add `thread/list` requests and a `thread/resume` startup path.
- `crates/agent_utils/src/chat.rs`: add backend-neutral types such as SessionSummary.
- `crates/app/src/ui/agent_pane.rs`: add history-list state, virtual-list rendering for the empty state, and the restore flow.
- Dependencies: use only the existing `v_virtual_list` and `Scrollbar` from gpui-component; add no external dependency.
- External behavior: require Claude CLI 2.1.x or later for `--resume`, validated with 2.1.222, and the Codex app-server v2 protocol, confirmed from the 0.146.0 schema.
