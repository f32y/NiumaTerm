# Agent Conversation Titles Specification

## Purpose

Define how Codex Agent Tabs obtain concise conversation titles while keeping the interface responsive and protecting user-authored names.

## Requirements

### Requirement: Show a provisional title immediately
When the first ordinary user prompt is accepted for a new Codex conversation, the system SHALL derive a single-line provisional title from the prompt, SHALL display it without waiting for the agent turn to complete, and SHALL limit it to 60 characters. The provisional title SHALL remain local until title generation settles. A blank prompt or a local slash command SHALL NOT start title generation.

#### Scenario: First prompt starts a conversation
- **WHEN** a new Codex Agent Tab accepts a multi-line ordinary prompt
- **THEN** the Tab displays a whitespace-normalized provisional title immediately and the primary turn starts normally

#### Scenario: Local command runs before the first prompt
- **WHEN** a new Codex Agent Tab executes a local slash command
- **THEN** the Tab keeps its profile-derived title and waits for an ordinary prompt

### Requirement: Generate the durable title in isolation
The system SHALL generate a concise title asynchronously in a bounded, ephemeral, read-only Codex thread. The title-generation thread SHALL NOT add transcript items, change running state, request approval, or otherwise alter the primary conversation. A generated title SHALL contain at most 36 characters and SHALL use the language of the opening prompt.

#### Scenario: Generation overlaps the primary turn
- **WHEN** the first primary turn is running and title generation completes
- **THEN** only the Tab title changes and the primary transcript and working state remain unchanged

#### Scenario: Generation exceeds its time limit
- **WHEN** the title-generation thread does not complete within 30 seconds
- **THEN** the system stops waiting for it and retains the provisional title

### Requirement: Persist a generated title without overwriting newer input
The system SHALL apply and persist a generated title only when the same primary conversation is still active and its provisional title has not been superseded. Persistence SHALL use the Codex thread naming operation. A user-authored rename SHALL cancel any pending generated replacement, remain visible, and be persisted as the thread name.

#### Scenario: Generated title replaces its provisional title
- **WHEN** generation returns a valid title while the original provisional title is still current
- **THEN** the Tab displays the generated title and the Codex thread stores that title

#### Scenario: User renames while generation is running
- **WHEN** the user commits a Tab rename before the generated title arrives
- **THEN** the user-authored name remains visible and stored, and the later generated result is ignored

#### Scenario: Conversation changes before generation finishes
- **WHEN** the Tab starts or restores another conversation before the generated title arrives
- **THEN** the result for the previous conversation does not change the Tab or the newly active Codex thread

### Requirement: Fall back to the provisional title
If title generation cannot start, fails, times out, or returns an empty or invalid value, the system SHALL retain and persist the provisional title without surfacing the generation failure as a primary conversation error.

#### Scenario: Title model fails
- **WHEN** the isolated title-generation turn fails while the primary turn remains usable
- **THEN** the provisional title remains visible, is stored for history, and no error row is added to the primary transcript

### Requirement: Restore stored Codex titles
When a Codex thread is restored, the system SHALL display its stored non-empty name and SHALL treat the conversation as already named. A later follow-up prompt SHALL NOT replace the restored name through first-prompt title generation.

#### Scenario: Resume a named thread
- **WHEN** a history entry restores a Codex thread with a stored name
- **THEN** the Tab displays that name and the next user prompt leaves it unchanged

### Requirement: Keep other Agent backends unchanged
The two-stage title flow SHALL apply only to Codex Agent Tabs. Claude Code and DeepSeek title handling SHALL retain their existing provider-specific behavior.

#### Scenario: Claude conversation starts
- **WHEN** a Claude Code Agent Tab accepts its first prompt
- **THEN** it continues to obtain its title through the existing Claude title path
