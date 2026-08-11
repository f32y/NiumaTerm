## ADDED Requirements

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

## RENAMED Requirements

- FROM: `### Requirement: Keep Background Tasks read-only`
- TO: `### Requirement: Keep child operations out of Background Tasks`

## MODIFIED Requirements

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

### Requirement: Keep child operations out of Background Tasks
`Background Tasks` SHALL allow inspecting a child agent's status and conversation. It SHALL NOT provide controls for sending input to a child, responding to child approvals, interrupting a child, resuming a child, stopping a child, or closing a child.

#### Scenario: Inspect a child row
- **WHEN** the user activates a task row or reads its detail view
- **THEN** the view exposes status and conversation only and does not dispatch a child operation

#### Scenario: A child needs input
- **WHEN** a child reports that it needs approval or user input
- **THEN** the view reports that state and offers no control that answers on the child's behalf
