# Proposal: add-agent-slash-command-palette

## Why

Agent Tab currently sends all composer text directly to Claude Code or Codex. Users cannot enter `/` to browse and execute session commands as they can in the native Clients. Missing discovery and local routing makes common actions harder to reach and can send TUI-local Codex commands such as `/compact` as ordinary model prompts.

## What Changes

- Add a slash-command palette to the Claude and Codex Agent Tab composer. Open it only for `/` at the start of a message, filter it as the user types, and support keyboard and mouse selection.
- Show each command's name, purpose, optional argument hint, and current availability. Explain unavailable commands instead of sending them through the ordinary message path.
- Add backend-neutral command catalog and execution-result models so AgentPane owns interaction and transcript presentation while each backend owns capability discovery and execution.
- Build Claude Code's dynamic catalog from executable slash commands published by the stream-json session and send published provider commands through the Claude protocol. Keep NiumaTerm commands local.
- Show only commands implemented by NiumaTerm for Codex, mapping them to app-server RPCs, existing settings controls, or session-lifecycle actions. Never send a recognized Codex slash command as ordinary `turn/start` or `turn/steer` text.
- Initially support `/compact`, `/new` and `/clear`, `/model`, `/permissions`, `/status`, and `/review`, while allowing the Claude dynamic catalog to add commands supported by the current stream-json session.
- Define behavior for commands during active work, unknown commands, backend errors, and a dynamic catalog that is not ready, ensuring a slash command never creates a synthetic user turn or an unintended steer.

## Capabilities

### New Capabilities

- `agent-slash-commands`: slash-command triggering, list display, filtering, and selection in Agent Tab, plus Claude and Codex command discovery, availability, execution routing, and error handling.

### Modified Capabilities

None. Existing `agent-session-history` requirements for session enumeration and restoration remain unchanged; new-session commands only reuse the existing session lifecycle.

## Impact

- `crates/agent_utils/src/chat.rs`: add backend-neutral command descriptions, catalog events, and command-result types.
- `crates/agent_utils/src/claude_code/stream_json.rs`: parse Claude's published slash-command catalog and execute published commands.
- `crates/agent_utils/src/codex/app_server.rs`: add app-server request and response handling for compaction, review, and related commands.
- `crates/app/src/ui/agent_pane.rs`: add palette state, composer command parsing, keyboard and mouse interaction, local commands, and transcript feedback.
- Implement the palette in AgentPane without changing general gpui-component input or completion behavior.
- Add no external dependency and preserve ordinary messages, approvals, history restoration, and existing settings dropdown behavior.
- External behavior depends on the command set published by Claude Code stream-json initialization and support in the current Codex app-server for methods such as `thread/compact/start` and `review/start`.
