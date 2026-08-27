# workspace-multi-directory

## Purpose

Define how one NiumaTerm workspace owns, edits, restores, and routes across a primary directory and additional local directories.

## Requirements

### Requirement: Workspace owns one primary directory
Each normal workspace SHALL own exactly one primary directory and MAY own an ordered list of additional directories. The primary and additional directories SHALL be unique under the platform's path identity rules.

#### Scenario: Create a workspace with several directories
- **WHEN** the user creates a workspace with directory A as primary and directories B and C as additional directories
- **THEN** the workspace records A as primary and retains B and C in the user's order

#### Scenario: Reject an equivalent directory
- **WHEN** the user adds a path that identifies a directory already owned by the workspace after path normalization
- **THEN** the workspace remains unchanged and the UI reports that the directory is already attached

#### Scenario: Keep at least one directory
- **WHEN** an edit would leave a normal workspace with no directory
- **THEN** the edit is rejected and the existing primary directory remains selected

### Requirement: Workspace directories are editable
The workspace editor SHALL let the user add one or more directories, remove an additional directory, and make any attached directory primary. Making a directory primary SHALL preserve the relative order of all other directories.

#### Scenario: Add several directories
- **WHEN** the user selects multiple valid directories from the Add folder action
- **THEN** every new directory is appended once and the primary directory does not change

#### Scenario: Make an additional directory primary
- **WHEN** the user makes directory C primary in a workspace whose ordered directories are A, B, C, and D
- **THEN** the workspace order becomes C, A, B, D and C supplies primary-directory behavior

#### Scenario: Remove the primary directory
- **WHEN** the user removes the primary directory from a workspace that still has additional directories
- **THEN** the first remaining directory becomes primary and the remaining order is preserved

### Requirement: Newly attached directories are validated
Before a user-selected directory is attached, the application SHALL resolve it to an absolute existing directory on a background executor. A path that is missing, is not a directory, or cannot be resolved SHALL NOT change the workspace.

#### Scenario: Directory selection succeeds
- **WHEN** the directory picker returns an existing directory that the application can resolve
- **THEN** its resolved absolute path is attached to the workspace

#### Scenario: Directory selection is unusable
- **WHEN** the selected path is missing, is a file, or cannot be resolved
- **THEN** the workspace remains unchanged and the UI reports why the path was not attached

### Requirement: Primary directory owns default behavior
New terminal tabs and Agent Tabs SHALL start in the workspace's primary directory. Workspace-generated labels, default relative-path resolution, and workspace-level Git branch display SHALL also use the primary directory. Additional directories SHALL NOT replace these defaults merely because a file under one becomes active.

#### Scenario: Open a new terminal tab
- **WHEN** a workspace has primary directory A and additional directory B and the user opens a terminal tab
- **THEN** the terminal process starts in A

#### Scenario: Open a new Agent Tab
- **WHEN** a workspace has primary directory A and additional directory B and the user opens an Agent Tab
- **THEN** the Agent Tab records A as its working directory and B as an additional directory

### Requirement: Workspace directory edits use launch snapshots
Each terminal or Agent Tab SHALL retain the directory configuration it received when its current process or conversation started. Editing the parent workspace SHALL affect future tabs and replacement conversations without mutating a process or conversation already running.

#### Scenario: Edit a workspace while an Agent Tab is running
- **WHEN** the user adds directory C while an existing Agent Tab is running with directories A and B
- **THEN** the running conversation retains A and B and a newly opened Agent Tab receives A, B, and C

#### Scenario: Start a replacement conversation after editing
- **WHEN** a tab replaces its conversation after its workspace directory list changed
- **THEN** the replacement conversation receives the current workspace directory list

### Requirement: New Tab menu selects terminal profile and directory
The New Tab menu shared by the horizontal tab bar and workspace sidebar SHALL keep one top-level entry for each configured terminal Profile with a non-empty command. Selecting a top-level terminal entry SHALL start that Profile in the active workspace's primary directory. When the workspace has additional directories, the menu SHALL add one localized `More` submenu after the top-level terminal entries and before Agent Profile entries. The submenu SHALL contain every valid terminal Profile and attached-directory combination, including primary-directory combinations.

