## Context

The Codex adapter owns one app-server process and one selected parent thread per Agent tab. Its session currently stores one `thread_id` and one `current_turn`, then applies every turn, item, and thread-status notification to that parent. Collaboration items are rendered as generic transcript items, so their child identities and lifecycle data are not retained for a status view.

The Claude Code adapter consumes stream-json output and replays JSONL session history. Live assistant and stream events with a non-null `parent_tool_use_id` are deliberately dropped so subagent content does not duplicate the parent Task row. History replay similarly excludes `isSidechain` records, and the hook adapter ignores `SubagentStop` because it must not stop the parent turn. These choices protect the parent conversation but also discard the child data needed by a background-task view.

The shell renders `GitSidebar` as its only right-side child. That component owns its open state, width, slide animation, resize handle, and Git content. A second independent sidebar would duplicate this behavior and could narrow the main pane twice.

The inspected Codex desktop app uses collaboration lifecycle items and descendant-thread loading for its Subagents view. The inspected Claude desktop app uses a broader `Background tasks` view driven by an incremental reduction of Task launches, task events, tool results, sidechain activity, and restored parent history. NiumaTerm will use the visible name `Background Tasks` while limiting this initial change to child-agent work.

## Goals / Non-Goals

**Goals:**

- Keep parent sessions immune to child turn, item, sidechain, and stop events.
- Maintain a provider-independent, testable task snapshot for Codex and Claude Code.
- Support Codex query-based recovery and Claude Code history-based recovery without stale data replacing newer live state.
- Reuse one right-side area for Git and `Background Tasks`, including width, animation, and resizing.
- Expose active counts and unseen lifecycle activity without polling either provider.
- Limit timer work to visible elapsed-time labels.

**Non-Goals:**

- Display or navigate a child transcript.
- Send messages, approvals, interrupts, resume requests, stop requests, or shutdown requests to a child.
- Show background shell commands, workflows, monitors, preview servers, scheduled work, or other non-agent tasks.
- Reproduce Claude's movable multi-pane or popout window system.
- Persist panel selection or width in application settings.
- Change provider spawning behavior.

## Decisions

### 1. Use provider-qualified task identities

`crates/agent_utils/src/chat.rs` will add `BackgroundTaskSummary`, `BackgroundTaskState`, `BackgroundTaskDiscoveryState`, and a session event carrying the latest snapshot. A `BackgroundTaskKey` will combine the provider with a provider-local stable ID so a child thread ID is not required for every provider.

A summary will contain:

- the provider-qualified key and parent session key;
- optional provider references, including Codex child and immediate-parent thread IDs or Claude task, tool-use, and linked parent-tool-use IDs;
- display name, agent type, objective, and latest short status when available;
- lifecycle state and a local increasing update sequence;
- start, update, and completion times when available;
- optional model, depth, and last child preview for later presentation needs.

Provider-specific reference data will use an enum instead of unrelated optional fields on the common model. The panel will depend only on the common fields and will not parse provider messages.

Requiring a child thread ID was rejected because Claude Task children are primarily associated through tool-use and task identifiers. Using an unqualified string ID was rejected because two providers can emit the same local value.

### 2. Route Codex thread notifications before parent state changes

`Session::process_notification` will extract the notification thread ID before invoking existing parent handling.

- A matching parent ID follows the current transcript, approval, compaction, and turn-state paths.
- A confirmed descendant ID updates the task registry and never changes `current_turn`, parent status, or parent transcript items.
- A non-parent ID that is not yet confirmed is held only as a bounded latest-state candidate. It can be applied after a collaboration item or descendant query confirms the relationship; otherwise it is discarded.
- A thread-scoped notification with no usable thread ID is logged and ignored. Notifications that are not scoped to a thread retain their existing handling.

Holding only the latest candidate avoids losing a child update that arrives before its spawn item while preventing unrelated thread content from entering the parent conversation. Applying all notifications to the parent was rejected because child completion can clear a still-running parent turn.

### 3. Reduce and restore Codex descendants in the Codex adapter

The Codex adapter will keep a registry keyed by provider-qualified child thread ID. It will parse `collabAgentToolCall` and `subAgentActivity` items as lifecycle input before applying ordinary parent transcript parsing. A collaboration call names its children through `receiverThreadIds` and reports each one's authoritative status in `agentsStates`; a thread's own runtime status describes only whether it is loaded and busy, so `idle` and `notLoaded` make no lifecycle claim of their own.

