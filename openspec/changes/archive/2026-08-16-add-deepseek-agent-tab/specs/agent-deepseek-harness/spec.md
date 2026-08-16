## ADDED Requirements

### Requirement: DeepSeek harness selection
The system SHALL let a user create an agent profile whose harness is DeepSeek
Harness and open an Agent Tab from it, using the same profile surfaces that
serve the existing harnesses.

#### Scenario: DeepSeek appears in the harness list
- **WHEN** the user opens agent profile settings
- **THEN** DeepSeek Harness is offered alongside the existing harnesses without any harness being hidden or reordered

#### Scenario: A DeepSeek profile opens a tab
- **WHEN** the user opens an Agent Tab from a DeepSeek profile
- **THEN** the tab presents the DeepSeek harness identity and a composer ready to accept a prompt

### Requirement: Shared harness host process
The system SHALL run the DeepSeek harness as one `dsh web` host process bound to
loopback on an ephemeral port, SHALL discover its address from the address line
the host prints on its standard output, and SHALL serve every open DeepSeek tab
from that one process.

#### Scenario: The first DeepSeek tab starts the host
- **WHEN** the first DeepSeek tab opens and no host is running
- **THEN** the system starts one host, reads its address from the host's own output, and does not treat a fixed port as known in advance

#### Scenario: A second tab reuses the running host
- **WHEN** a second DeepSeek tab opens while a host is already running
- **THEN** the system creates another session on the running host and starts no second host process

#### Scenario: The last tab stops the host
- **WHEN** the last DeepSeek tab closes
- **THEN** the system terminates the host process and its process tree

#### Scenario: The host exits unexpectedly
- **WHEN** the host process exits while DeepSeek tabs are open
- **THEN** each affected tab reports that the harness stopped and stops presenting itself as ready to accept a prompt

### Requirement: Per-tab session isolation
Each DeepSeek tab SHALL own one harness session, and the system SHALL deliver an
event to a tab only when the event names that tab's session.

#### Scenario: Events reach only their own tab
- **WHEN** two DeepSeek tabs have sessions on the shared host and one is running a turn
- **THEN** only the tab owning that session shows the turn's activity

#### Scenario: A replayed event for an unknown session
- **WHEN** the event stream carries an event for a session no open tab owns
- **THEN** the system ignores it without error

### Requirement: Harness availability reporting
The system SHALL report why a DeepSeek tab cannot run when the harness is
unavailable, and SHALL distinguish a missing installation from a host that
failed to start.

#### Scenario: The harness is not installed
- **WHEN** the user opens a DeepSeek tab and `dsh` cannot be resolved
- **THEN** the tab states that DeepSeek Harness is not installed and identifies it as a user-installed dependency rather than offering to install it

#### Scenario: The host fails to start
- **WHEN** `dsh` resolves but the host process exits before printing an address, or prints no address within a bounded wait
- **THEN** the tab reports that the harness failed to start and surfaces the host's own failure output

#### Scenario: The installed version is outside the supported range
- **WHEN** the resolved `dsh` reports a version this build does not support
- **THEN** the tab states which version is installed and which range is supported, and still allows the user to proceed

### Requirement: Prompt submission
The system SHALL send a composed prompt to the tab's session and SHALL show the
user's own message in the transcript.

#### Scenario: A prompt is accepted
- **WHEN** the user submits a prompt in an idle DeepSeek tab
- **THEN** the system sends it to the tab's session and the transcript shows the submitted text as the user's message

#### Scenario: Injected context is not shown as the user's message
- **WHEN** the harness reports messages it injected itself alongside the user's prompt
- **THEN** the transcript shows only the message the user actually submitted

#### Scenario: The prompt is rejected
- **WHEN** the harness rejects a prompt
- **THEN** the tab reports the rejection and leaves the composed text recoverable

### Requirement: Streamed assistant output
The system SHALL render assistant text and assistant reasoning as they stream,
as separate transcript content, and SHALL fold the harness's completed message
into what streaming already produced without discarding it.

#### Scenario: Text streams incrementally
- **WHEN** the harness streams assistant text for the tab's session
- **THEN** the transcript grows as the text arrives rather than appearing only when the turn ends

#### Scenario: Reasoning is distinguishable from the answer
- **WHEN** the harness streams reasoning and answer text in the same turn
- **THEN** the transcript presents them as distinct content rather than one merged block

