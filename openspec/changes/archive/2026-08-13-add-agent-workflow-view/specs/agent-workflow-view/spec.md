## Purpose

Provide a read-only right-side view of Claude Code Dynamic Workflow runs for the active Agent tab, so a long multi-agent run exposes its phases, its per-agent progress, and each agent's own conversation while it is still running.

## ADDED Requirements

### Requirement: Offer a Workflows view scoped to the active session
The system SHALL provide a selectable `Workflows` view in the right-side area. When the active pane is a Claude Code Agent tab with a started or restored session, the view SHALL show only that session's workflow runs. Otherwise the view SHALL report that there is no session to show, and SHALL NOT show runs belonging to another tab.

#### Scenario: Open the view for a Claude Code session
- **WHEN** the active Claude Code Agent tab has a session and the user selects `Workflows`
- **THEN** the right-side area shows the workflow runs of that session

#### Scenario: Active pane has no Claude Code session
- **WHEN** the active pane is a terminal, a Codex Agent tab, or a Claude Code tab that has not established its session
- **THEN** the view reports that there is no session, and no run from another tab is shown

#### Scenario: The active pane changes while the view is open
- **WHEN** the view is open and the user activates a pane with no Claude Code session
- **THEN** the right-side area stays open reporting that there is no session, rather than closing

### Requirement: Reveal the control only once a workflow exists
The system SHALL keep the `Workflows` control out of the title bar until a tab has at least one workflow run, live or restored. Once shown it SHALL remain, so a run stays reachable after it has finished. While shown it SHALL report the number of agents the active tab currently has running, and SHALL show no number when that count is zero.

#### Scenario: No workflow has run
- **WHEN** no tab has a workflow run
- **THEN** the title bar offers no `Workflows` control

#### Scenario: A workflow starts
- **WHEN** a tab starts its first workflow run
- **THEN** the control appears

#### Scenario: A resumed session ran workflows before
- **WHEN** the user resumes a session whose earlier runs were persisted
- **THEN** the control appears once that session is ready, without the view being opened first

#### Scenario: Every run finishes
- **WHEN** the last run of every tab reaches a terminal state
- **THEN** the control remains available and reports no number

#### Scenario: Agents are running
- **WHEN** the active tab has three agents running
- **THEN** the control reports three

### Requirement: Present each workflow run with its status and phases
The view SHALL show one entry per workflow run of the session, identified by the workflow's name. Each run SHALL carry a lifecycle state of Starting, Running, Done, Failed, or Stopped, and SHALL list the run's phases in the order the provider reports them. A run SHALL appear as soon as the provider announces it, before any agent has produced progress.

#### Scenario: A run is announced before any agent starts
- **WHEN** the provider announces a workflow run and no agent has reported progress
- **THEN** the view shows the run with its name in a non-terminal state and an empty agent list

#### Scenario: A run reports phases
- **WHEN** the provider reports the run's phases
- **THEN** the view lists the phase titles in provider order, and each agent is shown under the phase it belongs to

#### Scenario: A run finishes
- **WHEN** the provider reports the run as completed
- **THEN** the run's state becomes terminal and its final result or error text is shown

### Requirement: Present the agent list with per-agent progress
For each run the view SHALL show one row per workflow agent, in the order the provider assigns. Each row SHALL show the agent's label and its state of Queued, Running, Done, Failed, or Stopped, where Stopped means the run ended before that agent did. A row SHALL additionally show the agent type, model, token count, and tool-call count whenever the provider reports them, and SHALL omit a detail the provider has not reported rather than showing a placeholder value. An agent whose result was reused from an earlier run SHALL be identified as reused, and an agent that failed SHALL show its error text.

#### Scenario: Agents progress through states
- **WHEN** a run has one agent queued, one running, and one finished
- **THEN** the view shows three rows carrying those three distinct states

#### Scenario: The provider reports partial detail
- **WHEN** an agent has a label and state but no reported token count
- **THEN** the row shows the label and state, and shows no token count

#### Scenario: An agent fails
- **WHEN** the provider reports an agent as failed with error text
- **THEN** that row shows the failed state and the error text

#### Scenario: An agent result is reused
- **WHEN** the provider reports an agent whose result was reused from an earlier run
- **THEN** that row is identified as reused

### Requirement: Report run-level totals
Each run SHALL show its agent count, aggregate token count, and aggregate tool-call count as reported by the provider. These totals SHALL update as the run progresses.

#### Scenario: Totals advance during a run
- **WHEN** a second agent of a run completes and the provider reports higher aggregate totals
- **THEN** the run's displayed totals increase accordingly

### Requirement: Refresh an active run once per second
While the `Workflows` view is open and at least one run of the scoped session is in a non-terminal state, the system SHALL refresh that run's state from the run's on-disk record at one-second intervals. Refreshing SHALL stop once every run of the session has reached a terminal state, and SHALL NOT run while the view is closed. A refresh SHALL NOT block the parent conversation, the composer, or provider message handling.

#### Scenario: A run is in progress
- **WHEN** the view is open and a run is in a non-terminal state
- **THEN** its agent states and conversations advance without further user action

