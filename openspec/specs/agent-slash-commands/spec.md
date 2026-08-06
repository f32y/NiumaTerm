# agent-slash-commands

## Purpose

Agent Tab provides discoverable and filterable slash commands in the composer and routes them safely for Claude Code stream-json and Codex app-server without affecting ordinary terminal panes.

## Requirements

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

### Requirement: Claude publishes its complete command catalog before the first turn

The Claude adapter SHALL treat the structured initialize `response.commands` array as the primary provider command catalog. It SHALL publish each command's canonical name, description, argument hint, and aliases to the existing slash palette before the first user turn. The legacy `system/init.slash_commands` list SHALL be used only when initialize did not provide a structured catalog, and catalog discovery SHALL NOT create a hidden provider turn.

#### Scenario: Structured initialize catalog is available

- **WHEN** Claude initialize returns a skill, a project command, and a namespaced plugin skill in `response.commands`
- **THEN** the slash palette exposes all three entries before the first user turn with their published descriptions and argument hints

#### Scenario: A command publishes aliases

- **WHEN** a Claude command in the structured initialize catalog includes one or more aliases
- **THEN** its canonical name and aliases are filterable and executable through the provider-command route while existing local-over-adapter-over-provider conflict precedence is preserved

#### Scenario: Only the legacy catalog is available

- **WHEN** Claude initialize omits `response.commands` and a later `system/init` message contains `slash_commands`
- **THEN** the adapter publishes the legacy entries as the provider command catalog

#### Scenario: A legacy catalog follows a structured catalog

- **WHEN** the adapter has already published `response.commands` and later receives a string-only `system/init.slash_commands` list
- **THEN** the legacy list does not overwrite the structured descriptions, argument hints, or aliases

#### Scenario: Claude publishes no command catalog

- **WHEN** neither initialize nor `system/init` publishes a command catalog
- **THEN** local and adapter commands remain usable, command-catalog loading ends without a fatal session error, and no warm-up turn is sent

### Requirement: Codex loads and refreshes the skill catalog for the session working directory

After app-server initialization, the Codex adapter SHALL request the skills available to the session working directory through `skills/list` and publish a replacement snapshot containing each skill's name, description, absolute path, scope, enabled state, and available display metadata. Entries with the same name but different paths or scopes SHALL remain distinct. `skills/changed` SHALL invalidate the snapshot and trigger a forced reload, with overlapping notifications coalesced so an older response cannot replace a newer catalog. Catalog errors SHALL be non-fatal.

#### Scenario: Initial Codex skill catalog loads

- **WHEN** app-server initialization completes for an Agent Tab session
- **THEN** the adapter requests `skills/list` for that session's working directory and replaces the UI skill snapshot with the response

#### Scenario: Same-name skills exist in multiple scopes

- **WHEN** `skills/list` returns two skills with the same name but different absolute paths or scopes
- **THEN** both entries remain available with their own path and scope identities

#### Scenario: A skill catalog change is reported

- **WHEN** app-server sends one or more `skills/changed` notifications while no newer snapshot has been published
- **THEN** the adapter performs a forced `skills/list` reload and ultimately publishes the newest complete replacement snapshot

#### Scenario: The skill API is unavailable

- **WHEN** `skills/list` returns a method error or another catalog failure
- **THEN** ordinary chat, existing slash commands, and settings remain usable, while `/skills` reports that its catalog is unavailable

#### Scenario: The session is rebuilt

- **WHEN** Agent Tab replaces the active Codex session
- **THEN** the previous session's skill snapshot is cleared and cannot be selected while the new backend catalog is loading

### Requirement: Codex slash palette exposes skills directly and retains a focused view

The Codex top-level slash palette SHALL combine slash commands and the current skill snapshot in one filterable result set. Top-level skill rows SHALL be visually addressable from `/`, retain a dedicated skill action with exact name/path identity, and participate with command rows in the existing exact, prefix, and substring ranking. The Codex slash catalog SHALL also retain `/skills` as an optional skills-only second-stage view. Both views SHALL show the skill name, description, scope, and disabled state; preserve same-name entries as separate rows; and support the existing Up, Down, Enter, Tab, Escape, and mouse interactions. Disabled entries SHALL remain visible but SHALL NOT be selectable.

#### Scenario: Open the top-level Codex slash palette

- **WHEN** a Codex user types `/` after the skill snapshot is available
- **THEN** the first-level palette includes ordinary slash commands, ordinary skills, and namespaced plugin skills without requiring `/skills` to be selected first

#### Scenario: Filter commands and skills together

- **WHEN** the user continues typing after `/` with a query matching a command, skill name, display name, or description
- **THEN** command and skill rows are filtered and ordered together using exact, prefix, and substring ranking