#### Scenario: A completed message arrives after streaming
- **WHEN** the harness reports the completed message for content the tab already streamed
- **THEN** the transcript reconciles them into one entry rather than showing the content twice

### Requirement: Stopping a running turn
The system SHALL let the user stop a running turn without closing the tab, and
SHALL keep whatever the turn already produced visible in the transcript.

#### Scenario: The user stops a running turn
- **WHEN** the user stops a turn that has already streamed part of an answer
- **THEN** the turn ends, the partial answer remains in the transcript, and the tab returns to accepting prompts

#### Scenario: A stopped turn is distinguishable from a finished one
- **WHEN** a turn ends because the user stopped it
- **THEN** the tab presents it as interrupted rather than as a completed answer

#### Scenario: Stop is offered only while a turn is running
- **WHEN** no turn is running in the tab
- **THEN** the tab offers no stop action

### Requirement: Run status and step progress
The system SHALL drive the tab's run status from the harness's own turn
lifecycle, and SHALL show step progress within a running turn.

#### Scenario: A turn starts
- **WHEN** the harness reports that a turn started for the tab's session
- **THEN** the tab presents itself as running

#### Scenario: A turn ends
- **WHEN** the harness reports that a turn ended
- **THEN** the tab presents itself as idle

#### Scenario: A turn fails
- **WHEN** the harness reports a turn that ended in failure, or reports an agent error with no turn position
- **THEN** the failure text is recorded in the transcript and the tab returns to idle

### Requirement: Unsupported capabilities are hidden
The system SHALL omit controls for capabilities this harness integration does
not yet provide, rather than presenting them disabled or inert.

#### Scenario: A capability is not provided
- **WHEN** a DeepSeek tab is open and a capability such as rewind is not provided by this integration
- **THEN** the tab presents no control for it

#### Scenario: A control would misreport what it does
- **WHEN** a control's effect cannot be expressed to this harness and a nearby weaker effect could be substituted silently
- **THEN** the tab omits the control rather than substituting the weaker effect

#### Scenario: Existing harnesses are unaffected
- **WHEN** a Codex or Claude tab is open
- **THEN** every control those tabs offered before this change is still offered

### Requirement: Answering an approval that blocks a turn
When the harness asks permission before continuing, the system SHALL present the
request and SHALL send the user's decision back to the harness. A request the
system cannot present or answer SHALL NOT leave the turn waiting with nothing on
screen.

#### Scenario: An approval is raised and answered
- **WHEN** the harness asks permission to run something during a turn
- **THEN** the tab presents what is being asked, and answering it lets the turn continue

#### Scenario: The request names what it is asking about
- **WHEN** an approval is presented
- **THEN** it identifies the tool involved and carries the harness's own stated reason, rather than reporting only that permission is needed

#### Scenario: A refused decision does not strand the turn
- **WHEN** the harness does not accept the answer the system sent
- **THEN** the system reports that the answer did not land instead of treating the decision as delivered

#### Scenario: Another client answers first
- **WHEN** the harness reports the pending approval resolved without this tab answering it
- **THEN** the tab stops presenting it

#### Scenario: A decision the harness cannot express is not offered
- **WHEN** the harness accepts only per-call decisions
- **THEN** the tab offers no control for granting permission for the rest of the session

### Requirement: Tool activity is visible
The system SHALL show each tool call the harness runs as a transcript entry, and
SHALL complete that entry with the call's outcome. A call the system has no
dedicated presentation for SHALL still appear rather than be omitted.

#### Scenario: A shell command
- **WHEN** the harness runs a command
- **THEN** the transcript shows the command, and completes it with the captured output and its exit status

#### Scenario: A file change
- **WHEN** the harness modifies a file
- **THEN** the transcript shows which file changed and a reviewable diff of the change

#### Scenario: A call with no dedicated presentation
- **WHEN** the harness runs a tool this integration models no specific row for
- **THEN** the transcript shows the call with what the harness said it is doing, rather than omitting it

#### Scenario: A call fails
- **WHEN** a tool call fails
- **THEN** its entry is completed as failed and carries the failure text the model itself received

#### Scenario: A result arrives for a call this tab never saw
- **WHEN** a result names a call whose start this tab did not receive
- **THEN** the system ignores it rather than opening an entry it cannot describe
