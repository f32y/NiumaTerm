# agent-session-history

When an Agent Tab opens, it displays historical sessions for the current working directory and lets the user restore and continue a conversation with either Claude Code through its stream-json CLI or Codex through app-server.

## ADDED Requirements

### Requirement: Show historical sessions in the empty state
When Agent Tab is empty, meaning the transcript has no item and the user has neither sent a message nor selected a session to restore, the system SHALL display historical sessions for the tab's current cwd above the composer. The list SHALL be ordered by last-active time in descending order.

#### Scenario: A new Agent Tab has historical sessions
- **WHEN** the user creates an Agent Tab and at least one historical session for the same backend exists under that cwd
- **THEN** the history list appears above the composer with the most recently active session first

#### Scenario: A new Agent Tab has no historical session
- **WHEN** the user creates an Agent Tab and no historical session for the same backend exists under that cwd
- **THEN** the entire list region remains hidden and the UI matches the existing empty state

#### Scenario: History loads asynchronously
- **WHEN** the history scan or request has not completed
- **THEN** the UI remains usable and accepts composer input; the list appears after loading, while a load failure hides the list and records the reason in the log

### Requirement: Display session details in each row
Each row SHALL display a session title derived from the first user message and truncated to one line, plus a relative last-active time. When the session includes Git branch information, the row SHALL display the branch name. When no user message can provide a title, the row SHALL use the first eight characters of the session id.

#### Scenario: Normal session row
- **WHEN** the first user message is "fix the login bug", the session was active five minutes ago, and its branch is dev
- **THEN** the row shows the truncated prompt, branch `dev`, and a relative time styled as `5m`

#### Scenario: Session has no user message
- **WHEN** no user text message can be parsed from a historical session file
- **THEN** the row title is the first eight characters of the session id

### Requirement: Use a scrolling virtual list
The list SHALL show at most 10 rows at once by default and allow scrolling to additional rows. It SHALL use virtualization and render only the visible range so hundreds of sessions remain responsive.

#### Scenario: More than 10 historical sessions exist
- **WHEN** a cwd has 50 historical sessions
- **THEN** the list remains 10 rows high, the user can scroll through the other 40, and each frame renders only visible rows

### Requirement: Hide the list after the user proceeds
After the user selects a historical session or sends the first new message without selecting one, the system SHALL hide the history list for the remainder of that tab's lifetime.

#### Scenario: Send a new message directly
- **WHEN** the user enters and sends a message while the list is visible
- **THEN** the list disappears and a new session starts normally

#### Scenario: Select a session to restore
- **WHEN** the user selects a historical session
- **THEN** the list disappears and the restore flow begins

### Requirement: Enumerate Claude Code sessions
For the Claude Code backend, the system SHALL enumerate historical sessions by scanning `~/.claude/projects/<munged-cwd>/*.jsonl`, where munged-cwd replaces every non-alphanumeric character in cwd with `-`. It SHALL derive the session id from the filename, last-active time from mtime, and title and branch from the first record near the start of the file whose `type == "user"` and whose content includes text. Scanning and title parsing SHALL run on a background thread and read only a bounded prefix of each file.

#### Scenario: Enumerate Claude history
- **WHEN** cwd is `C:\Workspace\NiumaTerm` and `~/.claude/projects/C--Workspace-NiumaTerm/` contains several `<uuid>.jsonl` files
- **THEN** each JSONL produces one row with the filename UUID as its id, ordered by mtime descending

#### Scenario: Project directory is missing
- **WHEN** `~/.claude/projects/` has no directory for the cwd
- **THEN** the list is empty, the UI treats it as no history, and no error is shown

### Requirement: Restore a Claude Code session
After the user selects a Claude history row, the system SHALL start Claude with an added `--resume <session-id>` argument while retaining the existing spawn arguments and using the tab cwd. At the same time, it SHALL parse historical messages from that session's JSONL and prefill the transcript. The restored session SHALL retain its original session id as reported by `session_id` in init, and later messages SHALL append to the original JSONL.

#### Scenario: Restore succeeds
- **WHEN** the user selects session `8365ddfc-…` and sends a new message
- **THEN** Claude starts with `--resume 8365ddfc-…`, the transcript begins with replayed history, the new response follows it, and the existing file under `~/.claude` is appended instead of creating a new file

#### Scenario: Restore fails
- **WHEN** Claude returns an error such as "No conversation found" because the session file was deleted
- **THEN** the transcript displays a restore error and the composer remains usable

### Requirement: Limit Claude Code transcript replay
When replaying JSONL, the system SHALL render user and assistant text messages. It SHALL skip tool calls, hook output, sidechain records, and queue-operation records or render them as folded placeholders, and SHALL NOT render every record in full.

#### Scenario: History contains tool calls
- **WHEN** a restored session has 20 tool calls and five user and assistant exchanges
- **THEN** the transcript shows the five exchanges and either combines tool calls into placeholders or omits them, with render cost proportional to the number of conversation turns

### Requirement: Enumerate Codex sessions
For the Codex backend, after app-server initialization the system SHALL request historical sessions through `thread/list` with the current tab cwd as `cwd`, `sortKey: "recency_at"`, and default sourceKinds for interactive sources only. It SHALL map row fields as id from `id`, title from `name` or `preview`, time from `recencyAt`, and branch from `gitInfo` when available. It SHALL load additional pages on demand through `nextCursor`.

#### Scenario: Enumerate Codex history
- **WHEN** a Codex Agent Tab opens and app-server is ready
- **THEN** the Client sends `thread/list` filtered by cwd, each returned thread produces a row, and subagent threads are excluded by the default filters

### Requirement: Restore a Codex session
After the user selects a Codex history row, the system SHALL call `thread/resume` with `threadId` instead of `thread/start` and SHALL rebuild the transcript from `thread.turns[].items` in the response. The model and reasoning effort SHALL use restored persisted values and update the settings row. Replay SHALL match the Claude scope: render user and assistant text fully and fold or omit command and tool items.

#### Scenario: Restore a Codex thread successfully
- **WHEN** the user selects thread `thr_123`
- **THEN** the Client sends `thread/resume {threadId: "thr_123"}`, rebuilds the transcript from response turns, and appends later `turn/start` operations to that thread

#### Scenario: Resume request fails
- **WHEN** `thread/resume` returns an error because the thread was deleted or damaged
- **THEN** the transcript shows an error, the tab returns to its unstarted state, and the composer can start a new session

### Requirement: Match the composer visual style
The history list SHALL reuse the rounded composer shell hierarchy as a block above the input, separated by a hairline in the same structure as the approval panel. Rows SHALL follow the existing settings-row control style with small text, subdued foreground colors, a brighter hover color, a left-aligned truncated title, and a right-aligned relative time in tabular digits.

#### Scenario: Visual consistency
- **WHEN** the history list and approval panel are compared side by side
- **THEN** they share the rounded outer container and divider treatment, and list rows match the style of controls in the bottom settings row
