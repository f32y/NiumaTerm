## ADDED Requirements

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
