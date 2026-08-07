# Agent Binary Updates Specification

## Purpose

Let users discover and safely install Claude Code and Codex binary updates while NiumaTerm preserves open Agent Tabs and resumes their provider conversations.

## Requirements

### Requirement: Installation-scoped update identity
The system SHALL derive an effective update installation identity from the provider kind, configured launcher resolution, and update-relevant environment without persisting or logging secret environment values.

#### Scenario: Profiles share one installation
- **WHEN** multiple agent profiles resolve to the same provider launcher and update context
- **THEN** the system exposes one shared update status and runs at most one check or update transaction for that installation

#### Scenario: Profiles use distinct installations
- **WHEN** two profiles resolve to different launchers or update contexts
- **THEN** the system tracks and updates them independently

### Requirement: Provider-aware version discovery
The system SHALL use the configured launcher and effective agent environment to discover the current version, available version, installation method, and update channel when the provider exposes those values.

#### Scenario: Codex discovery succeeds
- **WHEN** the configured Codex launcher returns a supported `doctor --json` update check
- **THEN** the system reports the installed version, latest version, availability state, and installation diagnostics from the structured response

#### Scenario: Claude discovery succeeds for a known channel
- **WHEN** the configured Claude launcher reports its current version and a supported `latest` or `stable` channel
- **THEN** the system compares that version with strictly validated metadata from the corresponding official release channel

#### Scenario: Discovery contract is unsupported
- **WHEN** a launcher lacks the required diagnostic command, returns an unknown schema, or reports a channel without a known release endpoint contract
- **THEN** the system reports any safely discovered current version and marks available-version discovery unsupported without guessing an installer or channel mapping

### Requirement: Update status and user control
The system SHALL present update controls under Agent General, with one manual Check action covering all effective installations referenced by agent profiles and one status and Update row per distinct installation.

#### Scenario: Profiles share update controls
- **WHEN** several agent profiles resolve to the same effective installation
- **THEN** Agent General shows that installation once and the shared Check action probes it at most once

#### Scenario: Profiles reference distinct installations
- **WHEN** agent profiles resolve to distinct effective installations
- **THEN** Agent General shows a separately actionable status row for each installation

#### Scenario: Update is available
- **WHEN** a check finds a strictly newer provider version
- **THEN** the settings UI shows the current and available versions and enables an Update action

#### Scenario: Installation is current
- **WHEN** the installed version is equal to or newer than the reported channel version
- **THEN** the settings UI reports that the installation is current and does not offer an unnecessary update

#### Scenario: User has not approved installation
- **WHEN** an automatic or manual check reports an available version
- **THEN** the system shows the version-keyed update notification and MUST NOT execute the update until the user approves an Update action

### Requirement: Top-right update notification lifecycle
The system SHALL present an available provider update as a persistent in-app notification at the top-right of the window and SHALL update that same notification in place as the installation transaction progresses.

#### Scenario: Available update notification appears
- **WHEN** a check finds a newer target version for an installation and that installation/version notification has not been dismissed
- **THEN** the system shows one top-right notification below the application chrome with the provider identity, current and target versions, a primary Update action, a secondary Settings action directly below the primary action, and a close affordance

#### Scenario: One-click update is unsupported
- **WHEN** an available version is known but the effective installation cannot be updated through its configured launcher
- **THEN** the notification omits the Update action, keeps the Settings action, and explains that the installation requires manual attention

#### Scenario: Distinct installations have updates
- **WHEN** more than one effective installation has a non-dismissed available target version
- **THEN** the system stacks one independently actionable top-right notification per installation and target version

#### Scenario: Update action is accepted
- **WHEN** the user selects Update and every affected tab is idle and recoverable
- **THEN** the system starts the installation transaction and removes or disables the notification's Update action before another activation can dispatch a duplicate transaction

#### Scenario: Update action encounters busy tabs
- **WHEN** the user selects Update while any affected tab is not idle
- **THEN** the system presents the affected-tab choice between Update when idle and Stop now and update before starting suspension

#### Scenario: Notification follows the live transaction
- **WHEN** an accepted transaction moves through waiting, suspension, provider update, version verification, or tab restoration
- **THEN** the original notification remains visible with a matching tone, phase label, and progress indicator derived from the authoritative installation state

#### Scenario: Measurable phase progress is available
- **WHEN** a transaction phase reports a completed and total count such as suspended tabs or restored tabs
- **THEN** the notification shows the corresponding determinate linear progress

