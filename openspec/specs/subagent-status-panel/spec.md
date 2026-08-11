# subagent-status-panel

## Purpose

Provide a reliable read-only `Background Tasks` view of Codex and Claude Code child-agent progress while keeping child activity isolated from the parent conversation.

## Requirements

### Requirement: Offer a title-bar Background Tasks button
The system SHALL place a button labeled `Background Tasks` at the upper-right of the application title bar. Activating it SHALL open the right-side view. When the active pane is a Codex Agent tab with a started or restored thread, or a Claude Code Agent tab with a started or restored session, the view SHALL be scoped to that parent session; otherwise the view SHALL report that there is no agent session to show. The button SHALL remain available in every case, and SHALL NOT carry a task count or an activity indicator.

#### Scenario: Open the view for a Codex thread
- **WHEN** the active Codex Agent tab has a parent thread ID and the user clicks the title-bar `Background Tasks` button
- **THEN** the right-side area opens with task data scoped to that parent thread

#### Scenario: Open the view for a Claude Code session
- **WHEN** the active Claude Code Agent tab has a session ID and the user clicks the title-bar `Background Tasks` button
- **THEN** the right-side area opens with task data scoped to that Claude Code session

#### Scenario: Active pane has no supported parent session
- **WHEN** the active pane is a terminal, an unsupported Agent provider, or an Agent tab that has not established its provider session ID
- **THEN** the view opens and reports that there is no agent session, and no task data from another tab is shown

#### Scenario: The active pane stops being a supported session while the view is open
- **WHEN** the view is open and the user activates a pane with no supported provider session
- **THEN** the right-side area stays open reporting that there is no agent session, rather than closing

### Requirement: Share one right-side area
Git and `Background Tasks` SHALL select the same resizable right-side area. Selecting either view SHALL replace the other view without opening a second right-side column, and both views SHALL use the same current width and resize behavior.

#### Scenario: Switch from Git to Background Tasks
- **WHEN** Git is visible and the user selects `Background Tasks`
- **THEN** `Background Tasks` replaces Git within the existing right-side area and the main pane is not narrowed by another column

#### Scenario: Switch back to Git
- **WHEN** `Background Tasks` is visible and the user selects Git
- **THEN** Git replaces `Background Tasks` at the current right-side width

### Requirement: Limit the initial view to child-agent work
The initial `Background Tasks` view SHALL include Codex descendants created as subagents and Claude Code Task or Agent child work. It SHALL NOT include background shell commands, preview servers, workflows, monitors, scheduled work, or other non-agent background activity.

#### Scenario: Claude Code runs agent and command work
- **WHEN** a Claude Code session has one Task child agent and one background shell command
- **THEN** the view shows the child agent and does not show the shell command

### Requirement: Present running and finished child states
The `Background Tasks` view SHALL group Starting, Working, and Needs Input rows under Running. It SHALL group Done, Interrupted, Stopped, and Failed rows under Finished while preserving the specific terminal state on each row. The Running heading SHALL show the active count and the number that need input. The Finished heading SHALL show the terminal count.

#### Scenario: Mixed child states
- **WHEN** one child is working, one needs input, one completed, and one failed
- **THEN** Running shows two rows with a needs-input count of one and Finished shows two rows with distinct Done and Failed labels

#### Scenario: No child agents exist
- **WHEN** the selected parent session has no known child agents
- **THEN** the view displays a clear empty state instead of retaining rows from another parent session

### Requirement: Show useful task row details
Each row SHALL show the provider, a stable child identity, a display name when available, an objective or latest status summary, and its lifecycle state. Active rows SHALL show elapsed time that advances locally. Terminal rows SHALL show a relative completion time. Missing optional values SHALL use a neutral fallback without hiding the row. Each row SHALL indicate that it opens that child's detail view.

#### Scenario: Full child metadata is available
- **WHEN** a child has a provider, display name, objective, start time, and running state
- **THEN** its row shows the provider, name, objective, Working state, and advancing elapsed time

#### Scenario: Optional metadata is missing
- **WHEN** a child has a provider-qualified ID and lifecycle state but no name or objective
- **THEN** its row remains visible with an ID-derived name and a neutral status summary

#### Scenario: A row is inspectable
- **WHEN** the user points at a task row
- **THEN** the row presents itself as activatable rather than relying on a hover-only summary of its status

### Requirement: Restore Codex descendant status
The system SHALL discover all descendant Codex threads belonging to the selected parent after the parent becomes ready, after a restored session loads, and after app-server reconnection. Restored rows SHALL merge with later live updates by provider-qualified child ID without duplicating a child or replacing a newer state with older data.

#### Scenario: Resume a Codex parent with completed children
- **WHEN** the user restores a Codex parent thread that already has completed descendants
- **THEN** those descendants appear under Finished without requiring new child activity

