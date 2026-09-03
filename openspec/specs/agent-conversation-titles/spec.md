# Agent Conversation Titles Specification

## Purpose

Define how Codex and Claude Agent Tabs obtain concise conversation titles while keeping the interface responsive and protecting user-authored names.

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

### Requirement: Generate Claude conversation titles
When the first ordinary user prompt is accepted for a new Claude conversation, the system SHALL display a single-line provisional title without waiting for the primary turn and SHALL request one persisted model-generated title through Claude Code's title operation. The provisional title SHALL contain the first six whitespace-delimited words and SHALL be limited to 60 characters. The title request SHALL contain at most 2,000 characters from the trimmed opening prompt and SHALL be made at most once for that conversation. A blank prompt or local slash command SHALL NOT start title generation.

Title generation SHALL remain independent of the primary transcript and running state. A non-empty generated title SHALL replace the provisional title. If generation cannot start, fails, or returns no title, the provisional title SHALL remain. A user-authored title SHALL remain authoritative over a generated result.

#### Scenario: First Claude prompt starts a conversation
- **WHEN** a new Claude Agent Tab accepts an ordinary prompt containing more than six words
- **THEN** the Tab immediately displays a provisional title made from the first six words and starts the primary turn normally

#### Scenario: Claude returns a generated title
- **WHEN** Claude Code returns a non-empty title for the opening prompt
- **THEN** the Tab replaces the provisional title and Claude Code persists the generated title without adding a transcript row

#### Scenario: Claude returns no title
- **WHEN** Claude Code rejects title generation or returns no usable title
- **THEN** the provisional title remains and later prompts do not start another automatic naming attempt

#### Scenario: User renames while Claude is generating
- **WHEN** the user renames the Tab before Claude Code returns a generated title
- **THEN** the user-authored title remains visible and stored

#### Scenario: Claude local command precedes the first prompt
- **WHEN** a new Claude Agent Tab executes a local slash command before an ordinary prompt
- **THEN** the Tab keeps its profile-derived title and waits for an ordinary prompt

### Requirement: Restore Claude conversation titles
Claude session history SHALL choose the newest user-authored title when one exists, otherwise the newest persisted model-generated title, otherwise the bounded provisional title derived from the opening prompt, and otherwise a session-ID fallback. A resumed Claude conversation SHALL be treated as already named so a follow-up prompt cannot replace its restored title through first-prompt generation.

#### Scenario: Generated Claude title is listed
- **WHEN** a Claude transcript contains an `ai-title` record and no `custom-title` record
- **THEN** session history displays the generated title

#### Scenario: User title outranks generated title
- **WHEN** a Claude transcript contains both `custom-title` and `ai-title` records
- **THEN** session history displays the newest user-authored title regardless of record order

#### Scenario: Untitled Claude session is listed
- **WHEN** a Claude transcript contains an opening prompt but no stored title metadata
- **THEN** session history displays the same bounded provisional title used for a new live conversation

#### Scenario: Resumed Claude conversation receives a follow-up
- **WHEN** the user resumes an existing Claude conversation and submits another prompt
- **THEN** the restored title remains and no first-prompt title request is made

### Requirement: Keep DeepSeek title handling unchanged
The Claude title flow SHALL NOT change DeepSeek title handling. DeepSeek SHALL retain its existing provider-specific behavior.

#### Scenario: DeepSeek conversation starts
- **WHEN** a DeepSeek Agent Tab accepts its first prompt
- **THEN** it continues to obtain its title through the existing DeepSeek title path
