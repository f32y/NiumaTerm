## 1. Workflow model and stream reduction

- [x] 1.1 Add a `workflows` module under `crates/agent_utils/src/claude_code/` with the run model: run identity (`task_id`, `workflow_name`), lifecycle state, phase list, per-agent rows, and run-level totals, mirroring how `background_task` separates its shared model from the provider reducer.
- [x] 1.2 Define the per-agent row: `agent_id`, `label`, `index`, phase index and title, `agent_type`, `isolation`, `model`, state, token count, tool-call count, reused flag, error text, and prompt/result previews — every provider-sourced field optional so a missing one is omitted rather than defaulted.
- [x] 1.3 Map the provider's agent `state` values (`start`, `done`, `error`) onto the spec's Queued / Running / Done / Failed vocabulary, treating an entry with a `queuedAt` but no `startedAt` as Queued.
- [x] 1.4 Implement `observe(&Value) -> bool` recognising `task_started` with `task_type: "local_workflow"` as a run, and `task_progress` / `task_updated` / `task_notification` carrying a known `task_id` as updates — remembering that only `task_started` carries `task_type`, so later records must match on `task_id` alone.
- [x] 1.5 Parse `workflow_progress` into phases and agent rows, replacing the run's rows on each record since the provider repeats the full array.
- [x] 1.6 Map run lifecycle status onto Starting / Running / Done / Failed / Stopped, and capture the run's final result or error text on completion.
- [x] 1.7 Publish a replacement run snapshot for the session, and expose it the way `ClaudeTasks::snapshot` does.
- [x] 1.8 Call the reducer from `stream_json::Session::process` beside the existing `tasks.observe`, leaving `ClaudeTasks` and its `local_workflow` exclusion untouched.
- [x] 1.9 Unit-test the reducer against captured records: a run announced with no agents, phases plus agents arriving, an agent failing with error text, a reused agent, and a `task_progress` with no `task_type` still matching its run.
- [x] 1.10 Add a regression test asserting a `local_workflow` run still produces no `Background Tasks` row.

## 2. On-disk run reading

- [x] 2.1 Add a run-directory resolver that first reads `<session-uuid>/workflows/<runId>.json` for a `taskId` match, then falls back to globbing `<session-uuid>/subagents/workflows/wf_*/` for the directory containing `agent-<agent_id>.jsonl` for any agent id the run reported — the completion snapshot does not exist while a run is still in progress. Return nothing when neither resolves.
- [x] 2.2 Add a `journal.jsonl` reader returning per-agent `started` / `result` entries keyed by `agentId`, tolerating a truncated final line by using the records read so far.
- [x] 2.3 Add a transcript reader that returns `Vec<Item>` for one `agent-<agent_id>.jsonl` via the existing `parse_child_replay`, with no change to that function.
- [x] 2.4 Track each read file's last-seen size so a transcript is re-parsed only after it has grown.
- [x] 2.5 Report a read failure as "cannot refresh" for that run without dropping known rows and without marking any run or agent as failed.
- [x] 2.6 Unit-test the readers against the captured fixtures under `~/.claude/projects/C--Users-yuhaonan-AppData-Local-Temp-wf-probe/`, copying the two real agent transcripts and journal into the repo's test fixtures first.

## 3. Session plumbing

- [x] 3.1 Add chat events carrying the workflow run snapshot and a per-agent transcript update, beside the existing `BackgroundTasks` / `BackgroundTaskTranscript` events.
- [x] 3.2 Emit the snapshot event from `stream_json::Session::process` whenever the reducer reports a change.
- [x] 3.3 Add session methods to resolve a run directory, poll a journal, and read one agent transcript, and dispatch them through `Backend` with Codex as a no-op.

## 4. Pane state and refresh loop

