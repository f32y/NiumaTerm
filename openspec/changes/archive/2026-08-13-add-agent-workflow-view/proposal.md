## Why

Claude Code's Dynamic Workflow runs many agents across phases in a single tool call, and a run can last many minutes and consume a large share of a session's budget. NiumaTerm currently shows none of it: the workflow registers as one task of type `local_workflow`, which the `Background Tasks` reducer rejects by design, and the individual workflow agents never emit child-agent records at all. The user is left watching a single silent tool row with no agent list, no phase progress, and no way to read what any agent produced.

The data already exists. The CLI streams a per-agent progress array on every `task_progress` record, and each workflow agent's full conversation is persisted to disk in the same format the app already parses for ordinary child agents.

## What Changes

- Add a `Workflows` view to the existing right-side area, alongside Git and `Background Tasks`. Workflow runs stay out of `Background Tasks`, whose child-agent scope is unchanged.
- Reduce the Claude Code stream's `local_workflow` lifecycle records into a workflow run model: run identity, name, status, phase list, and one row per workflow agent with its label, phase, agent type, model, state, token count, and tool-call count.
- Poll each active run's on-disk workflow directory once per second so agent state and conversation content advance while the run is in progress, rather than only after it ends. Polling stops when the run reaches a terminal state.
- Let the user open one workflow agent's own conversation from its row, rendered with the same transcript presentation the parent conversation and child-agent detail views already use.
- Show run-level totals (agent count, aggregate tokens, aggregate tool calls) and the workflow's final result or error when it finishes.
- Localize every new label in `en` and `zh-CN`.

## Capabilities

### New Capabilities

- `agent-workflow-view`: A right-side view of Claude Code Dynamic Workflow runs for the active Agent tab, covering run and phase status, the per-agent list with live progress, one-second polling of the run's on-disk transcript directory, and per-agent conversation reading.

### Modified Capabilities

- `subagent-status-panel`: The `Share one right-side area` requirement currently names Git and `Background Tasks` as the only two views selecting that area. It changes to admit a third selectable view so `Workflows` participates in the same single-column, shared-width behavior.

## Impact

- `crates/agent_utils/src/claude_code/`: a new workflow reduction module beside the existing `tasks` reducer, plus a disk reader for `subagents/workflows/wf_*/`. The existing `parse_child_replay` is reused unchanged; the `tasks` reducer and its `local_workflow` exclusion are untouched.
- `crates/agent_utils/src/chat.rs`: new events carrying the workflow snapshot and per-agent transcript updates.
- `crates/app/src/agent_pane/`: workflow state on the pane, the polling task, and the new view; the right-side area gains a third selectable view.
- `crates/i18n/locales/`: new keys in `en.toml` and `zh-CN.toml`.
- No provider launch flags change, and no existing `Background Tasks` behavior changes.