#### Scenario: Nested Codex descendants exist
- **WHEN** a direct Codex child created another child under the selected parent
- **THEN** both descendants are included and retain their immediate parent references for future hierarchical presentation

#### Scenario: Older Codex data arrives after a live update
- **WHEN** restored data reports a child as Working after a newer live update reported it as Done
- **THEN** the row remains Done

### Requirement: Restore Claude Code child-agent status
The system SHALL rebuild Claude Code child-agent summaries from Task or Agent launches and results in the selected parent history, enriching those summaries with linked sidechain activity and recognized task lifecycle records. Restored rows SHALL merge with later live updates by provider-qualified child ID without duplication or stale-state replacement.

#### Scenario: Resume a Claude Code session with completed tasks
- **WHEN** the user restores a Claude Code session whose history contains completed Task child agents
- **THEN** those child agents appear under Finished before any new child activity occurs

#### Scenario: Linked sidechain history is available
- **WHEN** sidechain records identify a parent Task that belongs to the selected parent history
- **THEN** those records can enrich that task's latest status without appearing in the parent transcript

#### Scenario: Unlinked sidechain history is present
- **WHEN** a sidechain record cannot be linked to a Task launch in the selected parent history
- **THEN** it does not create a row and does not alter the parent transcript

#### Scenario: Older Claude Code history is merged after a live update
- **WHEN** restored history reports a task as Working after a newer live update reported it as Done
- **THEN** the row remains Done

### Requirement: Isolate child activity from the parent session
Codex thread notifications and Claude Code task or sidechain records SHALL affect the parent transcript and parent running state only when they belong to parent activity. Confirmed child activity SHALL update task status without inserting child content into the parent transcript or changing the parent's current turn identity. Unrelated activity SHALL be ignored by that Agent tab. A record the provider synthesized to report a child's status SHALL NOT be presented as something the user sent, in a live or a restored conversation.

#### Scenario: Codex child turn starts while the parent is idle
- **WHEN** a confirmed Codex descendant emits a turn-started notification while the parent has no active turn
- **THEN** the child becomes active and the parent remains idle with no parent turn ID

#### Scenario: Codex child completes while the parent is running
- **WHEN** a confirmed Codex descendant emits a turn-completed notification while the parent turn is still running
- **THEN** the child enters its reported terminal state and the parent remains running

#### Scenario: Claude Code sidechain emits content
- **WHEN** a Claude Code sidechain linked to a Task emits user, assistant, reasoning, command, or file-change content
- **THEN** the content can update that task's latest summary but does not appear in the parent transcript

#### Scenario: Claude Code child completes before the parent
- **WHEN** a Claude Code child reports completion while the parent turn continues
- **THEN** the child enters its terminal state and the parent remains running

#### Scenario: A restored conversation contains child status notifications
- **WHEN** a restored session's history contains provider-synthesized turns reporting a child agent's status
- **THEN** those turns do not appear as user prompts and do not title the session

### Requirement: Map provider lifecycle data consistently
The system SHALL map provider lifecycle input to the shared Starting, Working, Needs Input, Done, Interrupted, Stopped, and Failed states. A later explicit lifecycle update SHALL replace an earlier state for the same child according to live event order, including an explicit resume after a terminal state.

For Codex, the system SHALL derive Starting from `pendingInit`, Working from `running`, Done from `completed`, Interrupted from `interrupted`, Stopped from `shutdown`, and Failed from `errored`, `notFound`, or a child system error. An active Codex child with `waitingOnApproval` or `waitingOnUserInput` SHALL display Needs Input.

For Claude Code, the system SHALL derive Starting from a Task or Agent launch before confirmed activity, Working from `running`, `task_started`, or linked child activity, Done from `completed` or a successful terminal result, Stopped from `stopped`, `killed`, or a confirmed local-process boundary, and Failed from `failed` or an error result. A task-scoped wait for approval or user input SHALL display Needs Input; an unscoped parent wait SHALL NOT be assigned to a child.

#### Scenario: Codex child awaits user input
- **WHEN** a running Codex child reports `waitingOnUserInput`
- **THEN** its row displays Needs Input and contributes to the needs-input count

#### Scenario: Claude Code task starts before detailed activity arrives
- **WHEN** a Claude Code Task launch is observed before its first child event
- **THEN** its row appears as Starting

#### Scenario: Claude Code task reports an error
- **WHEN** a Claude Code child reports `failed` or its matched terminal result is an error
- **THEN** its row moves under Finished with a Failed state

#### Scenario: Claude Code process restarts with active local tasks
- **WHEN** a Claude Code process boundary is observed and a previously active local task is not confirmed active in the new process epoch
- **THEN** that task moves under Finished with a Stopped state

