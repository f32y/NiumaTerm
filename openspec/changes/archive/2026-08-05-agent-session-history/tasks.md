# Tasks: agent-session-history

## 1. Backend-neutral types and Claude data source

- [x] 1.1 Add `SessionSummary { id, title, branch, last_active }` to `chat.rs`.
- [x] 1.2 Create `crates/agent_utils/src/claude_code/sessions.rs` with cwd-to-munged-project-directory conversion and directory scanning based on UUID filenames and mtime. Cover the conversion rules with unit tests.
- [x] 1.3 Extract titles in sessions.rs by reading at most the first 64 KiB, finding the first `"type":"user"` row with text, and reading text and gitBranch. Use the first eight characters of the id when missing, and test with a real repository JSONL sample.
- [x] 1.4 Parse one complete JSONL transcript into `Vec<Item>` with user and assistant text. Skip sidechains, hooks, queue operations, and tool details, and combine tool calls into a placeholder. Cover each skip rule with unit tests.

## 2. Codex protocol additions

- [x] 2.1 Add a `thread/list` request to `app_server.rs` with cwd filtering, `sortKey: "recency_at"`, and a limit of 50. Parse the response into `Vec<SessionSummary>` and deliver it to the UI through the event channel.
- [x] 2.2 Add a restore path in `app_server.rs` that uses `thread/resume {threadId}` instead of `thread/start`, maps `thread.turns[].items` to replay items, and restores model and effort into `ThreadSettings`.
- [x] 2.3 Load the next page through `nextCursor` when scrolling reaches the end. The first version may request only the first page while retaining the cursor field.

## 3. Claude restore path

- [x] 3.1 Add `resume: Option<&str>` to `stream_json.rs::Session::spawn` and append `["--resume", id]`.
- [x] 3.2 Capture `session_id` from init in `process_system`, store it in Session, and expose it to AgentPane.
- [x] 3.3 When the process reports "No conversation found" or another restore error, deliver an Error item and keep the composer usable.

## 4. AgentPane UI

- [x] 4.1 Add `history: Vec<SessionSummary>` and `history_dismissed: bool` to AgentPane. Start loading in `new()` by scanning Claude history in the background or requesting Codex history after Ready.
- [x] 4.2 Render the history block above the input inside the composer shell, aligned with the approval panel and separated by a hairline. Show it only for `items.is_empty() && !history.is_empty() && !history_dismissed`.
- [x] 4.3 Use `v_virtual_list`, `VirtualListScrollHandle`, and `Scrollbar` with 28 px rows and a 280 px maximum container height. Show a truncated title, a subdued 12 px branch icon and name, and a right-aligned relative timestamp.
- [x] 4.4 On row selection, set `history_dismissed`, restore through the selected backend, and fill the transcript with replay items. Claude parses history and starts with resume; Codex calls thread/resume.
- [x] 4.5 Set `history_dismissed` when the user sends the first message.

## 5. Validation

- [x] 5.1 Run `cargo test -p agent_utils` for cwd conversion, title extraction, and transcript replay parsing.
- [x] 5.2 After `cargo build`, launch `target\debug\NiumaTerm.exe --testing` and give the user a manual GUI checklist for both backends: list display and scrolling, context continuity after restore by asking "What did we discuss?", restore failures, and the empty state without history. The user completed this, including two UI refinement passes.
- [x] 5.3 Confirm that restoring a Claude session appends to its original JSONL by checking mtime and size without creating a new file. This was tested with Claude 2.1.222: the original file grew from 25 KiB to 33 KiB, retained its session id, and no new file appeared.