#### Scenario: Every run has finished
- **WHEN** the last non-terminal run of the session reaches a terminal state
- **THEN** periodic refreshing stops

#### Scenario: The view is closed
- **WHEN** the user switches the right-side area to another view while a run is in progress
- **THEN** periodic refreshing stops, and resumes when `Workflows` is selected again

#### Scenario: A refresh is slow
- **WHEN** a refresh takes longer than its interval
- **THEN** refreshes do not overlap or accumulate, and the parent conversation stays responsive

### Requirement: Open one workflow agent's conversation
The view SHALL let the user open the conversation of a single workflow agent from its row, and SHALL let the user return to the run's agent list. A conversation SHALL be shown for an agent whose transcript the provider has persisted, including while the agent is still running. An agent with no persisted transcript SHALL report that its conversation is not available, while its row stays visible.

#### Scenario: Open a finished agent's conversation
- **WHEN** the user opens a Done agent's row
- **THEN** that agent's own conversation is shown

#### Scenario: Open a running agent's conversation
- **WHEN** the user opens a Running agent's row whose transcript has been partially written
- **THEN** the content written so far is shown and continues to extend while the agent runs

#### Scenario: An agent has no transcript yet
- **WHEN** the user opens the row of an agent the provider has not yet persisted a transcript for
- **THEN** the view reports that the conversation is not available and the row remains listed

#### Scenario: Return to the agent list
- **WHEN** the user leaves an open agent conversation
- **THEN** the run's agent list is shown again

### Requirement: Present a workflow agent conversation like a parent conversation
A workflow agent's conversation SHALL be presented with the same message, reasoning, and tool presentation the parent conversation uses, so a reader does not learn a second layout. It SHALL be read-only: the view SHALL NOT offer any way to send input to a workflow agent.

#### Scenario: An agent used tools
- **WHEN** a workflow agent's conversation contains assistant text and tool calls
- **THEN** they are rendered with the same row presentation the parent conversation uses for the same content

#### Scenario: No input is offered
- **WHEN** a workflow agent conversation is open
- **THEN** the view presents no composer or other means of sending input to that agent

### Requirement: Keep workflow content out of the parent conversation
Workflow run status and workflow agent conversation content SHALL NOT appear in the parent transcript, and SHALL NOT affect the parent's turn state, working indicator, or approval handling.

#### Scenario: A run produces agent output
- **WHEN** a workflow run's agents produce conversation content
- **THEN** the parent transcript shows only the workflow tool row, and no agent content is added to it

#### Scenario: A run is active
- **WHEN** a workflow run is in a non-terminal state
- **THEN** the parent conversation's turn state is unaffected by the run's own state

### Requirement: Survive an unreadable run record without harming the session
When a run's on-disk record is missing, partially written, or unreadable, the view SHALL keep every run and agent detail already known, SHALL report the affected run as unavailable for refresh rather than removing it, and SHALL retry on the next interval. A failure SHALL NOT interrupt the parent session, and SHALL NOT mark a run or agent as failed when the failure is only in reading the record.

#### Scenario: The record is not written yet
- **WHEN** a run has been announced but its on-disk record does not exist
- **THEN** the run stays listed with the detail the provider already reported, and refreshing retries

#### Scenario: A partially written record
- **WHEN** a refresh reads a record that ends mid-entry
- **THEN** the entries read so far are used, no state is reported as failed, and the next refresh reads the rest

#### Scenario: The record cannot be read
- **WHEN** a run's record cannot be read at all
- **THEN** the run reports that its state cannot be refreshed, its known detail stays visible, and the parent session is unaffected

### Requirement: Scope runs to their session
Workflow runs SHALL be tied to the Claude Code session that produced them. Switching to a different session SHALL show that session's runs, and SHALL NOT retain runs of the previous session. Runs of a resumed session SHALL be restored from the session's persisted record as soon as the session is ready, without waiting for the view to be opened.

#### Scenario: Switch between sessions
- **WHEN** the user switches from a tab with workflow runs to another Claude Code tab
- **THEN** the view shows only the second session's runs

#### Scenario: A resumed session had runs
- **WHEN** the user resumes a session whose earlier runs were persisted and opens the view
- **THEN** those runs are listed with their recorded terminal state

### Requirement: Restore a run the process outlived
A run whose completion record was never written SHALL still be restored, reported as Stopped, listing every agent whose conversation was persisted. Each such agent SHALL be identified from its own conversation, and SHALL report Done when the run recorded its result and Stopped otherwise. Detail the completion record alone carries — phases, per-agent accounting, and the run's own result — SHALL be omitted rather than guessed.

#### Scenario: A run was interrupted before it finished
- **WHEN** the user resumes a session whose workflow run ended with the process, leaving no completion record
- **THEN** the run is listed as Stopped with its name and one row per persisted agent conversation

#### Scenario: An interrupted run's agents are only partly accounted for
- **WHEN** an interrupted run recorded results for some of its agents
- **THEN** those agents report Done and the rest report Stopped

#### Scenario: An interrupted run reports no invented detail
- **WHEN** an interrupted run is listed
- **THEN** it shows no phases and no per-agent token or tool-call counts