#### Scenario: Parent-only approval is requested
- **WHEN** Claude Code requests approval without a stable association to a child task
- **THEN** the parent session shows its normal approval state and no child row is changed to Needs Input

### Requirement: Keep child operations out of Background Tasks
`Background Tasks` SHALL allow inspecting a child agent's status and conversation. It SHALL NOT provide controls for sending input to a child, responding to child approvals, interrupting a child, resuming a child, stopping a child, or closing a child.

#### Scenario: Inspect a child row
- **WHEN** the user activates a task row or reads its detail view
- **THEN** the view exposes status and conversation only and does not dispatch a child operation

#### Scenario: A child needs input
- **WHEN** a child reports that it needs approval or user input
- **THEN** the view reports that state and offers no control that answers on the child's behalf

### Requirement: Handle provider restoration failure without harming the session
If provider-specific restoration fails, the system SHALL keep the parent transcript and composer usable, preserve any newer live rows already known, present a non-blocking unavailable state when no task data can be shown, and record diagnostic details.

#### Scenario: Codex descendant query fails before any child is known
- **WHEN** `Background Tasks` opens and Codex descendant discovery fails before live child data exists
- **THEN** the view reports that status is unavailable while the parent conversation remains usable

#### Scenario: Claude Code history cannot be read
- **WHEN** `Background Tasks` opens for a restored Claude Code session and its history cannot be read before live child data exists
- **THEN** the view reports that status is unavailable while the parent conversation remains usable

#### Scenario: Refresh fails after live updates
- **WHEN** provider restoration fails after live task rows were received
- **THEN** the existing rows remain visible and the failure does not reset their states

### Requirement: Open a child transcript from a task row
Activating a task row SHALL replace the task list with a detail view for that child agent, within the same right-side area. The detail view SHALL identify the child by the same provider, name, lifecycle state, and timing the row showed, and SHALL offer a control that returns to the task list. Returning SHALL restore the list at its previous scroll position and section expansion.

#### Scenario: Open a child from the list
- **WHEN** the user activates a row for a running Codex or Claude Code child
- **THEN** the right-side area shows that child's detail view in place of the list, without opening another column

#### Scenario: Return to the list
- **WHEN** the user activates the back control in a detail view
- **THEN** the task list returns with the scroll position and expanded sections it had when the detail view opened

#### Scenario: The child changes while its detail view is open
- **WHEN** a child completes or fails while its own detail view is showing
- **THEN** the detail view updates that child's lifecycle state and timing without closing or returning to the list

### Requirement: Present a child transcript like a parent conversation
The child detail view SHALL render the child's conversation using the same transcript presentation as an Agent pane, covering row kinds, work-log grouping and folding, disclosure geometry and expansion, code virtualization, prose measure, and timestamps. A child transcript SHALL NOT introduce presentation that the parent conversation does not use.

#### Scenario: Child work log
- **WHEN** a child transcript contains user input, assistant replies, reasoning, command executions, file changes, and other tool calls
- **THEN** each is presented as the corresponding Agent pane row kind, with the same grouping, folding, and expansion behavior

#### Scenario: Long child output
- **WHEN** a child transcript contains output long enough to require virtualization in the Agent pane
- **THEN** the detail view virtualizes it the same way rather than rendering it in full

### Requirement: Load a child transcript per provider
The system SHALL load a Codex child transcript by reading the descendant thread's stored turns without resuming it or altering the parent session. It SHALL load a Claude Code child transcript from the activity linked to that child's launch, combining retained live activity with the linked records in the selected session history. Loading SHALL report a not-loaded, loading, ready, or unavailable state.

#### Scenario: Open a finished Codex child
- **WHEN** the user opens a Codex child that already completed
- **THEN** its stored conversation is shown and the parent thread's turn state and transcript are unchanged

#### Scenario: Open a running Claude Code child
- **WHEN** the user opens a Claude Code child whose linked activity has been arriving live
- **THEN** the detail view shows that activity, and later linked activity for the same child extends it

#### Scenario: A child transcript cannot be loaded
- **WHEN** loading a child transcript fails
- **THEN** the detail view reports that its transcript is unavailable, keeps the child's status details visible, and leaves the parent conversation usable

### Requirement: Scope the detail view to its parent session
A child detail view SHALL belong to the parent session that owns the child. When the active pane stops being that parent session, the right-side area SHALL NOT continue to show that child's transcript.

#### Scenario: Switch to another Agent tab
- **WHEN** a child detail view is open and the user activates a different Agent tab
- **THEN** the detail view does not remain for the previous session's child

#### Scenario: Switch to an unsupported pane
- **WHEN** a child detail view is open and the user activates a terminal or an unsupported Agent provider
- **THEN** the right-side area stops showing that child's transcript
