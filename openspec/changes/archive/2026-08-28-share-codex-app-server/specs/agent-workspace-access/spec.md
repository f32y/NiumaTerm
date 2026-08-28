## MODIFIED Requirements

### Requirement: Codex receives all workspace directories
For a Codex conversation, the thread SHALL use the primary directory as `cwd`, and the thread's runtime workspace roots SHALL contain the primary and every additional directory in workspace order. Every Codex request whose behavior depends on a working directory, including thread creation, history filtering, and skill discovery, SHALL carry the applicable absolute primary directory instead of relying on the shared App Server process working directory. In workspace-write mode, every runtime workspace root SHALL be included in the writable-root policy sent for turns.

#### Scenario: Start a Codex conversation
- **WHEN** Codex starts from workspace directories A, B, and C
- **THEN** the thread uses absolute directory A as `cwd`, the runtime root list is A, B, C, and the shared process does not change its working directory for that conversation

#### Scenario: Start a single-directory Codex conversation
- **WHEN** Codex starts from workspace directory A on an app-server shared with another workspace
- **THEN** `thread/start` explicitly names absolute directory A even though there are no additional roots

#### Scenario: Query workspace-scoped Codex data
- **WHEN** a Codex tab requests current-directory history or skills
- **THEN** the request carries the tab's absolute primary directory and does not use `.` as a proxy for process state

#### Scenario: Run Codex in workspace-write mode
- **WHEN** a Codex turn starts in workspace-write mode with directories A, B, and C
- **THEN** its workspace-write policy includes A, B, and C as writable roots

#### Scenario: Resume a Codex conversation
- **WHEN** a Codex conversation is resumed from a multi-directory workspace
- **THEN** the resumed thread retains its persisted identity and subsequent turns use the current workspace access snapshot