#### Scenario: Provider progress is not measurable
- **WHEN** a transaction is waiting for idle or process exit, running a vendor update command, or verifying a version without trustworthy numeric progress
- **THEN** the notification shows an indeterminate linear progress indicator and MUST NOT fabricate a percentage

#### Scenario: Update and restoration succeed
- **WHEN** the target version is verified and every affected tab restoration attempt has completed successfully
- **THEN** the notification shows success, the verified installed version, and full progress before automatically dismissing after three seconds during which the window is visible and focused

#### Scenario: Version remains unchanged
- **WHEN** verification still reports the installation as outdated
- **THEN** the notification shows bounded actionable detail, a primary Retry action, and a secondary Settings action directly below the primary action before automatically dismissing after three seconds during which the window is visible and focused

#### Scenario: Update fails
- **WHEN** the transaction fails
- **THEN** the notification remains visible with bounded actionable detail, a primary Retry action, and a secondary Settings action directly below the primary action

#### Scenario: Initial prompt is dismissed
- **WHEN** the user closes an available-update notification before starting the update
- **THEN** the system records that installation and target version as dismissed and does not show the prompt again until the reported target version changes

#### Scenario: Running notification is closed
- **WHEN** the user closes a notification after its update transaction has started
- **THEN** the system hides the notification without cancelling the transaction or marking the target version as skipped and continues exposing live state in settings and affected tabs

### Requirement: Cached and isolated update checks
The system SHALL cache successful automatic check results per installation for 24 hours while allowing a manual check to bypass the cache.

#### Scenario: Fresh cached result exists
- **WHEN** startup checking considers an installation whose successful cache entry is less than 24 hours old
- **THEN** the system uses the cached result without contacting the provider release service

#### Scenario: Manual check is requested
- **WHEN** the user selects Check
- **THEN** the system performs a new provider check regardless of cache age and updates the cache on success

#### Scenario: Application runs in testing mode
- **WHEN** NiumaTerm is launched with `--testing`
- **THEN** automatic provider checks, real network release queries, and real provider update execution are disabled and test doubles may be injected instead

### Requirement: Vendor-managed update execution
The system SHALL invoke the official update entry point through the exact configured launcher and effective environment, using only provider-specific hard-coded arguments.

#### Scenario: Codex update is approved
- **WHEN** the user approves an update for a Codex installation
- **THEN** the system invokes the configured launcher with `update` exactly once for that installation

#### Scenario: Claude update is approved
- **WHEN** the user approves an update for a Claude Code installation
- **THEN** the system invokes the configured launcher with `update` exactly once for that installation

#### Scenario: Diagnostic output contains an update command
- **WHEN** provider or network output includes a suggested command or update action
- **THEN** the system treats it as display-only data and MUST NOT execute it as shell input

### Requirement: Installation-wide tab coordination
The system SHALL coordinate every open agent tab using the target installation before executing its update.

#### Scenario: Several tabs share the installation
- **WHEN** an update begins for an installation used by multiple open tabs or profiles
- **THEN** all affected tabs participate in the same suspension and restoration transaction and the vendor updater runs once

#### Scenario: Concurrent update is requested
- **WHEN** a check or update is already running for an installation
- **THEN** a second request joins or observes the existing operation rather than starting a competing process

#### Scenario: Unrelated installation remains active
- **WHEN** another open agent tab uses a different effective installation
- **THEN** its backend and composer remain available throughout the update

### Requirement: Safe quiescence before suspension
The system SHALL wait for affected agent tabs to become recoverably idle unless the user explicitly approves stopping active work.

#### Scenario: Update when idle is selected
- **WHEN** at least one affected tab has an active turn, approval, rewind, compaction, or queued provider operation
- **THEN** the system enters a waiting state and starts suspension only after every affected tab becomes idle and recoverable

#### Scenario: Stop now and update is selected
- **WHEN** the user explicitly approves immediate interruption
- **THEN** the system interrupts active provider work, cancels local queued operations, waits for a completion or exit boundary, and then evaluates recoverability before suspension

#### Scenario: Active work is not explicitly interrupted
- **WHEN** an available update is detected without an immediate-stop confirmation
- **THEN** the system MUST NOT silently terminate an active turn or tool operation

### Requirement: Recovery identity validation
The system SHALL validate a recovery identity for every conversation-bearing tab before stopping any affected backend.

