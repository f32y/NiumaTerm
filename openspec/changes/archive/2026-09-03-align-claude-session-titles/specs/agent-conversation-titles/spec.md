## ADDED Requirements

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

## REMOVED Requirements

### Requirement: Keep other Agent backends unchanged
**Reason**: Claude now gains the two-stage title behavior, so a requirement that excludes every non-Codex provider is no longer accurate.

**Migration**: The new Claude requirements define its behavior, while the added DeepSeek requirement preserves the remaining provider-specific behavior.