Descendant discovery requires the client to declare the experimental API capability during initialize, because `thread/list` rejects `ancestorThreadId` without it. Resumed turns are re-read for their collaboration items, since the descendant listing carries no outcome for a finished child. Confirmed child transcript items may update an optional preview but will not enter the parent transcript.

The reducer will map Codex states as defined in the specification. Task-scoped active flags from `thread/status/changed` will override the visible active label with Needs Input. Terminal states will not be locked because a later explicit resume can make a child active again.

Once a parent thread ID is known after start or resume, the adapter will request `thread/list` with:

- `ancestorThreadId` set to the parent thread ID;
- `sourceKinds` restricted to `subAgentThreadSpawn`;
- a bounded page size, following `nextCursor` until complete.

Each request will use a dynamically allocated RPC ID and capture the current live sequence. A returned row can create a missing summary and fill missing metadata, but it cannot replace lifecycle data updated after the request began. The adapter will retain immediate parent IDs, validate that parent chains reach the selected root, and reject cycles or rows outside that root.

Opening `Background Tasks` can request a refresh when no query is active; reconnection and parent resume also trigger loading. No periodic provider query is added. Overview loading will use thread metadata only because the first version does not display child transcripts.

### 4. Reduce Claude Code child activity before parent filtering

The Claude Code live session will own a task reducer keyed by provider-qualified task identity. Every incoming message will first be offered to that reducer; existing parent transcript processing and sidechain exclusion will run afterward. This preserves the current parent display while retaining task state.

The reducer will recognize, when present:

- parent assistant Task or Agent tool-use launches;
- matching parent tool results;
- `task_started`, `task_progress`, `task_notification`, and `task_updated` system records;
- assistant, reasoning, and tool activity with a linked `parent_tool_use_id`;
- `SubagentStop` hook data that contains a stable identifier matching a known task.

Lifecycle records describe every background task type, so only records naming
the delegated-agent task type create a row. Background shells, monitors, and
workflows share these records and stay out of this view; a record with no task
type can enrich a row a Task launch already created but never creates one.

Terminal state can arrive through either terminal record. A task stopped through
the CLI's own stop path reports `killed` in a `task_updated` patch and its
`task_notification` can be suppressed entirely.

Task IDs, tool-use IDs, and agent IDs can describe the same child at different protocol stages. The reducer will maintain bounded aliases only when a message supplies enough identifiers to establish the relationship. An unmatched hook event or sidechain record will be logged and ignored rather than assigned to the most recent task.

A Task launch will create a Starting row before detailed child activity arrives. Explicit lifecycle records take precedence over inferred state at the same or later sequence. Successful matched results can complete a task, error results can fail it, and later explicit activity can resume it.

An `init` record advances the process epoch, and a task still shown as running from an earlier epoch becomes Stopped: a new process cannot be running the children the previous one owned.

The CLI's `background_tasks_changed` snapshot is deliberately not used for this. A subagent is registered in the foreground and only flips to backgrounded later without a second `task_started`, so the snapshot omits child agents that are still working; narrowing against it would retire live children. It also spans shells, monitors, and workflows, so widening from it would admit work that is not a child agent.

Using only the parent Task tool row was rejected because background lifecycle events can continue after the launch and can report richer terminal causes. Passing linked child content through ordinary transcript processing was rejected because it would duplicate child text in the parent conversation.

### 5. Rebuild Claude Code tasks through a separate history pass

Claude history restoration will keep the current parent transcript selection unchanged and add a separate task-history pass.

The pass will first collect Task or Agent launches and matched results from the selected main history. It may then enrich those known tasks with sidechain records whose `parent_tool_use_id` links to a collected launch and with recognized lifecycle records associated with the selected session. A sidechain that cannot be linked to a selected parent launch will not create a task.

The restored snapshot will carry a history sequence boundary. It may create missing rows or fill missing metadata, but it will not replace a state updated live after restoration began. This mirrors the stale-response protection used for Codex while allowing Claude to restore from local JSONL rather than a descendant-thread query.

Scanning all sidechain records without a selected parent launch was rejected because abandoned branches and unrelated child traffic could otherwise appear in the current session.

### 6. Project task data into the active Agent pane

`AgentPaneSession` will store the latest common task snapshot, provider restoration state, activity ordinal, and stable parent session key. Applying a task snapshot event updates that state and emits an Agent pane notification. The shell's existing Agent-tab observation path will update the right-side target when the active pane or task state changes.

