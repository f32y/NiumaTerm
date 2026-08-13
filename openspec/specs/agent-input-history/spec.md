# Agent Input History Specification

## Purpose

Provide consistent, NiumaTerm-managed recall of recent Agent composer text across tabs and application restarts without relying on provider-owned history sources.

## Requirements

### Requirement: Record accepted Agent composer input
NiumaTerm SHALL add non-empty text to Agent input history only after the composer action is accepted as an ordinary message, steering input, or successfully dispatched slash command. Rejected submissions, unknown commands, unavailable commands, and provider dispatch failures MUST NOT create a history entry.

#### Scenario: Record an accepted new turn
- **WHEN** an idle Agent Tab accepts `inspect the failing tests` as a new user turn
- **THEN** that text becomes the newest entry in the current history scope

#### Scenario: Record accepted steering input
- **WHEN** a running Agent accepts `also check the Windows path` as steering input
- **THEN** that text becomes the newest entry in the current history scope

#### Scenario: Record a successful slash command
- **WHEN** the user submits a slash command and its local or provider action is successfully dispatched
- **THEN** the submitted command text becomes the newest entry in the current history scope

#### Scenario: Do not record rejected input
- **WHEN** submission remains in the composer because validation or dispatch fails
- **THEN** NiumaTerm does not add the text to input history

### Requirement: Persist bounded history by Agent context
NiumaTerm SHALL persist the 100 most recent entries for each combination of execution target, Agent backend, and normalized working directory. Entries SHALL be ordered by acceptance time, survive application restart, and remain available to every Agent Tab using the same history scope.

#### Scenario: Reopen the same context
- **WHEN** NiumaTerm restarts and the user opens an Agent Tab with the same execution target, backend, and working directory
- **THEN** the tab can recall the entries recorded before restart in newest-first navigation order

#### Scenario: Open a different backend
- **WHEN** Claude input was recorded for a working directory and the user opens a Codex Agent Tab for that directory
- **THEN** the Claude entries do not appear in the Codex input history

#### Scenario: Open a different execution target
- **WHEN** two local or remote execution targets use the same working-directory text
- **THEN** each target exposes only its own Agent input history

#### Scenario: Exceed the retention limit
- **WHEN** a history scope accepts its 101st retained entry
- **THEN** NiumaTerm removes the oldest entry and retains the newest 100 entries

### Requirement: Collapse adjacent duplicate text
NiumaTerm SHALL retain only one copy when two consecutively accepted entries in the same history scope have identical text. Identical text separated by another accepted entry SHALL remain independently recallable.

#### Scenario: Submit the same text twice consecutively
- **WHEN** the same composer text is accepted twice with no different accepted entry between them
- **THEN** history contains one newest copy of that text

#### Scenario: Repeat text after another entry
- **WHEN** the accepted sequence is `first`, `second`, then `first`
- **THEN** both `first` entries remain in their original navigation positions

### Requirement: Enter history browsing only from eligible composer state
When no higher-priority composer surface is handling navigation, Up SHALL recall the newest entry when the composer text is empty and its selection is collapsed. Non-empty text that was not produced by history recall MUST keep the existing editor behavior for Up and Down.

#### Scenario: Recall from an empty composer
- **WHEN** history is available, the composer is empty, and the user presses Up
- **THEN** NiumaTerm replaces the composer with the newest history entry and places the cursor at the end

#### Scenario: Preserve a non-empty draft
- **WHEN** the composer contains a user-authored draft that was not produced by history recall
- **THEN** Up and Down continue to perform their existing editor actions without replacing the draft

#### Scenario: Preserve selection editing
- **WHEN** the composer has a non-collapsed text selection
- **THEN** Up and Down do not start input history browsing

### Requirement: Navigate unchanged recalled entries
While the composer text remains identical to the active recalled entry and the selection is collapsed at the start or end of the whole text buffer, Up SHALL move to the next older entry and Down SHALL move to the next newer entry. Each recalled entry SHALL place the cursor at the end. Navigation MUST NOT wrap at either end.

#### Scenario: Move to an older entry
- **WHEN** the composer shows an unchanged recalled entry at a whole-buffer boundary and an older entry exists
- **THEN** pressing Up replaces it with the older entry and places the cursor at the end

#### Scenario: Move to a newer entry
- **WHEN** the composer shows an unchanged recalled entry at a whole-buffer boundary and a newer entry exists
- **THEN** pressing Down replaces it with the newer entry and places the cursor at the end

#### Scenario: Leave history after the newest entry
- **WHEN** the composer shows the newest unchanged recalled entry and the user presses Down
- **THEN** NiumaTerm clears the composer and ends history browsing

#### Scenario: Stay at the oldest entry
- **WHEN** the composer shows the oldest unchanged recalled entry and the user presses Up
- **THEN** the oldest entry remains selected and history does not wrap to the newest entry

#### Scenario: Edit recalled text
- **WHEN** the user changes recalled text
- **THEN** history browsing ends and subsequent Up or Down input follows normal editor behavior until the composer becomes empty and browsing starts again

#### Scenario: Move inside recalled text
- **WHEN** the cursor is inside recalled text rather than at the start or end of the whole buffer
- **THEN** Up and Down perform normal editor movement without changing the history position

### Requirement: Respect composer navigation priority
Command palettes, rewind selection, and recent-session selection SHALL receive Up and Down before input history. Input history SHALL receive navigation only when those surfaces decline the action.

#### Scenario: Navigate a command palette
- **WHEN** a command or skill palette is visible and the user presses Up or Down
- **THEN** the palette selection changes and the input history position does not change

#### Scenario: Navigate recent sessions
- **WHEN** the recent-session list owns keyboard navigation and the user presses Up or Down
- **THEN** the session selection changes and input history does not replace the composer

### Requirement: Restore text without transient submission state
History recall SHALL restore only the recorded composer text. It MUST NOT reactivate attachments, provider handles, or structured skill bindings that were associated with the original submission.

#### Scenario: Recall text that names a skill
- **WHEN** history recalls text beginning with a previously selected skill name
- **THEN** the text appears in the composer without restoring the earlier structured skill path

### Requirement: Keep input usable when persistence fails
A history persistence failure MUST NOT reject or delay an otherwise accepted Agent action. The accepted entry SHALL remain available to current-process history when possible, and NiumaTerm SHALL record a diagnostic for the failed durable write.

#### Scenario: Persistent storage is unavailable
- **WHEN** an Agent action is accepted but the history store cannot be updated
- **THEN** the Agent action proceeds, the current process can still recall the entry when possible, and the failure is recorded for diagnosis
