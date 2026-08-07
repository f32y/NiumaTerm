## Why

Claude Agent Tab can restore an entire historical session but cannot return to the state before a selected user prompt as the interactive Claude Code terminal can. Sending `/rewind` as an ordinary provider command cannot represent its interactive checkpoint menu. NiumaTerm needs a native, recoverable rewind flow built on the stream-json integration, with a clear distinction between conversation restoration and file restoration and the risks of each.

## What Changes

- Add an idle-only `/rewind` local interactive command to Claude Agent Tab, using a two-stage selector for the target user prompt and restore action.
- Support three actions: restore files only, restore conversation only, and restore both files and conversation. The combined action restores files before switching to the shortened conversation branch.
- Enable SDK file checkpointing for newly started Claude stream-json sessions and restore supported file edits through a request-correlated `rewind_files` control request.
- Restore conversations by creating an independent fork that preserves the original session, replaying the active message chain before the selected prompt, and placing the removed prompt back into the composer for editing or resubmission.
- Reconstruct the active Claude history chain through `parentUuid` so restore and rewind omit abandoned branches.
- Show a clear unavailable state or non-fatal error when no file checkpoint exists, a checkpoint expired, or the provider rejects restoration. Never report conversation restoration as file restoration.
- Do not support Claude Code's summarize-from-here or summarize-to-here actions initially, and do not modify the original Claude JSONL session file directly.

## Capabilities

### New Capabilities

- `claude-session-rewind`: Claude checkpoint discovery, target and action selection, file restoration, conversation forking, combined actions, failure recovery, and compatibility limits.

### Modified Capabilities

- `agent-slash-commands`: add `/rewind` to the command catalog, state policy, and command feedback as a Claude-only local interactive command in NiumaTerm.
- `agent-session-history`: replay only the current active message chain identified through `parentUuid` and support restoration of a fork created by rewind.

## Impact

- Main areas: `crates/agent_utils/src/claude_code/stream_json.rs`, `crates/agent_utils/src/claude_code/sessions.rs`, shared agent event and command models, and command routing, selectors, and session-replacement state in `crates/app/src/ui/agent_pane.rs`.
- Enable SDK file checkpointing in the Claude child-process environment. Extend the control protocol with request-id correlation and success or failure events for `rewind_files`.
- Conversation restoration creates a new Claude session id and keeps the original session as restorable history. File restoration does not branch the working directory and cannot cover Bash, external-process, or other writes that Claude SDK does not track.
- Do not change Codex Agent Tab command or session behavior, and add no third-party runtime dependency.
