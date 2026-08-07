## Context

Claude Agent Tab maintains a long-lived process through `claude -p --input-format stream-json --output-format stream-json`. The current slash-command routing can send non-interactive provider commands, but Claude Code's `/rewind` opens a terminal menu and is not one of the non-interactive commands published during structured initialization. Sending `"/rewind"` as a user message therefore cannot provide equivalent behavior.

Claude Agent SDK exposes two lower-level operations that can be composed. With file checkpointing enabled, a `rewind_files` control request can restore files for a user-message UUID. The public SDK helper `fork_session(..., up_to_message_id=...)` can create an independent session from a historical prefix. Their effects differ: the former restores only files tracked by Claude tools, while the latter forks only the conversation and does not copy file undo history.

The current Claude history parser replays records in JSONL file order and does not use `parentUuid` to exclude branches abandoned by rewind or fork. This must be corrected first, or both the checkpoint list and restored transcript can contain messages outside the current conversation chain.

## Goals / Non-Goals

**Goals:**

- Provide a native `/rewind` experience inside Claude Agent Tab without relying on an interactive terminal.
- Support file restoration, conversation restoration, and a combined action independently, with each action's scope visible to the user.
- Preserve the original session and original JSONL so conversation restoration is reversible and failures remain recoverable.
- Use one active `parentUuid` message chain for history replay, checkpoint selection, and forking.
- Report old sessions, expired checkpoints, provider errors, and partial combined-action failures accurately.

**Non-Goals:**

- Implement Claude Code's summarize-from-here or summarize-to-here actions; the public stream-json and SDK control protocol exposes no stable operation for them.
- Truncate or modify the original session JSONL in place. Conversation restoration receives a new session id.
- Snapshot or roll back the working directory, Git branch, Bash writes, external-process writes, or subagent writes.
- Add `/rewind` to Codex or add a rewind button beside transcript messages in the first version.
- Change the live event stream through `--replay-user-messages`. Checkpoint UUIDs come directly from the persisted transcript so replayed user text is not mistaken for a tool result.

## Decisions

### 1. `/rewind` is a Claude command owned by the UI

The Claude adapter declares `/rewind` in its baseline catalog, but Agent Pane routes execution to a local interactive flow rather than the existing `execute_slash_command`. Its policy is `IdleOnly`: it can begin only when no turn, approval, or provider command is active. This prevents the interactive command from being sent to Claude as an ordinary user message and prevents concurrent writes while targets are listed or files are restored.

The UI reuses slash-palette keyboard and mouse behavior in a two-stage overlay:

1. List user prompts on the active chain from newest to oldest, showing truncated text and time.
2. Show `Restore files`, `Restore conversation`, `Restore files and conversation`, and `Cancel`, with file actions explaining that they cover only edits tracked by Claude checkpoints.

Selecting a second-stage action is explicit confirmation. Cancel changes neither transcript, files, nor backend.

### 2. One active-chain model drives replay and checkpoints

Add a structured transcript index to `sessions.rs`. The parser indexes messages by `uuid`, starts at the last valid leaf in file order, follows `parentUuid` backward to the root, and reverses the result into chronological order. It does not use `logicalParentUuid` to cross compaction linking messages, matching Claude Agent SDK session-reading behavior. It continues to parse tool calls and results from messages on the chain while filtering hooks, sidechains, meta records, and queue records.

The checkpoint view contains at least:

```text
ClaudeCheckpoint {
    user_message_id,
    parent_message_id,
    prompt,
    timestamp,
    file_restore_availability,
}
```

Only chain user messages with human-readable text become targets. Tool-result containers, meta user records, and subagent messages do not. If multiple leaves exist, choose the last valid leaf in JSONL order. If a parent is missing, retain only the connected segment that can be established and record a diagnostic; never append another branch.

### 3. File restoration uses a request-correlated control request

Set `CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING=true` for every new Claude process. To restore files, the adapter sends:

```json
{
  "type": "control_request",
  "request_id": "<unique-id>",
  "request": {
    "subtype": "rewind_files",
    "user_message_id": "<checkpoint-user-uuid>"
  }
}
```

The adapter stores `request_id -> PendingControlOperation` and converts the matching control response into an explicit success or failure event. It cannot reuse the current fire-and-forget path that handles only initialize. A successful file restore changes neither transcript, session id, nor composer and creates no user bubble, turn, or working timer.

The provider is authoritative on whether a checkpoint remains available. When session metadata establishes that an old session has no snapshot, disable file actions. Otherwise allow the request and show any provider rejection as a non-fatal error. A newly created fork does not copy old file-history snapshots, so inherited prompts in that fork support conversation restoration only; new prompts after the fork can create new file checkpoints.

