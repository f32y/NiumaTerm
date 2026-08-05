# agent-slash-commands

Agent Tab provides discoverable and filterable slash commands in the composer and routes them safely for Claude Code stream-json and Codex app-server without affecting ordinary terminal panes.

## ADDED Requirements

### Requirement: Open the slash-command palette at the start of a message
When the raw Agent Tab composer text begins with `/` and the cursor is within the first command token, the system SHALL display the slash-command palette above the composer. It SHALL NOT display the palette when whitespace or another character precedes `/`, when `/` occurs later in the text, or in an ordinary terminal pane.

#### Scenario: Enter slash in an empty composer
- **WHEN** the user enters `/` in an empty Claude or Codex Agent Tab composer
- **THEN** the list of commands available for the current backend appears immediately above the composer

#### Scenario: Ordinary text contains a slash
- **WHEN** the user enters `please inspect src/ui/` or ` https://example.com`
- **THEN** the system treats the input as ordinary message editing and does not display the slash-command palette

### Requirement: Display and filter command entries
Each palette entry SHALL show its normalized `/name`, a description, and an argument hint when present. As the query after `/` changes within the first token, the system SHALL filter entries in real time and order exact matches before prefix matches and prefix matches before substring matches. An unavailable command SHALL show why it is disabled and cannot execute.

#### Scenario: Filter by command prefix
- **WHEN** the user enters `/comp`
- **THEN** `/compact` appears before entries that match only by substring and remains highlighted and selectable

#### Scenario: Current state prevents execution
- **WHEN** an agent is running and the list contains the IdleOnly command `/clear`
- **THEN** `/clear` is disabled with a reason such as "stop the current task first" and the user cannot execute it

#### Scenario: No entry matches
- **WHEN** the user's query matches no command in the catalog
- **THEN** the palette displays an empty-result state and leaves the composer text unchanged

### Requirement: Support keyboard and mouse operation
While the palette is visible, the system SHALL support Up and Down to move the highlight, Enter to execute or enter the argument stage, Tab to complete the command token, Escape to close the palette, and mouse selection. It SHALL intercept only keys needed by the palette and leave other editing, IME, and newline behavior to the existing InputState.

#### Scenario: Select a command with the keyboard
- **WHEN** the palette is visible and the user highlights `/status` with Down and presses Enter
- **THEN** the system executes `/status` without passing the key to ordinary message submission

#### Scenario: Tab completes without executing
- **WHEN** the user enters `/per`, highlights `/permissions`, and presses Tab
- **THEN** the composer becomes `/permissions ` and enters argument selection without changing settings yet

#### Scenario: Escape closes the palette
- **WHEN** an agent is running, the palette is visible, and the user presses Escape
- **THEN** the palette closes, input remains, and the running agent is not interrupted

#### Scenario: Select a command with the mouse
- **WHEN** the user selects an available command row
- **THEN** the system performs the same action as pressing Enter on that row

### Requirement: Build a backend-aware command catalog
The system SHALL combine NiumaTerm local commands, commands explicitly supported by the current backend adapter, and commands published dynamically by the provider, deduplicating by normalized name. Local commands SHALL take precedence over adapter commands, which SHALL take precedence over dynamic commands with the same name. The Codex catalog SHALL contain only commands implemented by NiumaTerm. A newly published Claude dynamic catalog SHALL replace the previous dynamic catalog.

#### Scenario: Claude init publishes dynamic commands
- **WHEN** Claude `system/init.slash_commands` publishes `/compact`, `/custom-review`, and `/mcp__server__prompt`
- **THEN** the palette combines those entries, retains the known local description and route for `/compact`, and adds the other two commands

#### Scenario: Claude catalog is not ready
- **WHEN** a new Claude session has not received an init message containing `slash_commands`
- **THEN** the composer and core local commands remain available, the palette explains that additional provider commands appear after session initialization, and the system does not create a hidden turn to warm the catalog

#### Scenario: Codex omits an unsupported TUI command
- **WHEN** Codex TUI documentation includes `/theme` but NiumaTerm has no matching UI or app-server route
- **THEN** the Codex Agent Tab palette does not display `/theme`

### Requirement: Keep slash commands out of ordinary message submission
The system SHALL parse a leading slash command before calling ordinary `send_user_message`. Recognized, unknown, and unavailable commands SHALL NOT enter ordinary `turn/start` or `turn/steer`. Only input that does not follow slash syntax may use the existing ordinary message path.

#### Scenario: Unknown Codex command
- **WHEN** the user submits `/does-not-exist` in a Codex Agent Tab
- **THEN** an unknown-command error appears below the composer, the input remains, and app-server receives neither `turn/start` nor `turn/steer`

#### Scenario: Submit a slash command while running
- **WHEN** a Codex turn is running and the user submits `/compact`
- **THEN** the system queues it under the command's execution policy instead of sending `/compact` as a steer message

#### Scenario: Submit an ordinary message
- **WHEN** the user submits `please perform the equivalent of /compact`
- **THEN** the system uses the existing ordinary message or steer behavior

### Requirement: Select model and permissions arguments
`/model` and `/permissions` SHALL provide a filterable second stage from the same data sources as the composer settings row. Choosing a valid value SHALL update the same `ThreadSettings`, clear the command input, and show confirmation. An invalid or ambiguous explicit argument SHALL keep the input, display available values, and SHALL NOT create a provider turn.

#### Scenario: Switch through a model option
- **WHEN** the user executes `/model` and selects a model from the current model catalog in the second stage
- **THEN** `ThreadSettings.model` and the settings row update together, the next ordinary turn uses that model, and the current action creates no user bubble or working timer

