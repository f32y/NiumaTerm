## Context

See proposal.md — Why. The findings below come from reading Claude Code 2.1.228 and from one instrumented headless run (`--output-format stream-json --verbose`) of a two-agent workflow; they are what the approach is built on.

**A run is one task, not many.** The provider registers a workflow as a single task with `task_type: "local_workflow"`, carrying `task_id` and `workflow_name`. Its agents are never separate tasks — they live as entries in a `workflow_progress` array hanging off that one task. `crates/agent_utils/src/claude_code/tasks/mod.rs` rejects `local_workflow` at its first gate, and `tasks/tests.rs::only_agent_work_becomes_a_row` locks that in. That exclusion stays; this change reads the same records for a different view.

**The stream carries state, never conversation.** `task_progress` records repeat the full `workflow_progress` array. Two entry kinds appear:

- `{type: "workflow_phase", index, title}`
- `{type: "workflow_agent", index, label, phaseIndex, phaseTitle, agentId, agentType, isolation, model, state, queuedAt, startedAt, lastProgressAt, promptPreview, resultPreview, tokens, toolCalls, cached, blocked, error}` where `state` is `start`, `done`, or `error`.

The observed run produced no `parent_tool_use_id` on any message and no sidechain records, so the sidechain path that feeds ordinary child agents (`tasks/mod.rs::observe_sidechain`) never fires for workflow agents. `task_progress` also omits `task_type` entirely — only `task_started` carries it.

**Conversations are on disk, in the format already parsed.** Each agent's transcript is persisted at
`~/.claude/projects/<encoded-cwd>/<session-uuid>/subagents/workflows/wf_<runid>/agent-<agentId>.jsonl`,
beside `agent-<agentId>.meta.json` and a run-level `journal.jsonl`. Field-set comparison against the ordinary subagent transcripts the app already reads shows them structurally identical (`isSidechain: true` on every record, same key set). `sessions/replay.rs::parse_child_replay` filters on `isSidechain == true` and therefore parses these files unchanged.

**Two identifiers do not link.** The workflow's `meta.json` is only `{"agentType":"workflow-subagent","spawnDepth":1}` — it has no `toolUseId`, so `sessions/task_history.rs::attach_child_transcripts`, which keys on exactly that field, cannot be reused as-is. Separately, the run directory name (`wf_36e84f35-a64`) never appears on the stream; the stream's `task_id` (`wacob045a`) is a different identifier.

## Goals / Non-Goals

**Goals:**

- One reduction path that turns `local_workflow` lifecycle records into a run model, without touching the child-agent reducer or its exclusion.
- Bounded, predictable disk IO for the one-second refresh, independent of how many agents a run has.
- Reuse the existing transcript parsing and rendering so a workflow agent conversation needs no second presentation layer.

**Non-Goals:**

- Interacting with a run — no stopping, skipping, retrying, or answering an agent. The view is read-only.
- Rendering the workflow's own script, journal internals, or token accounting beyond the totals the provider reports.
- Any support for Codex, which has no workflow concept.
- Streaming a workflow agent's conversation live over the provider protocol; the provider does not emit it, and this change does not try to reconstruct it from anything other than the persisted file.

## Decisions

### Reduce workflows in a sibling module, not in the child-agent reducer

A new `crates/agent_utils/src/claude_code/workflows/` module owns the run model and a reducer with the same `observe(&Value) -> bool` shape `ClaudeTasks` has, called from the same place in `stream_json::Session::process`.

*Why:* the two views answer different questions from overlapping records, and `tasks` carries an explicit, tested rule that workflows stay out of it. Widening `ClaudeTasks` to carry both would put the exclusion and the inclusion of the same `task_type` in one reducer. *Alternative rejected:* a shared reducer emitting two snapshot kinds — it couples two independent views' retention and identity rules for no reuse beyond a few field readers, which are small enough to duplicate or lift into a shared helper.

### Take state from the stream, conversation from disk

Run identity, phases, agent list, and every per-agent field come from `workflow_progress` on the stream. Conversation content comes only from the persisted transcript. Neither source substitutes for the other: the stream never carries conversation, and the on-disk record does not carry the provider's own aggregate totals.

### Poll the journal plus the open conversation, not every transcript

Each one-second tick, for each non-terminal run of the scoped session, reads:

1. the run's `journal.jsonl` — small (hundreds of bytes), giving `{type: "started"|"result", agentId, result}` per agent;
2. the transcript of the **currently open** agent conversation, if one is open.

*Why:* it bounds a tick to one small file plus at most one transcript, regardless of agent count. *Alternative rejected:* re-reading every agent transcript each tick — the observed trivial agents were already 18.9 KB each and real ones reach hundreds of KB, so an N-agent run would re-read and re-parse N large files per second on the UI's behalf.

The journal is what makes the one-second cadence worth having: `task_progress` records arrive only when the provider chooses to emit them, so an agent that finishes during a quiet stretch is reflected within a second instead of waiting for the next stream record.

### Locate the run directory by agent id

The run directory is found by globbing `<session>/subagents/workflows/wf_*/` for a directory containing `agent-<agentId>.jsonl` for any `agentId` the run's `workflow_progress` reported. Agent ids are unique, so this is an exact match.

