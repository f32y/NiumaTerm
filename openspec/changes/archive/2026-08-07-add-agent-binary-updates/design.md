## Context

NiumaTerm launches a long-lived backend process for every Claude Code or Codex agent tab. The `AgentPane` owns the visible transcript, composer, working directory, profile snapshot, and thread settings separately from the provider `Session`, so the process can be replaced without destroying the tab. Claude sessions already retain the CLI-published session ID and can spawn with `--resume`; Codex sessions retain an app-server thread ID and can call `thread/resume`, but that ID and an initial-resume launch path are not currently exposed to the pane.

Provider updates are installation-scoped rather than profile-scoped. Several profiles and tabs can resolve to one launcher while differing in model or endpoint settings, so one update must coordinate every live process that can lock or execute that installation. Conversely, profiles that resolve to different binaries or update contexts must remain independent.

The vendor CLIs already know how they were installed and how to update themselves. Codex exposes machine-readable update diagnostics and a vendor update command. Claude exposes health/install information, release-channel metadata, and a vendor update command. NiumaTerm should orchestrate those contracts instead of becoming another binary installer.

## Goals / Non-Goals

**Goals:**

- Report current and available Claude Code and Codex versions per effective installation.
- Invoke only the configured vendor launcher and its official update entry point.
- Keep all affected agent tabs open while their backend processes are stopped and restarted.
- Restore the same provider conversation, pane transcript, draft, working directory, profile, and thread controls after an update.
- Recover suspended tabs when checking, shutdown, update, verification, or resume fails.
- Coordinate concurrent tabs and update requests without racing one installation.
- Prevent real network checks and update execution in `--testing` mode.

**Non-Goals:**

- Downloading, validating, replacing, downgrading, or rolling back provider binaries directly in NiumaTerm.
- Updating unrelated Claude Code or Codex installations that are not referenced by configured profiles or open tabs.
- Terminating provider processes owned by other applications.
- Guaranteeing recovery of an in-flight turn, approval request, rewind operation, compaction, or provider process that has not published a resumable identity.
- Automatically installing an update without a user-approved update action.

## Decisions

### 1. Coordinate updates by effective installation

Add an application-level update coordinator with a provider-neutral state model and provider adapters. An `InstallationKey` combines the provider kind, resolved configured launcher, and a non-secret fingerprint of update-relevant environment such as `PATH`, `CODEX_HOME`, `CLAUDE_CONFIG_DIR`, and package-manager provenance. Raw environment values and credentials are not persisted or logged.

Profiles with the same key share one check, one status record, and one update transaction. A per-key asynchronous lock serializes manual checks and updates. This is preferable to putting update state on each profile because duplicate profiles could otherwise run competing installer processes against the same files.

The coordinator state progresses through `Unknown`, `Checking`, `Current`, `Available`, `WaitingForIdle`, `Suspending`, `Updating`, `Verifying`, `Restoring`, `Updated`, `Unchanged`, `Unsupported`, or `Failed`. Pane lifecycle state remains separate so a tab can display that its backend is temporarily suspended without being treated as an unexpected exit.

### 2. Preserve the configured launcher and delegate installation to the vendor

Every probe and update uses the same configured launcher semantics and effective environment as an agent session. Arguments are selected from a provider-specific allowlist; no command string obtained from network data, doctor output, or cached state is executed.

For Codex, the adapter runs `doctor --json`, validates the supported schema, and reads the current/latest version and installation diagnostics from the update check. It runs `update` to install. The diagnostic `update action` is display-only because invoking it would turn vendor output into executable shell input.

For Claude Code, the adapter parses a bounded, known subset of `doctor` output for the current version, install method, channel, and update configuration, with `--version` as the current-version fallback. It queries the official release service only for channels with a known contract (`latest` and `stable`) and validates a strict semantic version. Unknown or pre-release channel mappings are reported as unavailable for preflight checking rather than guessed. It runs `update` to install, allowing Claude to select its configured channel and installation behavior.

If a launcher version does not support a required probe, NiumaTerm reports the current version when possible and marks update discovery unsupported. It does not silently substitute a different binary or package manager.

### 3. Cache checks, but make manual checks authoritative

