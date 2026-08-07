# claude-session-rewind

## Purpose

Claude Agent Tab can restore files, conversation state, or both from checkpoints on the active session chain while retaining the original session as recoverable history.

## Requirements

### Requirement: Claude rewind targets are built from the active conversation chain
Claude Agent Tab SHALL derive rewind targets from human-authored user messages on the current `parentUuid` chain and SHALL order them from most recent to oldest. Each target SHALL retain the user message UUID needed by the provider, the preceding message identity, the original prompt text, and an available timestamp. Tool-result containers, meta records, sidechain messages, and messages on abandoned branches MUST NOT appear as targets.

#### Scenario: Session contains an abandoned branch
- **WHEN** a Claude transcript contains user prompts on two branches and the latest valid leaf belongs to only one branch
- **THEN** the rewind target list contains prompts from the leaf's `parentUuid` chain and excludes prompts unique to the abandoned branch

#### Scenario: No eligible user prompt exists
- **WHEN** the active chain contains no human-authored user text message
- **THEN** Agent Tab reports that there is no rewind checkpoint and does not open the action selector

### Requirement: Claude rewind presents explicit target and action selection
Invoking `/rewind` SHALL first present the eligible prompt targets and then present `Restore files`, `Restore conversation`, `Restore files and conversation`, and `Cancel` for the chosen target. File-related choices SHALL state that only edits tracked by Claude checkpointing are covered. Cancelling either stage SHALL leave the backend, transcript, files, and turn accounting unchanged.

#### Scenario: User chooses a checkpoint
- **WHEN** the user activates a prompt row with the keyboard or mouse
- **THEN** Agent Tab retains that prompt's exact user message UUID and opens the action selector without sending a provider turn

#### Scenario: User cancels rewind
- **WHEN** the user cancels target selection or chooses `Cancel` in the action selector
- **THEN** Agent Tab closes the rewind UI and performs no file, conversation, or backend mutation

### Requirement: New Claude sessions enable provider file checkpointing
Every newly spawned Claude stream-json process SHALL enable the provider's SDK file-checkpointing mode. Existing sessions without a usable file checkpoint SHALL remain eligible for conversation restore; when file unavailability can be established before execution, file-related actions SHALL be disabled with a reason.

#### Scenario: A new session starts
- **WHEN** Agent Tab spawns a new Claude stream-json process
- **THEN** the process environment enables SDK file checkpointing before the first user prompt

#### Scenario: A legacy session has no file checkpoint
- **WHEN** a restored Claude session has rewindable conversation prompts but is known to have no file snapshot for the selected prompt
- **THEN** conversation restore remains selectable and file-related actions identify that no file checkpoint is available

### Requirement: File-only rewind uses the selected provider checkpoint
For `Restore files`, the Claude adapter SHALL send a `rewind_files` control request containing the selected user message UUID and SHALL correlate the matching control response before completing the operation. Success SHALL leave the conversation, session id, composer, and turn counters unchanged and SHALL report that Claude-tracked files were restored. Rejection, expiry, or protocol failure SHALL be non-fatal and MUST NOT be reported as success.

#### Scenario: Provider restores tracked files
- **WHEN** the matching `rewind_files` control response reports success
- **THEN** Agent Tab keeps the current conversation active, adds no user or assistant turn, and displays a local file-restore confirmation

#### Scenario: Provider rejects the checkpoint
- **WHEN** the matching control response reports that the checkpoint is unavailable or expired
- **THEN** Agent Tab displays a non-fatal local error, keeps the current session usable, and does not claim that files were restored

#### Scenario: An unrelated control response arrives
- **WHEN** a control response has a request id different from the pending rewind operation
- **THEN** that response does not complete or fail the pending file rewind

### Requirement: Conversation rewind creates a recoverable fork before the selected prompt
For `Restore conversation`, Agent Tab SHALL create a new Claude session containing the active-chain prefix ending at the selected prompt's parent, while leaving the original session transcript unchanged. The replacement session SHALL use the same cwd, agent kind, model and permission settings; the selected original prompt SHALL be restored into the composer. Conversation restore MUST NOT change workspace files or copy the original session's file undo history.

#### Scenario: Restore before a later prompt
- **WHEN** the user restores conversation at prompt `P` and `P` has a parent message
- **THEN** Agent Tab switches to a new session whose replay ends at that parent, places `P`'s text in the composer, and retains the original full session in history

#### Scenario: Restore before the first prompt
- **WHEN** the selected prompt is the first human prompt in the active chain
- **THEN** Agent Tab switches to a fresh empty Claude session and places the first prompt's text in the composer

#### Scenario: Continue after conversation restore
- **WHEN** the user edits or submits the restored composer text after a successful conversation rewind
- **THEN** subsequent messages are appended under the fork's new session id and the original session remains unchanged

### Requirement: Combined rewind restores files before forking conversation
For `Restore files and conversation`, Agent Tab SHALL wait for successful file rewind on the original session and selected user message UUID before creating the conversation fork. If file rewind fails, the fork MUST NOT begin. If files were restored but the later fork or backend replacement fails, Agent Tab SHALL report the partial result explicitly and SHALL retain the original session as a recovery path.

#### Scenario: Combined rewind succeeds
- **WHEN** file rewind succeeds and the conversation fork starts successfully
- **THEN** Claude-tracked files match the selected checkpoint, the active conversation is the prefix fork before the prompt, and the selected prompt text is in the composer

#### Scenario: File phase fails
- **WHEN** the file rewind request fails during a combined action
- **THEN** Agent Tab does not create or activate a fork and keeps the original conversation active

#### Scenario: Conversation phase fails after file success
- **WHEN** files were restored successfully but fork creation or replacement-session startup fails
- **THEN** Agent Tab states that files were restored but conversation was not rewound and offers the preserved original session as the recovery state

### Requirement: Rewind session replacement rejects stale activity
While a rewind mutation is executing, Agent Tab SHALL prevent duplicate rewind execution and ordinary message submission. After a conversation fork becomes active, events, approvals, queued commands, and operation results belonging to the replaced backend MUST NOT alter the new transcript or start work in the new session.

#### Scenario: Old backend emits a late event
- **WHEN** the replaced Claude backend emits an assistant delta or completion after the fork is active
- **THEN** Agent Tab discards the stale event and the fork transcript remains unchanged

#### Scenario: User submits while rewind is mutating state
- **WHEN** file restoration, fork creation, or session replacement is in progress
- **THEN** Agent Tab does not send the composer text and indicates that rewind must finish first
