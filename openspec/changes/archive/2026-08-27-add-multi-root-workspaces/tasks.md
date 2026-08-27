## 1. Workspace Domain and Persistence

- [x] 1.1 Move `crates/app/src/workspace.rs` to `workspace/mod.rs`, move its tests to `workspace/tests.rs`, and preserve existing `crate::workspace` imports and behavior.
- [x] 1.2 Add `WorkspaceRoots` with primary/additional accessors, ordered iteration, platform-aware identity, and mutation outcomes for add, remove, and make-primary operations.
- [x] 1.3 Add unit coverage for duplicate paths, primary promotion, preserved additional order, nested roots, Windows path spellings, and rejection of an empty normal workspace.
- [x] 1.4 Add default-empty `additional_cwds` persistence to `WorkspaceState`, including old-snapshot, new round-trip, and unknown-field rollback coverage.
- [x] 1.5 Update session restore and save plumbing so normal workspaces retain all roots while the Settings workspace remains location-free and unavailable saved roots do not block restore.

## 2. Workspace Routing and Default Behavior

- [x] 2.1 Update `Workspace`, `WorkspaceSummary`, constructors, and manager accessors to expose primary and additional directories while retaining primary-cwd compatibility helpers.
- [x] 2.2 Refactor exact and best path matching across every root with longest-root, primary-root, and workspace-order precedence, and cover each tie case.
- [x] 2.3 Add an explicit-directory terminal Profile launch path, keep the existing top-level and shortcut launch path anchored to primary, and reuse the explicit path where an existing flow already chose a directory.
- [x] 2.4 Keep workspace labels, relative link resolution, and Git branch polling anchored to the primary directory with focused regression tests.
- [x] 2.5 Ensure temporary workspace adoption, pinning, reordering, close behavior, and open-in-best-workspace continue to operate on stable workspace identity after roots are added.

## 3. Workspace Directory Editor and Sidebar

- [x] 3.1 Add a dedicated workspace-directory dialog module that supports multi-select Add folder, remove, and make-primary actions without performing filesystem work on the UI thread.
- [x] 3.2 Validate and canonicalize newly selected directories, display duplicate or unusable-path outcomes in the dialog, and retain restored unavailable directories with a visible status.
- [x] 3.3 Use the directory editor for new workspace creation and add an Edit workspace entry for existing normal workspaces.
- [x] 3.4 Render the primary path plus a stable `+N` additional-directory summary in sidebar rows, with ordered full paths in tooltip and accessibility text.
- [x] 3.5 Extend the shared New Tab menu with a localized `More` submenu shown only for multi-directory workspaces, containing the Profile-major terminal Profile and root combinations before Agent Profile entries.
- [x] 3.6 Disable combinations for unavailable roots, use Profile plus full-path labels, and preserve top-level Agent Profile and keyboard-shortcut behavior.
- [x] 3.7 Add pure menu-projection and launch tests for Cartesian-product count, ordering, empty-command omission, exact selected cwd, duplicate basenames, unavailable roots, and both New Tab button surfaces.
- [x] 3.8 Add English source strings and every maintained locale entry for the editor, primary badge, additional count, `More` submenu, unavailable path, validation errors, and session-access notice.

## 4. Shared Agent Workspace Input

- [x] 4.1 Add provider-neutral `AgentWorkspace` and `MultiRootAccess` types in `nmt_agent_utils`, with primary-first iteration and a normalized input-history signature.
- [x] 4.2 Pass configured workspace roots from Shell to `AgentPane` and pass an immutable start snapshot from `AgentPane` through `Backend::spawn` to every adapter.
- [x] 4.3 Update open Agent Tabs with edited configured roots for their next conversation while proving that a running backend retains its active snapshot.
- [x] 4.4 Scope local Agent input history by harness and normalized root set, including workspaces that share a primary directory but differ in additions.
- [x] 4.5 Extend the no-default harness capability table with explicit full or primary-only multi-root access and render the primary-only notice before first prompt.

## 5. Codex Adapter

- [x] 5.1 Start Codex App Server in the primary directory and add primary `cwd` plus ordered `runtimeWorkspaceRoots` to new-thread request construction.
- [x] 5.2 Emit the full workspace-write policy with every selected root in `writableRoots`, while retaining existing read-only and danger-full-access behavior.
- [x] 5.3 Reapply the current Agent workspace snapshot to resumed-thread turns while keeping `thread/list` current-directory history filtered by primary cwd.
- [x] 5.4 Add request-shape and session tests for single-root compatibility, multi-root start, workspace-write turns, resume, paths with spaces, and provider rejection without broader retry.

## 6. Claude Code Adapter

- [x] 6.1 Keep Claude Code process cwd on the primary directory and append one native `--add-dir` group containing every additional directory for new sessions.
- [x] 6.2 Apply the same additional-directory arguments when spawning with `--resume` without changing transcript lookup or session identity.
- [x] 6.3 Add launch tests that capture exact argument boundaries, ordering, spaces, metacharacters, single-root omission, and combined resume arguments.

## 7. DeepSeek Harness Adapter

- [x] 7.1 Accept `AgentWorkspace` in DeepSeek session creation, send only the primary cwd to `session.create`, and keep shared-host identity based only on launch configuration.
- [x] 7.2 Show the primary-only limitation for multi-root workspaces and ensure permission-preset changes never relabel the session as selected-root isolated.
- [x] 7.3 Keep DeepSeek recent-session filtering and resume identity anchored to primary cwd, with the same limitation shown after restore.
- [x] 7.4 Add adapter and Agent Tab tests for primary-only launch, additional-directory disclosure, no automatic permission widening, history filtering, and future capability activation points.

## 8. End-to-End Validation

- [x] 8.1 Run focused tests for local-state migration, workspace mutations and routing, sidebar rendering, Agent input history, and all three adapter request or launch shapes.
- [x] 8.2 Run the affected workspace-member test and lint suites and resolve every new warning without changing unrelated local DeepSeek edits.
- [x] 8.3 Launch `NiumaTerm.exe --testing`, create and edit a multi-directory workspace, restart the application, and confirm primary startup plus sidebar restoration.
- [x] 8.4 In the isolated test instance, verify top-level terminal entries start in primary and every `More` combination starts the selected Profile in the selected directory from both New Tab buttons.
- [x] 8.5 Start and resume Codex and Claude conversations with two unrelated directories and confirm both roots are usable; start DeepSeek and confirm its primary-only notice and unchanged permission mode.
