## Why

`Background Tasks` reports what a child agent is doing in one truncated line behind a hover tooltip, so the only way to read a child's actual work is to leave the panel. The child transcripts already exist — Codex stores each descendant as its own thread and Claude Code writes linked sidechain records — but nothing presents them. Showing them in the panel is only worth doing if they read exactly like the parent conversation, and today the transcript presentation is written directly against `AgentPane`, so a second renderer would either duplicate it or drift from it.

## What Changes

- Make each `Background Tasks` row open a detail view for that child agent, replacing the hover tooltip as the way to read its work.
- Render the child transcript with the same presentation the Agent pane uses: identical row kinds, work-log folding, disclosure behavior, code virtualization, and timestamps.
- Extract the Agent pane's transcript presentation into a shared component that both the parent conversation and the child detail view render, so the two cannot diverge.
- Load a Codex child transcript by reading the descendant thread without resuming it, and a Claude Code child transcript from the sidechain records linked to its launch, retaining live linked activity instead of discarding it after the preview.
- Add back navigation from the detail view to the task list, and keep the detail view scoped to its parent session so switching tabs cannot leave another conversation's child on screen.
- **BREAKING** (spec-level): the `Background Tasks` view stops being strictly read-only — opening a child transcript becomes allowed, while sending input, approvals, interrupts, resume, and stop remain out of scope.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `subagent-status-panel`: rows become an entry point to a child transcript rather than tooltip-only status; the read-only requirement is narrowed to exclude child *operations* while permitting child *inspection*; new requirements cover the detail view, its transcript presentation matching the parent, per-provider transcript loading, and its failure and navigation behavior.
- `agent-transcript-visual-hierarchy`: the presentation rules become a property of a shared transcript component applied to any agent conversation, rather than of the Agent pane specifically, so a child transcript is covered by the same requirements.

## Impact

- Agent pane transcript presentation in `crates/app/src/agent_pane/transcript/` and its render entry points in `crates/app/src/agent_pane/view/`, which move behind a shared component that owns transcript rows, expansion state, virtualized code, and list geometry.
- `crates/app/src/agent_pane/mod.rs` and `session/`, which delegate transcript state to that component instead of owning it directly.
- `crates/app/src/ui/background_tasks/`, which gains row activation, detail rendering, and back navigation.
- Codex descendant reading in `crates/agent_utils/src/codex/app_server/`, reusing the existing turn/item parser against `thread/read`.
- Claude Code child-activity retention in `crates/agent_utils/src/claude_code/tasks/` and child transcript reconstruction in `crates/agent_utils/src/claude_code/sessions/`.
- Common child-task models in `crates/agent_utils/src/background_task/`, which gain a transcript-loading state alongside the existing summary.
- No new external dependency and no persisted-data migration. Bounded additional memory: a child transcript is retained only while its detail view is the visible content.
