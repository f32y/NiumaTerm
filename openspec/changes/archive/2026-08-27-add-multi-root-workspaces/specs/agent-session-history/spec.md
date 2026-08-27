## MODIFIED Requirements

### Requirement: Show historical sessions in the empty state
When Agent Tab is empty, meaning the transcript has no item and the user has neither sent a message nor selected a session to restore, the system SHALL display historical sessions for the tab's primary directory above the composer. Additional workspace directories SHALL NOT widen this provider-history query. The list SHALL be ordered by last-active time in descending order.

#### Scenario: A new Agent Tab has historical sessions
- **WHEN** the user creates an Agent Tab and at least one historical session for the same backend exists under the workspace's primary directory
- **THEN** the history list appears above the composer with the most recently active session first

#### Scenario: History exists only for an additional directory
- **WHEN** a multi-directory workspace has no historical session under its primary directory but has sessions under an additional directory
- **THEN** those additional-directory sessions do not appear in the current-directory list

#### Scenario: A new Agent Tab has no historical session
- **WHEN** the user creates an Agent Tab and no historical session for the same backend exists under the primary directory
- **THEN** the entire list region remains hidden and the UI matches the existing empty state

#### Scenario: History loads asynchronously
- **WHEN** the history scan or request has not completed
- **THEN** the UI remains usable and accepts composer input; the list appears after loading, while a load failure hides the list and records the reason in the log

### Requirement: Enumerate Claude Code sessions
For the Claude Code backend, the system SHALL enumerate historical sessions by scanning `~/.claude/projects/<munged-primary-cwd>/*.jsonl`, where munged-primary-cwd replaces every non-alphanumeric character in the workspace's primary directory with `-`. It SHALL derive the session id from the filename, last-active time from mtime, and title and branch from the first record near the start of the file whose `type == "user"` and whose content includes text. Scanning and title parsing SHALL run on a background thread and read only a bounded prefix of each file.

#### Scenario: Enumerate Claude history
- **WHEN** the primary directory is `C:\Workspace\NiumaTerm` and `~/.claude/projects/C--Workspace-NiumaTerm/` contains several `<uuid>.jsonl` files
- **THEN** each JSONL produces one row with the filename UUID as its id, ordered by mtime descending

#### Scenario: Additional directories have Claude history
- **WHEN** an additional directory has its own Claude transcript directory
- **THEN** those rows remain outside the current-directory list for this workspace

#### Scenario: Project directory is missing
- **WHEN** `~/.claude/projects/` has no directory for the primary directory
- **THEN** the list is empty, the UI treats it as no history, and no error is shown

### Requirement: Restore a Claude Code session
After the user selects a Claude history row, the system SHALL start Claude with an added `--resume <session-id>` argument while retaining the existing spawn arguments, using the workspace's primary directory, and attaching the current additional directories. At the same time, it SHALL parse historical messages from that session's JSONL and prefill the transcript. The restored session SHALL retain its original session id as reported by `session_id` in init, and later messages SHALL append to the original JSONL.

#### Scenario: Restore succeeds
- **WHEN** the user selects session `8365ddfc-…` from a workspace with additional directories and sends a new message
- **THEN** Claude starts with `--resume 8365ddfc-…`, receives the current additional directories, the transcript begins with replayed history, and the existing file under `~/.claude` is appended instead of creating a new file

#### Scenario: Workspace directories changed since the session was created
- **WHEN** a Claude session is resumed after the workspace's additional directories changed
- **THEN** the restored conversation keeps its session id and receives the workspace's current additional directories

#### Scenario: Restore fails
- **WHEN** Claude returns an error such as "No conversation found" because the session file was deleted
- **THEN** the transcript displays a restore error and the composer remains usable

### Requirement: Enumerate Codex sessions
For the Codex backend, after App Server initialization the system SHALL request historical sessions through `thread/list` with the workspace's primary directory as `cwd`, `sortKey: "recency_at"`, and default sourceKinds for interactive sources only. Additional directories SHALL NOT widen the history query. It SHALL map row fields as id from `id`, title from `name` or `preview`, time from `recencyAt`, and branch from `gitInfo` when available. It SHALL load additional pages on demand through `nextCursor`.

#### Scenario: Enumerate Codex history
- **WHEN** a Codex Agent Tab opens in a multi-directory workspace and App Server is ready
- **THEN** the Client sends `thread/list` filtered by the primary directory, each returned thread produces a row, and subagent threads are excluded by the default filters

### Requirement: Restore a Codex session
After the user selects a Codex history row, the system SHALL call `thread/resume` with `threadId` instead of `thread/start`, SHALL apply the current workspace access snapshot to subsequent turns, and SHALL rebuild the transcript from `thread.turns[].items` in the response. The model and reasoning effort SHALL use restored persisted values and update the settings row. Replay SHALL reuse live item parsing: render user and assistant text in full, and retain each reasoning, command, file-change, and other tool item's id, type, title, output, state, exit code, and available diff. Consecutive work items MAY be grouped only in the UI and SHALL NOT be reduced to counts or omitted from replay data.

#### Scenario: Restore a Codex thread successfully
- **WHEN** the user selects thread `thr_123` from a multi-directory workspace
- **THEN** the Client sends `thread/resume {threadId: "thr_123"}`, rebuilds the transcript from response turns, and uses the current workspace directories for later turns

#### Scenario: Resume request fails
- **WHEN** `thread/resume` returns an error because the thread was deleted or damaged
- **THEN** the transcript shows an error, the tab returns to its unstarted state, and the composer can start a new session

## ADDED Requirements

### Requirement: Enumerate and restore DeepSeek sessions by primary directory
For DeepSeek Harness, the system SHALL filter the harness's session list by the workspace's primary directory. Restoring a matching session SHALL keep that primary directory and SHALL present the same additional-directory limitation as a new DeepSeek conversation.

#### Scenario: Enumerate DeepSeek history
- **WHEN** a DeepSeek Agent Tab opens in a multi-directory workspace
- **THEN** only harness sessions whose recorded `cwd` matches the primary directory appear in the current-directory list

#### Scenario: Restore DeepSeek history
- **WHEN** the user restores a DeepSeek session from a multi-directory workspace
- **THEN** the session retains its primary-directory identity and the tab reports that the current additional directories are unavailable to the harness