#### Scenario: Claude session identity is available
- **WHEN** an affected Claude tab contains a conversation and has published a session ID
- **THEN** the system records that ID for post-update `--resume`

#### Scenario: Codex thread identity is available
- **WHEN** an affected Codex tab contains a conversation and has published a thread ID
- **THEN** the system records that ID for post-update `thread/resume`

#### Scenario: Conversation identity is unavailable
- **WHEN** an affected tab contains conversation state but has not published its provider recovery identity
- **THEN** the system waits or aborts before stopping any affected backend and identifies the blocking tab

#### Scenario: Untouched tab has no identity
- **WHEN** an affected tab has no provider identity and contains no conversation
- **THEN** the system treats it as recoverable by starting a new provider conversation after the update

### Requirement: In-place tab suspension
The system SHALL stop the provider session without closing or reconstructing the affected Agent Tab.

#### Scenario: Tab is suspended for update
- **WHEN** the update transaction reaches suspension
- **THEN** the tab retains its transcript, draft input, working directory, profile, thread controls, selection, scroll position, and expansion state while disabling provider input and displaying update progress

#### Scenario: Expected process exit arrives
- **WHEN** the old provider process exits after its tab has been suspended
- **THEN** stale output and EOF from that process are ignored and no unexpected-exit transcript error is appended

### Requirement: Confirmed provider shutdown
The system SHALL confirm that NiumaTerm-owned provider processes have exited before launching the vendor updater.

#### Scenario: Graceful shutdown succeeds
- **WHEN** closing provider stdin causes the launcher and provider process to exit within the configured timeout
- **THEN** the system proceeds only after observing process completion

#### Scenario: Graceful shutdown times out after immediate-stop approval
- **WHEN** the user approved immediate interruption and a NiumaTerm-owned provider process tree remains alive after the graceful timeout
- **THEN** the system terminates that owned process tree, waits for completion, and then proceeds or reports failure

#### Scenario: External process locks the installation
- **WHEN** the vendor updater reports that a process outside NiumaTerm still holds the installation
- **THEN** the system leaves the external process untouched, reports actionable guidance, and continues with tab restoration

### Requirement: Conversation restoration with the updated launcher
The system SHALL restart every recoverable suspended tab through its configured launcher and reconnect it to the captured provider conversation.

#### Scenario: Claude conversation is restored
- **WHEN** a suspended Claude tab has a captured session ID
- **THEN** the system starts the configured launcher with that session ID, preserves the existing pane transcript, and enables input after the resumed session is ready

#### Scenario: Codex conversation is restored
- **WHEN** a suspended Codex tab has a captured thread ID
- **THEN** the system initializes app-server directly into `thread/resume`, restores provider context without first creating an empty thread, and does not duplicate replayed transcript rows

#### Scenario: Untouched tab is restored
- **WHEN** a suspended tab had no conversation identity because it was untouched
- **THEN** the system starts a fresh provider conversation in the same retained tab

### Requirement: Failure-safe restoration
The system SHALL attempt to restore all suspended tabs after every terminal update outcome, including update failure and an unchanged installed version.

#### Scenario: Vendor update fails
- **WHEN** the provider update command exits unsuccessfully
- **THEN** the system records bounded diagnostics, attempts to restart every suspended tab with the runnable installed launcher, and reports the update failure without closing tabs

#### Scenario: Version verification succeeds
- **WHEN** the update command succeeds
- **THEN** the system re-runs version discovery and reports success only after it has verified the installed version and completed restoration attempts for all affected tabs

#### Scenario: Individual resume fails
- **WHEN** one tab cannot resume after the update
- **THEN** that tab retains its visible transcript and offers retry and start-new-session recovery while other tabs finish restoring independently

### Requirement: Bounded and secret-safe diagnostics
The system SHALL bound process output and execution time and SHALL prevent credentials or raw environment values from entering update caches, normal logs, or user-facing diagnostics.

#### Scenario: Provider command hangs or emits excessive output
- **WHEN** a probe, shutdown, update, or verification operation exceeds its configured time or output limit
- **THEN** the system terminates the owned operation, preserves a bounded diagnostic suffix, and transitions the installation to a recoverable failure state

#### Scenario: Profile contains credentials
- **WHEN** an agent profile supplies API keys, endpoints, or other environment values
- **THEN** update identity, logging, and diagnostics expose only allowlisted non-secret labels or irreversible fingerprints and never expose the raw secret values