#### Scenario: Top-level terminal entry uses primary directory
- **WHEN** a workspace owns primary directory A and additional directory B and the user selects terminal Profile P from the top level
- **THEN** terminal Profile P starts in A

#### Scenario: Single-directory workspace has no More submenu
- **WHEN** the active workspace owns only primary directory A
- **THEN** the New Tab menu shows the existing top-level terminal and Agent Profile entries and no `More` submenu

#### Scenario: More submenu contains the Cartesian product
- **WHEN** the application has terminal Profiles P1 and P2 and the active workspace owns directories A, B, and C
- **THEN** the `More` submenu contains exactly six launch entries covering P1-A, P1-B, P1-C, P2-A, P2-B, and P2-C

#### Scenario: Combination entries have deterministic order
- **WHEN** the `More` submenu is built for terminal Profiles P1 and P2 and directories A, B, and C in workspace order
- **THEN** entries are ordered P1-A, P1-B, P1-C, P2-A, P2-B, P2-C

#### Scenario: Select an additional-directory combination
- **WHEN** the user selects terminal Profile P combined with additional directory B
- **THEN** a new terminal tab starts Profile P with B as its process working directory without changing the workspace primary directory

#### Scenario: Combination path is unambiguous
- **WHEN** two attached directories have the same final component
- **THEN** each combination entry includes the terminal Profile name and the directory's full path so the user can distinguish them

#### Scenario: Saved directory is unavailable
- **WHEN** an attached directory is unavailable while the `More` submenu is open
- **THEN** its Profile combinations remain visible but disabled and no terminal process is launched from them

#### Scenario: Agent Profile entries remain unchanged
- **WHEN** a multi-directory workspace opens the New Tab menu
- **THEN** Agent Profile entries remain at the top level after their existing separator and do not expand into per-directory combinations

#### Scenario: Keyboard shortcut uses default Profile and primary directory
- **WHEN** the user invokes the existing New Tab keyboard shortcut in a multi-directory workspace
- **THEN** the default terminal Profile starts directly in the primary directory without opening the menu

### Requirement: Workspace directories persist compatibly
The application SHALL persist the primary directory in the existing workspace `cwd` field and additional directories in a default-empty list. Restoring state that has no additional-directory list SHALL produce a single-directory workspace with unchanged behavior. A persisted directory that is temporarily unavailable SHALL remain visible and SHALL NOT prevent other workspace state from restoring.

#### Scenario: Restore an older snapshot
- **WHEN** local state contains a workspace `cwd` and no additional-directory field
- **THEN** the workspace restores with that `cwd` as its only directory

#### Scenario: Round-trip a multi-directory workspace
- **WHEN** a workspace with one primary and two additional directories is saved and restored
- **THEN** the same primary selection and additional-directory order are restored

#### Scenario: Restore a missing additional directory
- **WHEN** an additional directory no longer exists when the application restores local state
- **THEN** the workspace and its other directories restore, and the unavailable directory is visibly marked until removed or available again

### Requirement: Path routing considers every workspace directory
Commands that locate the best workspace for a filesystem path SHALL consider primary and additional directories. The most specific ancestor match SHALL win; an equal match on a primary directory SHALL outrank an equal match on an additional directory, followed by existing workspace order.

#### Scenario: Target belongs to an additional directory
- **WHEN** a target path is under directory B and B is an additional directory of workspace W
- **THEN** workspace W is eligible as the target workspace

#### Scenario: Nested roots compete
- **WHEN** one workspace owns a root that is a longer ancestor of the target than another workspace's root
- **THEN** the workspace with the longer matching root is selected

#### Scenario: Primary and additional roots tie
- **WHEN** the same normalized path is a primary directory in one workspace and an additional directory in another workspace
- **THEN** the workspace that owns it as primary is selected