### 4. Conversation restoration creates a prefix fork

Selecting user prompt `P` means returning to the state before `P` was sent. The session helper follows public Claude Agent SDK `fork_session` behavior:

- Take the active-chain prefix ending at `P.parentUuid`; if `P` is the first prompt, target an empty new session.
- Allocate a new session id, remap copied message UUIDs and parent references, and create a new JSONL through an atomic file operation.
- Do not copy file-history snapshots or undo history, and do not modify the original JSONL.
- Start or resume the new session with the same cwd, agent kind, and thread settings, then replay its prefix.
- Place the original text of `P` into the composer for editing or resubmission.

After a successful fork, Agent Pane increments a session epoch so late events from the old backend cannot update the new transcript. It clears the active turn, approval, command queue, compaction progress, and context usage, refreshes the provider command catalog, and retains cwd, model and permission settings, and the hidden-history-list state. The original session remains in history and can be restored in another tab.

Modifying the original JSONL could imitate truncation within one session, but it depends on a private persistence format, destroys the original branch, and is difficult to recover after a mid-operation failure, so this design does not use it.

### 5. The combined action restores files before creating the fork

`Restore files and conversation` first waits for `rewind_files(P.uuid)` to succeed on the original session id, then creates the fork ending at `P.parentUuid`. Reversing this order would fail because the fork remaps UUIDs and does not copy undo history, leaving the original checkpoint unavailable after switching.

If file restoration fails, do not start the fork and leave the original backend and conversation unchanged. If files restore but fork creation or new-process startup fails, report the partial result as "files restored, conversation unchanged" without claiming the files were rolled back. The original session remains available from history.

### 6. Session replacement and errors use an explicit state machine

Agent Pane stores one rewind operation whose states include selecting-checkpoint, selecting-action, rewinding-files, forking-conversation, and replacing-session. Disable the composer and duplicate rewind requests while the operation runs. Every asynchronous result carries a session epoch and operation id; stale results are logged without changing the current tab.

Present every expected failure as a local notice and return to an editable state: missing transcript, missing target message, expired checkpoint, rejected control request, failed fork-file creation, or failed new-backend startup. Error feedback does not count as a turn and is not grouped into provider conversation content.

## Risks / Trade-offs

- **[File restoration overwrites tracked workspace edits]** Label the scope in the action selector, permit it only while idle, and require the user to choose a file action explicitly in the second stage.
- **[Claude tracks only supported tools such as Write, Edit, and NotebookEdit]** Do not promise a complete workspace rollback. Use "files tracked by Claude" in success notices and convert provider errors into non-fatal feedback.
- **[Conversation forking still depends on Claude's persisted JSONL format]** Follow the public SDK helper's field behavior, preserve or skip unknown fields without guessing, read the original file only, create the new file atomically, and cover branches, compaction, and format extensions with samples.
- **[The combined action cannot be fully transactional]** Always restore files before conversation state, report partial success accurately, and retain the original session as a recovery path.
- **[Old sessions or old checkpoints may lack file snapshots]** Keep conversation restoration independently available. Disable known-unavailable file actions; otherwise let the provider decide and display failure.
- **[A forked historical prefix has no undo history]** Do not display unsupported file-restoration promises. New prompts in the fork establish new checkpoints.
- **[Choosing the wrong active leaf can hide valid history]** Choose the last valid leaf, follow the parent chain strictly, test multiple branches and missing parents, and never combine separate branches into one conversation.

## Migration Plan

1. Add active-chain parsing and replay tests first so existing resume behavior is deterministic for branched sessions.
2. Enable file checkpointing for new Claude processes and add a request-correlated control-response channel.
3. Add the fork helper, rewind state machine, and two-stage UI, then publish `/rewind` in the command catalog.
4. Existing JSONL requires no migration. Old sessions automatically gain conversation restoration, while file actions depend on existing snapshots.
5. To remove the feature, delete the command entry and checkpointing environment variable. Created forks remain ordinary Claude sessions that history can restore, so user data needs no deletion.

## Open Questions

- Publish only the canonical `/rewind` name initially. Evaluate `/checkpoint` and `/undo` aliases and a shortcut beside user messages after the core state machine is stable.
- During implementation, validate the error shape of `rewind_files` control responses against the oldest supported Claude Code version. If the protocol lacks support, keep conversation restoration available and mark file actions unavailable instead of using a hidden CLI argument or modifying raw JSONL.
