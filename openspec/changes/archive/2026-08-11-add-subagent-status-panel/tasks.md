## 1. Common Background Task Model

- [x] 1.1 Add provider-qualified task keys, provider reference variants, shared lifecycle state, restoration state, task summary, snapshot, and session event types in `agent_utils`.
- [x] 1.2 Add update sequence, activity ordinal, active count, needs-input count, and stable parent session key fields needed by the Agent pane and title-bar `Background Tasks` button.
- [x] 1.3 Add unit tests for provider-qualified identity, optional metadata, lifecycle grouping, task resumption, activity ordinal changes, and unchanged lifecycle updates.

## 2. Codex Lifecycle and Descendant Recovery

- [x] 2.1 Parse `collabAgentToolCall` and `subAgentActivity` items into Codex task registry updates while preserving the existing parent collaboration transcript item behavior.
- [x] 2.2 Extract thread IDs from Codex turn, item, and thread-status notifications and route matching parent, confirmed descendant, and unrelated notifications through separate paths.
- [x] 2.3 Add bounded pending updates for not-yet-confirmed child IDs and test relationship confirmation, unrelated update removal, explicit child resume, and live event ordering.
- [x] 2.4 Add regression tests proving child turn start, completion, and transcript items do not alter the parent turn ID, running state, approval state, or transcript.
- [x] 2.5 Add dynamically tracked descendant `thread/list` requests using `ancestorThreadId`, `subAgentThreadSpawn`, and cursor pagination without changing history-list request handling.
- [x] 2.6 Parse descendant metadata, retain immediate parent IDs, reject cycles and rows outside the selected root, and merge results by provider-qualified child ID.
- [x] 2.7 Protect live Codex updates from older query responses with a per-query starting sequence, including delayed and paginated response tests.
- [x] 2.8 Trigger Codex descendant loading after parent start, resume, reconnect, and a guarded panel refresh request while retaining known rows after failure.

## 3. Claude Code Live Child Reduction

- [x] 3.1 Add a Claude Code task reducer that observes each incoming message before existing parent transcript handling and sidechain exclusion.
- [x] 3.2 Parse parent Task or Agent tool-use launches into Starting rows using the tool-use ID, description, prompt, and agent type when available.
- [x] 3.3 Parse matched parent tool results into Done or Failed updates without changing the existing parent Task transcript row.
- [x] 3.4 Parse `task_started`, `task_progress`, `task_notification`, and `task_updated` records into shared lifecycle and metadata updates, restricted to delegated-agent task types.
- [x] 3.5 Keep ordinary user messages and non-child tool results unchanged, and never create a row from a background shell, monitor, or workflow record.
- [x] 3.6 Associate sidechain assistant, reasoning, and tool activity with known Task launches through `parent_tool_use_id` before dropping that content from the parent transcript.
- [x] 3.7 Add bounded aliases for task, tool-use, and agent identifiers only when a Claude record explicitly associates them, and ignore unmatched sidechain or hook activity.
- [x] 3.8 Track Claude process epochs so a task left running by an earlier process becomes Stopped, without narrowing against the live background-task snapshot.
- [x] 3.9 Convert a matched `SubagentStop` hook into a child lifecycle update without changing the parent turn state, and retain the current ignore behavior when no stable match exists.
- [x] 3.10 Add live-stream tests for launch-before-activity, progress, success, failure, stop, explicit resume, task-scoped Needs Input, parent-only approval, alias matching, unmatched activity, and process restart.

## 4. Claude Code History Restoration

- [x] 4.1 Add a task-history pass that collects Task or Agent launches and matched results from the selected main Claude Code history without changing parent transcript replay.
- [x] 4.2 Enrich restored tasks only with sidechain records whose `parent_tool_use_id` links to a collected launch, excluding abandoned or unrelated sidechains.
- [x] 4.3 Restore recognized Claude lifecycle records for the selected session and map unfinished tasks at a confirmed process boundary to Stopped.
- [x] 4.4 Merge restored Claude rows with live state using a starting sequence so older history can create missing rows or metadata but cannot replace newer lifecycle state.
- [x] 4.5 Report not-loaded, loading, ready, and unavailable Claude restoration states while retaining known live rows after history-read or parse failure.
- [x] 4.6 Add history tests for completed tasks, failed tasks, nested linked activity, missing optional metadata, unmatched sidechains, interrupted history, stale restoration, and unreadable history.

## 5. Agent Pane Projection

- [x] 5.1 Store task snapshots, restoration state, activity ordinal, and parent session key in the Agent pane session for both Codex and Claude Code.
- [x] 5.2 Apply task snapshot events and notify observers without changing parent composer, transcript, approval, queued-command, or running-state behavior.
- [x] 5.3 Expose read-only accessors for the active parent session and clear provider task state when the pane starts or restores a different session.
- [x] 5.4 Add Agent pane tests for snapshot replacement, retained rows after restoration failure, activity ordinal propagation, and switching among Codex, Claude Code, and unsupported panes.

## 6. Shared Right-Side Host

- [x] 6.1 Move open state, width, slide animation, resize handling, and outer card rendering from `GitSidebar` into one reusable right-side host while preserving Git behavior.
- [x] 6.2 Add a `Background Tasks` button at the upper-right of the application title bar and connect it with the existing Git control so choosing one view replaces the other at the current width and selecting the current view closes the host.
- [x] 6.3 Connect the host to the active Agent pane and close `Background Tasks` when the active pane lacks a started or restored supported provider session.
- [x] 6.4 Add shell tests proving Git and `Background Tasks` never render as two columns and stale rows are cleared after another parent or unsupported pane becomes active.

## 7. Background Tasks View and Title-Bar Button

- [x] 7.1 Add the read-only `Background Tasks` component with Running and Finished sections, active and terminal counts, needs-input count, empty state, and unavailable state.
- [x] 7.2 Render provider, stable identity, display-name fallback, objective or status fallback, lifecycle label, elapsed time, and relative completion time for each row.
- [x] 7.3 Order running rows by earliest known start and finished rows by latest completion, show four running and ten finished rows by default, and add section controls for further rows.
- [x] 7.4 Show the current active count on the title-bar `Background Tasks` button and a parent-session-scoped unseen indicator for task creation and lifecycle changes while the view is closed.
- [x] 7.5 Mark the current parent session's activity ordinal as seen when the view opens without clearing unseen activity for another session.
- [x] 7.6 Add one local elapsed-time repaint task that runs only while `Background Tasks` is open with active timed rows.
- [x] 7.7 Add tooltips and accessible labels for the title-bar button, activity indicator, section controls, lifecycle states, and truncated row text without adding child operation controls.
- [x] 7.8 Add view tests for both providers, mixed states, failed and stopped children, missing metadata, compact limits, timer shutdown, title-bar button signals and disabled state, session switching, and read-only row behavior.

## 8. Validation

- [x] 8.1 Run formatting, workspace lint checks, and focused Rust tests for common task models, the Codex adapter, the Claude Code adapter, Agent pane, shell, Git view, and `Background Tasks` view.
- [x] 8.2 Launch `target\debug\NiumaTerm.exe --testing` and manually validate Codex live spawning, needs-input state, completion, failure, restored descendants, and reconnect recovery.
- [x] 8.3 In the same isolated test mode, manually validate Claude Code Task launch, linked sidechain activity, completion, failure, process restart, restored history, and parent transcript isolation.
- [x] 8.4 Manually validate active counts, unseen activity, parent-session switching, Git replacement, resizing, empty state, and restoration failure for both providers.