*Why:* the stream gives no run-directory identifier. *Fallback:* a run announced before any agent has an id resolves no directory and simply has nothing to poll until its first agent appears — its stream-reported state still displays. *Alternative rejected:* picking the newest directory by modification time, which misattributes when two runs overlap.

### Read transcripts whole, gated on file growth

A transcript is re-read and re-parsed only when its size has grown since the previous tick. `parse_child_replay` walks a `parentUuid` chain and has no resumable form, so incremental parsing would mean a second parser to maintain.

*Trade-off:* a long conversation is re-parsed on each growth. It is bounded to the one open conversation, and the parse is already done once per restoration today.

### Own the model in the pane, render in a `ui/` view

`AgentPane` holds the workflow model and the refresh task; a new `WorkflowsView` in `crates/app/src/ui/` renders published snapshots. This mirrors how `BackgroundTasksView` is fed today, so session scoping, epoch handling, and clearing on session switch follow one existing pattern.

`RightPanelKind` gains a third variant. `RightPanelSelection::select` is already written against an arbitrary kind, so the shared-area behavior the modified requirement describes needs no new logic.

### Drive the refresh from view visibility

The refresh task starts when `Workflows` becomes the selected right-side content with a non-terminal run present, and stops when the view is hidden or every run reaches a terminal state. Ticks do not overlap: a tick still running when the next is due skips that beat rather than queueing.

*Why:* polling a file every second for a view nobody is looking at is pure cost, and the spec requires refreshing to stop in both cases.

### Restore runs from the per-run completion snapshot, not the session transcript

On completing a run the provider writes `<session-uuid>/workflows/<runId>.json`, carrying `runId`, `taskId`, `workflowName`, `summary`, `status`, `startTime`, `durationMs`, `phases`, the full `workflowProgress` array, `result`, `logs`, `agentCount`, `totalTokens`, and `totalToolCalls`. Restoration lists that directory and rebuilds each run from one file; `subagents/workflows/<runId>/` then supplies the agent transcripts. Restored runs are terminal by definition — a run of a previous process cannot still be advancing.

*Why not the session transcript:* the `local_workflow` lifecycle records are **not** persisted to `<session-uuid>.jsonl`. Inspecting a captured session file shows its only `system` record is `stop_hook_summary`; the run survives there solely as the Workflow tool's `tool_result`. Scanning it the way `task_history.rs` scans for child-agent launches would restore nothing.

### Restore interrupted runs from the directory alone

The completion snapshot only exists for a run that ended cleanly. On the machine this was built against, one of the two recorded runs has none: a long research run the process outlived. Those are exactly the runs worth reopening, so restoration also scans `subagents/workflows/` for directories no snapshot claimed and rebuilds each from what is there — the run id from the directory name, the workflow name from the script copy at `<session>/workflows/scripts/<name>-<run-id>.js`, and one agent per persisted transcript.

Such a run is reported Stopped, and its agents carry a new `Stopped` state meaning the run ended before they did. Two sources settle those rows: the transcripts say which agents ran, the journal says which finished. The journal alone would not do — it is keyed by prompt hash and records only the agents whose results were cached, covering 28 of the 121 agents in the captured run.

Agent labels come from the opening line of each agent's own prompt, read from the head of its transcript. *Why not the agent id:* a list of opaque ids is unreadable at this size, and the prompt's first line is what its author wrote to identify it. Only the first few lines of each file are scanned, so the cost stays proportional to the agent count rather than to transcript size.

Everything the snapshot alone carries — phases, ordering, per-agent models and accounting, the run's own result — is omitted rather than guessed.

*This file is written once the run ends, so it says nothing about a live one.* In the captured run its mtime lands 3 ms after the run's own `startTime + durationMs` window closes, so it cannot serve the one-second refresh — which is why live polling reads `journal.jsonl` instead. It does, however, give the only direct `taskId` → `runId` mapping, so once a run has finished, its directory resolves from this file rather than from the agent-id glob.

## Risks / Trade-offs

- **The `workflow_progress` shape is undocumented provider internals** → Every field is read defensively: a run displays from `task_id` and `workflow_name` alone, and each per-agent detail is omitted when absent, which the spec already requires. A shape change degrades detail rather than breaking the view.
- **One-second disk polling on every tick of an active run** → Bounded to one small journal plus at most one growing transcript, and gated on both view visibility and run activity. Reads run off the UI thread.
- **A partially written JSONL line during a poll** → Parsing tolerates a truncated final line by using the records read so far, and the next tick picks up the rest. No state is marked failed because of a read.
- **Run-directory resolution depends on agent ids appearing** → A run with no agents yet polls nothing; its stream-reported state still displays, and the directory resolves as soon as the first agent is reported.
- **Two overlapping runs in one session** → Each resolves its own directory through its own agent ids, so neither can claim the other's transcripts.
- **Provider drops or renames `local_workflow`** → The view reports no runs; the child-agent view and the parent conversation are unaffected, because nothing in this change touches their paths.

## Open Questions

- Whether run-level totals should also be shown when the view is scrolled to an open agent conversation, or only on the run list. This affects layout only and can be settled while building the view.
