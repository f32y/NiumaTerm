## 1. Update Domain and Launcher Foundation

- [x] 1.1 Add provider-neutral installation identity, version status, update state, result, and error models with secret-safe formatting.
- [x] 1.2 Factor a shared configured-launcher abstraction that preserves Windows executable and command-shim resolution for sessions, probes, and updates while accepting only structured allowlisted arguments.
- [x] 1.3 Implement bounded child-process execution with timeout, output caps, explicit exit observation, and redacted diagnostics.
- [x] 1.4 Implement effective-installation key derivation and profile deduplication tests for shared launchers, distinct launchers, and update-relevant environment contexts.

## 2. Provider Discovery and Update Adapters

- [x] 2.1 Implement and fixture-test Codex `doctor --json` schema validation and extraction of current/latest versions, availability, install diagnostics, and display-only remediation data.
- [x] 2.2 Implement Codex current-version fallback and the configured-launcher `update` runner without executing diagnostic command text.
- [x] 2.3 Implement and fixture-test bounded Claude `doctor` parsing for current version, install method, channel, and update configuration, with `--version` fallback.
- [x] 2.4 Implement the injectable Claude `latest`/`stable` release-channel client with strict semantic-version validation and explicit unsupported handling for unknown channels.
- [x] 2.5 Implement the configured-launcher Claude `update` runner and normalize provider failures, unsupported installation methods, and external-lock diagnostics.

## 3. Coordinator, Cache, and Test Isolation

- [x] 3.1 Add the application-level per-installation coordinator, observable state transitions, and serialization of concurrent checks and updates.
- [x] 3.2 Add the local update-status cache with 24-hour freshness, manual bypass, last-check metadata, and per-version dismissal without storing raw environment or command output.
- [x] 3.3 Add delayed asynchronous startup checks and ensure they can notify but never install without a user-approved Update action.
- [x] 3.4 Wire testing mode and dependency injection so `--testing` prevents real startup checks, release network calls, and provider update execution while supporting fake launchers, release responses, time, and cache roots.

## 4. Recoverable Agent Session Lifecycle

- [x] 4.1 Add a provider-neutral recovery snapshot and pane readiness assessment covering conversation identity, untouched state, active turns, approvals, rewind, compaction, and queued operations.
- [x] 4.2 Replace drop-only provider cleanup with explicit graceful stdin shutdown, bounded exit waiting, and an immediate-stop path that terminates and waits for only the NiumaTerm-owned agent process tree.
- [x] 4.3 Add Claude suspension/resume support that captures the published session ID, blocks conversation-bearing tabs without one, and restarts untouched tabs as new conversations.
- [x] 4.4 Expose the Codex thread ID and add initial app-server `thread/resume` startup that avoids a fresh orphan thread and can suppress replay during an in-place restart.
- [x] 4.5 Add an AgentPane update-suspension lifecycle that advances the session epoch, retains all visual and composer state, disables provider input, ignores expected old-process EOF, and preserves settings across Ready.
- [x] 4.6 Add per-tab retry and start-new-session recovery when the updated launcher starts but provider resume fails.

## 5. Installation Update Transaction

- [x] 5.1 Enumerate all open tabs matching an installation key and prevent unrelated installation tabs from participating in the transaction.
- [x] 5.2 Implement Update when idle, including waiting for every affected tab to settle and validating every recovery snapshot before stopping any backend.
- [x] 5.3 Implement the confirmed Stop now and update flow, including provider interruption, local queue cancellation, completion/exit waiting, and a second recoverability check.
- [x] 5.4 Implement the transaction's suspend-all, run-one-vendor-updater, re-probe, and restore-all sequence with restoration in a finally-style path after every update outcome.
- [x] 5.5 Aggregate update and per-tab restoration outcomes so one failed resume does not block other tabs and success is reported only after verification and all restoration attempts finish.

## 6. Settings, Notifications, and Agent Tab Experience

- [x] 6.1 Show one shared Check action and installation-deduplicated current/available versions, install/channel labels, last check, status, and Update controls under Agent General.
- [x] 6.2 Add the update confirmation UI with affected-tab counts, Update when idle as the safe default, and an explicit Stop now and update choice for active work.
- [x] 6.3 Add retained-tab progress banners for waiting, stopping, updating, reconnecting, and failure states, with composer enablement tied to restored provider readiness.
- [x] 6.4 Implement a pure update-notification view reducer that maps authoritative coordinator states to available, waiting, suspending, updating, verifying, restoring, succeeded, failed, and unchanged presentations.
- [x] 6.5 Implement a dedicated top-right notification stack below the application chrome with a stable installation/version identity, provider icon, current and target versions, vertically stacked primary and Settings actions, and a close action without moving existing transient notifications.
- [x] 6.6 Wire the notification Update action through a synchronous re-entry guard and the installation lock, routing busy installations to the Update when idle versus Stop now and update confirmation.
- [x] 6.7 Update the same notification card in place during a transaction, remove or disable Update while active, and render determinate tab-count progress or indeterminate provider progress without fabricated percentages.
- [x] 6.8 Implement version-keyed prompt dismissal, running-notification hide-without-cancel semantics, three seconds of focused visible-time display for succeeded and unchanged outcomes, Retry and Settings actions on unchanged outcomes, and persistent failed Retry and Settings actions.
- [x] 6.9 Present bounded actionable errors for unsupported discovery, update-command failures, external file locks, and individual resume failures without exposing profile secrets.

## 7. Verification

- [x] 7.1 Add parser, semantic-version, cache-freshness, installation-deduplication, timeout, output-bound, and secret-redaction unit tests.
- [x] 7.2 Add notification reducer and UI tests for top-right placement, stable in-place transitions, version keys, stacking, button eligibility, double-click prevention, progress modes, and bounded diagnostics.
- [x] 7.3 Add deterministic notification lifetime tests for version dismissal, running-card closure, focused visible-time success and unchanged dismissal, and persistent Retry/Settings failure states.
- [x] 7.4 Add fake-launcher integration tests proving one updater invocation and in-place restoration for multiple Claude tabs and multiple Codex tabs sharing an installation.
- [x] 7.5 Add integration tests for mixed installations, waiting for idle, immediate interruption, missing recovery identities, updater failure, unchanged version, external-lock errors, and partial resume failure.
- [x] 7.6 Add regression tests proving expected shutdown EOF cannot append an exit error or duplicate transcripts and Codex restart does not create an empty thread before resume.
- [x] 7.7 Add testing-mode regression tests that fail if a real provider executable, release endpoint, or persistent production cache is accessed.
- [ ] 7.8 Run formatting, workspace tests, and clippy checks required by the repository hooks, then manually validate the fake update workflow by launching `NiumaTerm.exe --testing`.