#### Scenario: Switch through an explicit permission argument
- **WHEN** the user submits `/permissions read-only` and `read-only` is a valid protocol value for the current backend
- **THEN** `ThreadSettings.approval` changes to that value and the settings row reflects it immediately

#### Scenario: Model argument is invalid
- **WHEN** the user submits `/model missing-model` and the catalog has neither that protocol value nor an unambiguous display name
- **THEN** the system displays an error with available values, retains the composer input, and does not change the current model

### Requirement: Execute compact through the provider
When the session is ready, the system SHALL support `/compact`. The Claude backend SHALL send `/compact` through a dedicated provider-command path. The Codex backend SHALL call `thread/compact/start` with the current `threadId`. The command SHALL NOT create a user-message bubble, while standard turn and item events produced by the provider SHALL remain visible and drive the working state.

#### Scenario: Codex executes compact
- **WHEN** an idle Codex session submits `/compact`
- **THEN** the Client sends `thread/compact/start { threadId }` and does not send a `turn/start` containing `/compact`

#### Scenario: Claude executes compact
- **WHEN** an idle Claude session submits the supported `/compact`
- **THEN** the stream-json adapter sends it as a provider command and does not add a user `/compact` bubble to the transcript

#### Scenario: Compact RPC fails
- **WHEN** the provider rejects compaction or returns an error
- **THEN** the system displays a non-fatal error, returns to an idle editable state, and keeps the existing session usable

### Requirement: Execute review through the provider
When the current backend supports review, the system SHALL provide `/review`. The initial Codex implementation SHALL call `review/start` for the current thread with `delivery: inline` and `target: { type: "uncommittedChanges" }`. Claude SHALL pass the command to the provider only when the adapter explicitly supports it or the dynamic catalog publishes it. Standard transcript items SHALL expose review activity and results.

#### Scenario: Codex reviews uncommitted changes
- **WHEN** an idle Codex session executes `/review`
- **THEN** the Client sends inline `review/start` with an `uncommittedChanges` target and displays subsequent review activity and the final result

#### Scenario: Claude does not publish review
- **WHEN** the Claude dynamic catalog is ready but lacks `/review`, and the adapter declares no baseline support
- **THEN** the palette omits `/review` and manual submission produces a local unknown-command error

### Requirement: New and clear create a clean session
`/new` and `/clear` SHALL be equivalent NiumaTerm local actions. While the current agent is idle, they SHALL terminate and discard the current backend instance, clear transcript, turn, approval, and command-queue state, and start a new session with the same agent kind and cwd. Persisted provider history from the old session SHALL remain. A history list already hidden in this tab SHALL NOT reappear after clearing.

#### Scenario: Clear an idle session
- **WHEN** the user executes `/clear` in an idle Agent Tab with transcript content
- **THEN** the transcript clears, a backend with the same kind and cwd starts, session-history files remain, and the history list stays hidden in that tab

#### Scenario: Attempt clear while running
- **WHEN** an agent is running and the user attempts `/clear`
- **THEN** the command remains disabled with a prompt to stop the current task first, and the active backend is not destroyed

### Requirement: Status reports known local state
`/status` SHALL immediately display a local notice outside any provider turn, including at least the backend, connection or running state, current model, and permission or approval. Codex SHALL also show the current sandbox, effort, and tier. The system MUST NOT display token usage that it does not track or cannot confirm.

#### Scenario: Show Codex status
- **WHEN** the user executes `/status` in a Codex Agent Tab
- **THEN** the notice shows current connection state, model, approval, sandbox, effort, and tier without creating a user bubble, provider RPC, or working timer

#### Scenario: Show Claude status
- **WHEN** the user executes `/status` in a Claude Agent Tab
- **THEN** the notice shows current connection state, model, and permission mode and omits unknown fields instead of guessing

### Requirement: Apply explicit policies to commands during active work
Each command SHALL declare an Immediate, QueueUntilIdle, or IdleOnly policy. `/status`, `/model`, and `/permissions` SHALL execute immediately. `/compact`, `/review`, and dynamic Claude commands SHALL enter a FIFO queue during active work. `/new` and `/clear` SHALL be disabled during active work. Queued commands SHALL execute one at a time after the current turn completes, and a command that starts a new turn must complete before the next command runs.

#### Scenario: Execute queued commands in FIFO order
- **WHEN** the current turn is running and the user queues `/compact` followed by a dynamic Claude command
- **THEN** `/compact` runs after the current turn, and the dynamic command runs after the turn produced by `/compact` completes

#### Scenario: Show status immediately while running
- **WHEN** the current turn is running and the user executes `/status`
- **THEN** the status notice appears immediately and the current turn continues unaffected

#### Scenario: Session exits with queued commands
- **WHEN** the backend becomes fatal or exits while the command queue is non-empty
- **THEN** the system clears the queue, displays why those commands did not run, and does not send them to the exited session

### Requirement: Keep command feedback distinct from conversation turns
Confirmations, errors, and queue notices from local commands SHALL be distinct from user messages and Agent responses in both the data model and UI. They SHALL NOT advance the turn counter, start a working timer, or participate in completed-turn grouping. A provider command may enter the existing turn lifecycle only when the provider actually creates a turn.

#### Scenario: Local settings command succeeds
- **WHEN** `/model` changes the model successfully
- **THEN** the system displays a lightweight confirmation without adding a user or Agent conversation turn

#### Scenario: Provider command creates a turn
- **WHEN** Codex `/review` produces `turn/started`, item events, and `turn/completed`
- **THEN** the system uses existing working and grouping behavior for that provider turn without adding a synthetic user `/review` bubble