- [x] 4.1 Hold the workflow run model and the open-conversation selection on `AgentPane`, clearing both on session switch and on epoch change.
- [x] 4.2 Apply the snapshot and transcript events to that state in `session/events.rs`.
- [x] 4.3 Add a one-second refresh task that reads each non-terminal run's journal plus the open agent transcript, running its IO off the UI thread.
- [x] 4.4 Start the task when `Workflows` becomes the visible right-side content with a non-terminal run present; stop it when the view is hidden or every run reaches a terminal state.
- [x] 4.5 Skip a beat rather than queueing when the previous tick is still running, and drop a completed tick's result when the session epoch has since changed.
- [ ] 4.6 Test the start/stop conditions and the non-overlap rule.

## 5. Workflows view

- [x] 5.1 Add `WorkflowsView` under `crates/app/src/ui/`, rendering published snapshots the way `BackgroundTasksView` does.
- [x] 5.2 Render the run list: workflow name, lifecycle state, phase titles in provider order, agent count, aggregate tokens, aggregate tool calls, and the final result or error when terminal.
- [x] 5.3 Render agent rows grouped under their phase, showing label and state, plus agent type, model, tokens, and tool calls when reported; mark reused agents and show error text on failed ones.
- [x] 5.4 Report "no session" when the active pane has no Claude Code session, and keep the area open when the active pane changes.
- [x] 5.5 Make an agent row open its conversation, and provide a way back to the run's agent list.
- [x] 5.6 Render an open conversation through the existing transcript presentation, with no composer or other input affordance.
- [x] 5.7 Report that a conversation is not available when the agent has no persisted transcript, keeping the row listed.
- [ ] 5.8 Test the empty, no-session, running, and terminal renderings, and the row-to-conversation-and-back navigation.

## 6. Right-side area integration

- [x] 6.1 Add a `Workflows` variant to `RightPanelKind` and host `WorkflowsView` in `RightPanel` beside Git and `Background Tasks`.
- [x] 6.2 Add the action and title-bar affordance that selects the view, matching how `Background Tasks` is selected.
- [x] 6.4 Keep the control out of the title bar until a run exists, keep it once shown, and report the active tab's running-agent count beside its icon. Drive it from a pane event rather than observing every pane repaint.
- [x] 6.3 Verify selecting any of the three views replaces the visible one at the current width without opening a second column, and extend the right-panel tests to cover the third variant.

## 7. Restoration for resumed sessions

- [x] 7.1 Rebuild a resumed session's runs by listing `<session-uuid>/workflows/*.json` and reading each completion snapshot, marking every restored run terminal. Do not scan `<session-uuid>.jsonl`: the `local_workflow` lifecycle records are not persisted there.
- [x] 7.2 Attach restored agent transcripts from `<session-uuid>/subagents/workflows/<runId>/`.
- [x] 7.5 Restore runs whose completion snapshot never landed by scanning `subagents/workflows/` for unclaimed directories: run id from the directory, workflow name from the script copy, one agent per persisted transcript labelled from its prompt's opening line, Done when the journal recorded a result and a new `Stopped` state otherwise.
- [x] 7.6 Trigger restoration when the session becomes ready rather than when the view opens, so a resumed session can reveal the title-bar control at all.
- [x] 7.3 Fold restoration into the live model without letting an older read replace newer live state, following the `merge_restored` sequence rule.
- [x] 7.4 Test that a resumed session lists its earlier runs and that restoration failure leaves the session usable.

## 8. Localization

- [x] 8.1 Add every new label to `crates/i18n/locales/en.toml` and `crates/i18n/locales/zh-CN.toml`: view title, run and agent state names, phase and totals labels, reused marker, no-session message, conversation-unavailable message, and the refresh-failure message.

## 9. Verification

- [x] 9.1 Run `cargo fmt`, `cargo clippy --all-targets`, and the workspace tests.
- [ ] 9.2 Drive a real multi-agent workflow in a Claude Code tab launched with `--testing` and confirm against the spec scenarios: the run appears before its first agent, agent states advance, an open conversation extends while its agent runs, refreshing stops when the run finishes and when the view is hidden, and the parent transcript stays free of workflow agent content.
- [ ] 9.3 Confirm `Background Tasks` still shows no workflow rows during and after that run.