Persist a small version-status cache per installation under NiumaTerm's local application data. A delayed startup check reuses a successful result for up to 24 hours; a manual check bypasses the cache. The cache stores versions, channel/install labels, timestamps, and a skipped version, but no command output or environment values.

Automatic checks can produce a non-blocking notification but never start installation. `--testing` disables startup checks, network access, and real vendor update commands; tests inject fake launchers, fake release responses, a temporary cache root, and deterministic time.

### 4. Treat update as a recoverable multi-tab transaction

When a user approves an update, the coordinator snapshots every open pane with the target installation key. A snapshot contains the provider conversation identity, working directory, profile identity and launch snapshot, thread settings, and whether the tab is an untouched conversation. The transcript, draft, selection, scroll position, expansion state, and other visual state remain in the existing `AgentPane` entity and are not serialized for the restart.

The normal action is "Update when idle": tabs already idle are prepared immediately and the transaction waits for the rest to settle. "Stop now and update" is an explicit destructive choice that interrupts active turns and waits for their provider completion/exit boundary before suspension. Pending approvals, local rewind, compaction, and queued slash operations also prevent suspension until resolved or explicitly cancelled. NiumaTerm never silently interrupts active work because a partially completed tool call can leave the workspace in an unknown state.

Before stopping anything, the coordinator validates recoverability for every pane. A conversation-bearing Claude tab requires a published session ID and a Codex tab requires a published thread ID. An untouched tab without an identity is recoverable as a new conversation. If any pane cannot be recovered, the transaction remains pending or fails before any backend is stopped.

### 5. Add explicit session suspension and shutdown completion

Provider sessions gain an explicit shutdown operation instead of relying only on `Drop`. Suspension increments the pane's session epoch first so messages and EOF from the old process cannot append an exit error to the retained tab. The pane disables its composer and displays update progress while its `Session` is absent.

Shutdown closes stdin to request a clean provider exit and waits for the launcher and provider process to release. After a bounded timeout, an explicitly approved immediate update may terminate only the NiumaTerm-owned agent process tree and wait again. The updater is launched outside that process tree. If an external process still holds the installation, the vendor update failure is surfaced and NiumaTerm does not attempt to terminate the external owner.

This explicit completion boundary is required on Windows: merely killing the `cmd.exe` launcher can leave a native or Node descendant alive and holding the executable.

### 6. Resume provider conversations without rebuilding the tab

Claude restoration spawns the updated configured launcher with the captured `--resume` session ID and preserves the pane's existing transcript. An untouched tab starts a new Claude conversation.

Codex exposes its current thread ID and adds an initial-resume app-server startup mode. After app-server initialization, that mode sends `thread/resume` instead of first creating a fresh thread. The adapter consumes the resume response to restore provider context and thread settings but suppresses transcript replay during an update restart because the pane already contains those rows. This avoids both an orphan empty thread and duplicate transcript content.

Restoration runs after every update attempt, including installer failure or unchanged version. A successful update is complete only after the version has been re-probed and every recoverable pane has either reached ready state or reported an individual restart error. A failed pane keeps its visible transcript and exposes retry and start-new-session actions; it is never closed automatically.

### 7. Use a stable top-right notification as the primary update surface

Adapt the provider-update notification model used by T3 Code: a dedicated presenter observes authoritative coordinator state and reduces it into one notification view per installation and target version. The notification identity is derived from `InstallationKey` plus the target version, so the same card can be updated in place while a newly published version creates a new prompt. The presenter does not infer success from an update-command future alone; it renders the coordinator's latest checked, transaction, verification, and restoration state.

Update notifications use a dedicated top-right window layer below the application chrome. This avoids moving existing NiumaTerm transient notifications, which currently use the shared notification placement. Cards slide in from the right and stack when distinct installations have updates.

The initial `Available` view is persistent and contains the provider icon and name, current and target versions, a primary **Update** action, a secondary **Settings** action directly below the primary action, and a close affordance. The notification component exposes separate primary and secondary action slots and renders them as one right-aligned vertical column; the same layout places **Settings** below **Retry** in retryable terminal states. If one-click update is unsupported, it contains **Settings** without **Update**. The Update handler has a synchronous re-entry guard as well as the coordinator's per-installation lock. If affected tabs are busy, the action opens the existing choice between **Update when idle** and **Stop now and update**; otherwise it starts the safe update path directly.