The `Background Tasks` component will target a parent session key and a weak Agent pane handle. Changing tabs clears row expansion and scroll state before the new snapshot is shown. Provider adapters remain the lifecycle owners; the shell will not keep a second mutable task registry.

### 7. Move shared behavior into one right-side host

The shell will own one right-side host with a selection such as `RightPanelKind::Git` or `RightPanelKind::BackgroundTasks`. The host will own open state, current width, slide animation, resize handling, outer card styling, and the single right-side layout slot.

`GitSidebar` will become Git content inside that host rather than owning the outer geometry. A button labeled `Background Tasks` will be placed at the upper-right of the application title bar alongside the existing title-bar controls. That button and the existing Git control will select their respective right-side content. Selecting the current view toggles the host closed; selecting the other view replaces content while retaining width.

The host will close `Background Tasks` when the active pane lacks a started or restored supported provider session. Leaving Git as an outer sidebar and adding a second sibling was rejected because it duplicates resize logic and permits two simultaneous right-side columns.

### 8. Track title-bar button activity per parent session

Each task snapshot will include an activity ordinal that advances when a task is created or changes lifecycle state. The shell will retain the last seen ordinal keyed by the active parent session for the current application lifetime. Opening `Background Tasks` records the current ordinal as seen.

The title-bar button will derive its active count from the current snapshot and show an unseen indicator when the current ordinal exceeds the seen ordinal. Metadata-only changes that do not affect lifecycle state will update rows without producing repeated unseen indicators.

Keeping seen state globally was rejected because opening one tab could hide new activity in another tab. Persisting seen state was rejected for the first version because task snapshots themselves are restored and a stale persisted marker would add little value.

### 9. Keep presentation bounded and timers local

The panel title and upper-right title-bar button will use the exact text `Background Tasks`. The panel will show Running before Finished, order running rows by earliest known start time with update sequence as a fallback, and order terminal rows by most recent completion.

The default compact view will show up to four running rows and ten terminal rows, with section controls for additional rows. Rows are read-only and use theme semantics for state emphasis.

A single one-second repaint task runs only while `Background Tasks` is open and at least one active row has a start time. It recalculates labels from stored times without querying either provider or mutating lifecycle state. Closing the panel, switching its content, or reaching zero active timed rows stops the task.

### 10. Represent restoration progress separately from row data

Agent state will distinguish not loaded, loading, ready, and unavailable restoration states. Codex updates this state around descendant queries; Claude Code updates it around session-history loading. An initial failure with no rows displays a non-blocking unavailable message. A refresh failure retains existing rows and records the error without replacing a newer ready snapshot. The parent composer and transcript do not depend on restoration success.

## Risks / Trade-offs

- [Provider fields or state names change] → Parse known fields defensively, preserve identified children with a neutral label, and log unrecognized values without changing parent state.
- [Claude identifiers arrive in separate records] → Keep bounded aliases only after a record establishes their relationship; never guess from recency alone.
- [Claude sidechain history belongs to another branch] → Require a link to a Task or Agent launch in the selected main history before accepting it.
- [A process snapshot is incomplete during startup] → Wait for the first authoritative active-set record before stopping tasks missing from later snapshots.
- [A delayed restore result overwrites live state] → Associate restoration with a starting sequence and limit older results to missing rows or metadata.
- [A tab switch leaves prior task rows or indicators visible] → Key panel and seen state by parent session and clear transient view state when the active target changes.
- [Elapsed-time updates cause unnecessary rendering] → Run one timer only while the visible view contains active timed rows.
- [Changing Git geometry alters current behavior] → Move outer sizing and animation while retaining Git content tests and visual behavior, then add the second selection.

## Migration Plan

1. Add common task models and task snapshot events without exposing new UI.
2. Add Codex lifecycle reduction, thread routing, descendant loading, and regression tests.
3. Add Claude Code live reduction, linked sidechain handling, history restoration, and regression tests.
4. Project provider snapshots into Agent panes and add observer tests.
5. Extract the shared right-side host while preserving Git behavior.
6. Add the upper-right title-bar button, `Background Tasks` content, button signals, grouping, empty and unavailable states, and elapsed-time task.
7. Validate both providers in isolated `--testing` application launches before considering the view complete.

No persisted schema migration is needed. Rollback removes the `Background Tasks` selection and common task projection. The Codex parent-routing guard and Claude child-before-filter reduction can remain because they prevent child activity from changing the parent session even without the panel.
