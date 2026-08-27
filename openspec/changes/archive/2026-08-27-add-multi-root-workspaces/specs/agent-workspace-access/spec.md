## Purpose

Define one shared multi-directory input for Agent Tabs and truthful translation of that input to Codex, Claude Code, and DeepSeek Harness.

## ADDED Requirements

### Requirement: Agent sessions receive a workspace access snapshot
An Agent Tab SHALL start each conversation with an immutable workspace access snapshot containing the primary directory and ordered additional directories. Provider launch configuration SHALL remain separate from this workspace-owned input.

#### Scenario: Start from a multi-directory workspace
- **WHEN** an Agent Tab starts from a workspace whose directories are A, B, and C
- **THEN** its workspace access snapshot identifies A as primary and B and C as additional directories

#### Scenario: Restart after workspace editing
- **WHEN** the workspace directories change before the Agent Tab starts a replacement conversation
- **THEN** the replacement conversation receives a new snapshot from the current workspace

### Requirement: Harness support is explicit
Each registered harness SHALL declare whether it provides full multi-directory access or primary-only access. A harness registration SHALL NOT inherit another harness's behavior, and the Agent Tab SHALL expose a visible limitation whenever its harness cannot use every attached directory.

#### Scenario: Register a future harness
- **WHEN** a new harness kind is added
- **THEN** the build requires an explicit multi-directory capability choice for that harness

#### Scenario: Harness is primary-only
- **WHEN** a multi-directory workspace opens an Agent Tab whose harness declares primary-only access
- **THEN** the tab identifies which additional directories are unavailable before the user sends a prompt

### Requirement: Codex receives all workspace directories
For a Codex conversation, the App Server process and thread SHALL use the primary directory as `cwd`, and the thread's runtime workspace roots SHALL contain the primary and every additional directory in workspace order. In workspace-write mode, every runtime workspace root SHALL be included in the writable-root policy sent for turns.

#### Scenario: Start a Codex conversation
- **WHEN** Codex starts from workspace directories A, B, and C
- **THEN** the process and thread use A as `cwd` and the runtime root list is A, B, C

#### Scenario: Run Codex in workspace-write mode
- **WHEN** a Codex turn starts in workspace-write mode with directories A, B, and C
- **THEN** its workspace-write policy includes A, B, and C as writable roots

#### Scenario: Resume a Codex conversation
- **WHEN** a Codex conversation is resumed from a multi-directory workspace
- **THEN** the resumed thread retains its persisted identity and subsequent turns use the current workspace access snapshot

### Requirement: Claude Code receives additional directories through its CLI
For a Claude Code conversation, the process SHALL start in the primary directory and its launch arguments SHALL include every additional directory through fixed `--add-dir` argument boundaries. The same arguments SHALL be supplied for new and resumed conversations.

#### Scenario: Start Claude Code with additional directories
- **WHEN** Claude Code starts from workspace directories A, B, and C
- **THEN** its process starts in A and its launch arguments carry B and C as additional directories in that order

#### Scenario: Resume Claude Code with additional directories
- **WHEN** a Claude Code session is resumed from the same workspace
- **THEN** the resume id and the current additional-directory arguments are both supplied without changing the session id

#### Scenario: Directory name contains spaces
- **WHEN** an additional directory path contains spaces or shell metacharacters
- **THEN** the path remains one native argument and is not interpolated into a command string

### Requirement: DeepSeek Harness reduces safely to its primary directory
While the installed DeepSeek Harness exposes one workspace root per session, NiumaTerm SHALL create the session with the primary directory only. It SHALL NOT claim that additional directories are accessible and SHALL NOT automatically select danger-full-access or a broader common ancestor to approximate multi-directory access.

#### Scenario: Start DeepSeek from a multi-directory workspace
- **WHEN** DeepSeek Harness starts from workspace directories A, B, and C under workspace-write mode
- **THEN** the harness session uses A as its workspace root and the Agent Tab reports that B and C are unavailable to this harness

#### Scenario: User selected broader permission independently
- **WHEN** the user explicitly selects a broader DeepSeek permission preset
- **THEN** NiumaTerm sends that selected preset but does not relabel the harness as providing selected-root isolation

#### Scenario: DeepSeek gains native support
- **WHEN** a supported installed DeepSeek Harness version reports a native multi-root policy
- **THEN** the adapter may declare full multi-directory access and pass all selected roots without changing the Workspace model or other harness adapters

### Requirement: No adapter silently widens filesystem access
Translating a workspace access snapshot SHALL never grant a directory outside the selected roots merely to make one harness behave like another. If exact selected-root access is unavailable, the adapter SHALL reduce capability visibly or reject the start with an actionable error.

#### Scenario: Common ancestor contains unrelated directories
- **WHEN** selected roots have a common ancestor that also contains unselected directories
- **THEN** no adapter substitutes that ancestor as the writable root

#### Scenario: Multi-root request is rejected by a provider
- **WHEN** a provider version rejects the selected-root request
- **THEN** the Agent Tab reports the incompatibility and does not retry with broader filesystem access

### Requirement: Agent input history follows the workspace root set
Local Agent input history SHALL be scoped by harness kind, primary directory, and ordered additional directories. Two workspaces that share a primary directory but attach different additional directories SHALL NOT share input-history navigation.

#### Scenario: Root sets differ
- **WHEN** two Codex workspaces have the same primary directory but different additional directories
- **THEN** prompts recorded in one root set do not appear in the other root set's input-history navigation

#### Scenario: Root set is equivalent
- **WHEN** two workspace access snapshots normalize to the same ordered directories for the same harness
- **THEN** they resolve to the same local input-history scope