After acceptance, the presenter updates the same card instead of replacing it. The Update action is removed or disabled, and the title, tone, phase label, and progress indicator follow `WaitingForIdle`, `Suspending`, `Updating`, `Verifying`, and `Restoring`. A linear progress bar is determinate only when the coordinator has real counts, such as suspended tabs over affected tabs or restored tabs over suspended tabs. Waiting for idle, graceful process exit, the vendor update command, and version verification use an indeterminate bar because Claude Code and Codex do not publish trustworthy byte or percentage progress. The UI never fabricates download percentages.

Terminal views also reuse the same card. `Updated` shows the verified installed version and a full progress bar. `Unchanged` shows bounded actionable detail with a primary **Retry** action and a secondary **Settings** action. Both `Updated` and `Unchanged` automatically dismiss after three seconds of actual visible, focused-window time. `Failed` remains visible with bounded actionable detail, a primary **Retry** action, and a secondary **Settings** action. A failed individual resume may additionally direct the user to its retained tab. Retry is guarded by the same installation transaction lock.

Closing an initial availability prompt records the installation/version notification key so it does not reappear until the target version changes. Closing a running notification hides only that presentation: it does not cancel the update or record the target as skipped, and settings continues to expose live state. Only terminal state is eligible for persisted outcome display; a cached non-terminal view is discarded so an application restart cannot strand a progress card indefinitely.

Agent General settings show one shared **Check for Updates** action and one current-version, available-version, channel/install-method, last-check, status, and Update row per effective installation. The rows are derived from installation keys rather than profile entries, so profiles sharing a launcher never duplicate controls while genuinely distinct installations remain independently actionable. During an update, affected tabs also retain their local banner and disabled composer so progress and recovery remain discoverable even if the top-right card is closed. Provider output is bounded and summarized for users; detailed diagnostics may be logged only after secret redaction.

## Risks / Trade-offs

- **Provider diagnostic formats change** → Validate schemas and known fields, cap output, retain `--version` fallbacks, and report unsupported discovery rather than making installation assumptions.
- **A provider update changes persisted-session compatibility** → Keep the pane transcript independent, report resume failure without closing the tab, and provide retry or new-session recovery.
- **An update partially succeeds** → Delegate atomicity to the vendor, always re-probe the installed version, and restore tabs with whichever executable remains runnable.
- **External Claude or Codex processes lock files** → Stop only NiumaTerm-owned sessions, surface the vendor error with actionable guidance, and restore NiumaTerm tabs.
- **Immediate shutdown interrupts workspace mutations or background children** → Default to waiting for idle, require explicit confirmation for immediate stop, prefer EOF shutdown, and use forced tree termination only after timeout.
- **Profiles appear identical but use different update configuration** → Include the resolved launcher and update-relevant environment fingerprint in installation identity and test deduplication boundaries.
- **Automatic network checks add startup work** → Delay them until after startup, cache successful results for 24 hours, run asynchronously, and keep them disabled in testing mode.
- **A hidden or stale notification misrepresents a live transaction** → Derive views from authoritative coordinator state, update a stable card in place, never persist non-terminal views, and keep the same state visible in settings and affected tabs.

## Migration Plan

1. Introduce provider-neutral probe models, installation identity, cache storage, and fake adapters without enabling startup checks.
2. Add explicit provider session identity/shutdown/resume primitives and cover them with adapter and pane lifecycle tests.
3. Add settings status, the top-right notification reducer/presenter, and manual Check.
4. Add manual Update with multi-tab suspension/restoration and drive in-place notification progress from the coordinator.
5. Enable delayed cached startup checks and version-keyed availability notifications after the manual workflow is stable.

The feature is additive and requires no existing configuration migration. Removing or rolling back NiumaTerm support leaves vendor binaries at the version installed by their own updater and does not alter provider session files.

## Open Questions

None block implementation. Pre-release Claude channel discovery remains intentionally unsupported until Anthropic publishes or exposes a stable channel endpoint contract.
