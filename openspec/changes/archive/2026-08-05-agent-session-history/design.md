# Design: agent-session-history

## Context

Agent Tab in `crates/app/src/ui/agent_pane.rs` dispatches to two backends through the `Backend` enum:

- **Claude Code** in `crates/agent_utils/src/claude_code/stream_json.rs`: `Session::spawn` launches `claude -p --input-format stream-json --output-format stream-json …` from a fixed argument array, using cwd as the process working directory. It has no session-id model. `process_system` reads `model` and `permissionMode` but discards `session_id` from init.
- **Codex** in `crates/agent_utils/src/codex/app_server.rs`: after starting `codex app-server`, it sends `thread/start` through JSON-RPC and already captures `thread_id`.

Both persistence paths were tested and checked against source:

- Claude stores sessions at `~/.claude/projects/<munged-cwd>/<session-uuid>.jsonl`, where munged-cwd replaces every non-alphanumeric character in cwd with `-`. Sessions created by the GUI with `entrypoint: "sdk-cli"` use the same location. `--resume <id>` works in print and stream-json modes with Claude 2.1.222: it retains the original session id and appends to the same JSONL under the same cwd; another cwd returns "No conversation found"; output begins with init and contains no replay of prior messages.
- The Codex app-server v2 protocol, confirmed through the 0.146.0 schema and `codex-rs/app-server/README.md`, provides `thread/list` with exact cwd filtering, `sortKey: recency_at`, cursor pagination, and interactive sources by default. `thread/resume` has the same response shape as `thread/start`, normally includes full reconstructed history in `thread.turns[].items`, and retains the persisted model and reasoningEffort.

The empty-state insertion point is in `AgentPane::render`, where the transcript area is blank when `items` is empty. The composer shell already has a region above the input for the approval panel.

## Goals / Non-Goals

**Goals:**

- Show historical sessions for both backends and the current cwd in an empty Agent Tab, then restore a selected session and prefill its transcript.
- Display 10 rows by default and use gpui-component `v_virtual_list` for scrolling.
- Match the existing composer shell and the subdued controls in the settings row, following the t3code hierarchy.

**Non-Goals:**

- Session deletion, archiving, renaming, or search.
- Browsing sessions across cwd values. Resume is cwd-specific, so the list must also filter by cwd.
- Full rendering of Claude tool calls during historical replay. A folded placeholder is sufficient initially and can later align with every `ChatItem` type.
- Taking over a currently running session. The process is gone after application restart, so this change handles only persisted history.

## Decisions

### D1: Scan Claude files and use the Codex protocol

For Claude, scan `~/.claude/projects/<munged-cwd>/*.jsonl`. The filename is the session id, mtime is the last-active time, and the title and branch come from the first `"type":"user"` record near the start of the file, which contains `message.content[].text`, `gitBranch`, and `timestamp`. `~/.claude/history.jsonl` is unsuitable because it records only interactive CLI input and omits sessions started by the GUI.

For Codex, use `thread/list` with `cwd`, `sortKey: "recency_at"`, and default sourceKinds. The protocol already supports filtering, pagination, and titles through `name` or `preview`; scanning `~/.codex/sessions/` would duplicate that work. The list must wait for app-server initialization, which is acceptable because the process starts regardless.

### D2: Use a backend-neutral list model

Add `SessionSummary { id: String, title: String, branch: Option<String>, last_active: SystemTime }` to `chat.rs`. AgentPane stores only `Vec<SessionSummary>`, keeping the list UI independent of the backend while each backend produces the shared structure.

### D3: Add a spawn argument for Claude and replace the Codex start request

- Add `resume: Option<&str>` to `stream_json.rs::Session::spawn`. When present, append `["--resume", id]` to the argument array without changing other arguments. Make `process_system` store `session_id` from init so sessions created in this tab have an identity that can be restored later.
- Parameterize the initialization sequence in `app_server.rs`. The restore path replaces `thread/start` with `thread/resume {threadId}`. Both responses contain a thread object, while restore additionally reads `thread.turns[].items` for replay and updates `ThreadSettings` with the restored model and effort.

### D4: Replay transcripts through backend-specific paths

- **Codex:** map `thread.turns[].items` from the `thread/resume` response to `ChatItem` values. Map userMessage to User and agentMessage to Agent; skip other types or display a folded placeholder. This requires no extra I/O.
- **Claude:** stream-json produces no history during resume. A new `claude_code/sessions.rs` module parses the selected session JSONL completely, keeps `type: user` and `type: assistant` rows with text, and skips `isSidechain: true`, queue operations, hook attachments, and tool_result rows. Parse on a background thread and deliver results through the existing channel before or alongside process spawn.

Claude stream-json has no history-query API, so it cannot use the same protocol approach as Codex.

### D5: Place a virtual history block inside the composer shell

Render the history list as the first child of the composer shell, in the same position as the approval panel, with a hairline divider above the input. This avoids the negative margins and masks used by the t3code web version while retaining one rounded outer container.

Use a fixed 28 px row height and `v_virtual_list(view, "agent-history", Rc<Vec<Size>>, render_range)` with equal item sizes. Set the container to `max_h(px(280.))` for 10 visible rows, and attach `VirtualListScrollHandle` plus gpui-component `Scrollbar`. Each row shows a truncated title on the left, a 12 px subdued branch icon and name, and a right-aligned 12 px relative timestamp with tabular digits.

Show the list only when `items.is_empty() && !history.is_empty() && !history_dismissed`. Set `history_dismissed` after a restore selection or the first sent message.

### D6: Load Codex history after initialization

`thread/list` can be sent only after the Codex initialize handshake. Send the first list request with the initialization-complete event and return its response through the existing UI event channel. Request 50 rows on the first page and fetch the next page when `nextCursor` is non-empty and the user reaches the end. Claude has no such timing constraint, so `AgentPane::new` can start its background scan immediately.

## Risks / Trade-offs

- [Claude JSONL is a private persistence format and can change] Depend on only `type`, `message.content[].text`, `isSidechain`, `gitBranch`, and `timestamp`. Skip unknown rows. On parse failure, use the session-id prefix as the title without breaking the list.
- [Large session files can exceed 1.6 MB] Read at most 64 KiB during list loading for the title. Parse one complete file on a background thread only after the user selects it, so the UI does not block.
- [`--resume` depends on the Claude CLI version] On failure, show an error item and leave the composer usable so a new session can start. Version 2.1.222 is the tested baseline.
- [Codex `thread/resume` can rejoin a running thread] This flow restores only `notLoaded` threads, and the application does not open the same thread twice. Treat a resume error through the normal failure path.
- [The restored Claude model or permission display can differ from the original session] The init message already supplies model and permissionMode, and the existing `process_system` path updates them without extra logic.

## Migration Plan

This feature is additive and needs no data migration. It can be reverted as one change. Implement it in two commits:

1. Add the list and data sources, allowing row selection to open a new session temporarily so the UI and scanning can be validated independently.
2. Add resume arguments and requests plus transcript replay.

## Open Questions

- For Claude replay, should tool calls appear as one row per call or one combined row such as "N tool calls"? Combine them initially and adjust during implementation based on the visual result.
- The exact contents of `gitInfo` from Codex `thread/list` 0.146.0 were not checked field by field. Use the real response during implementation and omit the branch when absent.