#### Scenario: Complete a top-level skill with Tab

- **WHEN** the user highlights an enabled skill in the first-level slash palette and presses Tab
- **THEN** the composer becomes `$name `, the palette closes, and Agent Tab stores the selected row's exact name and path without sending a message

#### Scenario: Select a top-level skill with Enter or the mouse

- **WHEN** the user activates an enabled skill in the first-level slash palette with Enter or the mouse
- **THEN** Agent Tab performs the same `$name ` completion and exact binding action as Tab without starting or steering a turn

#### Scenario: Open the focused Codex skill picker

- **WHEN** a Codex user selects `/skills`
- **THEN** Agent Tab enters a skills-only second-stage view using the current snapshot and does not send `turn/start`, `turn/steer`, or a provider-command request

#### Scenario: Distinguish duplicate names

- **WHEN** multiple visible skill rows share the same name
- **THEN** their scope information and independent row actions allow the user to select the intended catalog entry

#### Scenario: A disabled skill is highlighted

- **WHEN** the selected skill row has `enabled=false` and the user presses Enter, Tab, or clicks it
- **THEN** the palette does not create a skill binding or close as if the skill were selected

#### Scenario: Cancel skill discovery

- **WHEN** the user presses Escape in the top-level or skills-only palette
- **THEN** the palette closes according to the existing cancellation behavior without creating a binding or sending a message

### Requirement: Codex invokes a selected skill with structured user input

Before submission, Agent Tab SHALL validate that a picker-created skill binding still matches the composer's exact first `$name` token and an enabled name/path pair in the current skill snapshot. A valid Codex `turn/start` or `turn/steer` request SHALL include both the original text input and a structured `{ type: "skill", name, path }` input item. The prompt SHALL retain normal user-message transcript and turn-lifecycle behavior. An invalid or stale binding SHALL preserve the composer, display an actionable error, and SHALL NOT send the stale skill path.

#### Scenario: Start a turn with a selected skill

- **WHEN** an idle Codex session submits a composer value whose first token and current catalog entry match the stored enabled skill binding
- **THEN** `turn/start` contains the text and structured skill items, and the prompt appears as one normal user turn

#### Scenario: Steer a running turn with a selected skill

- **WHEN** a running Codex session accepts steering and submits a composer value whose first token and current catalog entry match the stored enabled skill binding
- **THEN** `turn/steer` contains the text and structured skill items and follows the existing steered-message lifecycle

#### Scenario: Edit only the task arguments

- **WHEN** the user changes text after the bound `$name` token without changing that token
- **THEN** the binding remains eligible for submission and the complete edited text is sent with the structured skill item

#### Scenario: Edit the bound skill token

- **WHEN** the user changes or removes the first `$name` token after selecting a skill
- **THEN** Agent Tab clears or invalidates the binding and does not attach its name or path to a later submission

#### Scenario: The selected skill becomes stale

- **WHEN** a catalog refresh removes or disables the bound name/path pair before submission
- **THEN** submission is blocked, the composer text is retained, and Agent Tab explains that the skill must be selected again

#### Scenario: A skill token is typed manually

- **WHEN** the user types `$name` without selecting a catalog row
- **THEN** NiumaTerm sends it as ordinary text without guessing or attaching a path, including when multiple catalog entries share that name

### Requirement: Plugin-provided skills follow provider-published catalogs

NiumaTerm SHALL expose plugin-provided skills only when the active provider publishes them through its command or skill catalog. Claude namespaced plugin commands and aliases SHALL use the provider slash-command flow, while Codex namespaced plugin skills SHALL use the same top-level slash palette, optional `/skills` view, and structured-input flow as other Codex skills. This capability SHALL NOT scan plugin caches or advertise plugin marketplace, installation, removal, or enablement commands.

#### Scenario: Claude publishes a plugin command

- **WHEN** Claude initialize publishes a namespaced plugin skill and aliases in `response.commands`
- **THEN** the slash palette exposes those published command names through the Claude provider-command route

#### Scenario: Codex publishes a plugin skill

- **WHEN** Codex `skills/list` returns an enabled namespaced plugin skill
- **THEN** the top-level slash palette and `/skills` focused view allow the user to select it and submit its exact name and path through structured skill input

#### Scenario: A plugin has no published skill

- **WHEN** a plugin is installed but the active provider does not publish any command or skill for it
- **THEN** NiumaTerm does not synthesize an entry by inspecting plugin files or cache directories

#### Scenario: Plugin management is not supported

- **WHEN** a user opens the top-level slash palette for Claude or Codex
- **THEN** NiumaTerm does not advertise a `/plugins` management command unless a separate stable implementation is introduced
