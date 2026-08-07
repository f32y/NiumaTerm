## Why

Agent tabs run long-lived Claude Code and Codex processes, but NiumaTerm currently provides no way to discover or install provider binary updates. Users must leave NiumaTerm, identify the correct installation mechanism, stop every affected agent, update the CLI, and manually recover their conversations.

## What Changes

- Discover the installed and available versions of the Claude Code and Codex launchers configured by agent profiles.
- Present per-installation update status and manual check/update actions without duplicating vendor download or installation logic.
- Surface each newly available target version in a persistent top-right in-app notification with an Update action, phase-aware progress, settings access, dismissal, and terminal success or failure feedback.
- Update through the configured vendor launcher so native and package-manager installations retain their official update, integrity-checking, and channel behavior.
- Coordinate all open tabs that use the same installation: wait for or explicitly stop active work, retain each tab and its UI state, stop the agent process, run the update, and resume the same provider conversation with the updated binary.
- Recover every suspended tab even when the update fails, and expose actionable failures when an external process or unsupported installation method prevents the update.
- Cache update checks and notification dismissals while preventing real checks and updates in testing mode.

## Capabilities

### New Capabilities

- `agent-binary-updates`: Provider-aware update discovery, user controls, coordinated process shutdown, vendor-managed update execution, and in-place restoration of Claude Code and Codex agent tabs.

### Modified Capabilities

None.

## Impact

- Agent General settings and the window notification layer gain installation-deduplicated version status, update controls, phase-aware progress, and failure reporting.
- Agent process adapters gain explicit lifecycle operations for identity capture, graceful shutdown, exit confirmation, and conversation resume after restart.
- Claude Code integration uses its published session ID, doctor output, official channel metadata, and `claude update` command.
- Codex integration uses machine-readable doctor output, its thread ID, app-server thread resume, and `codex update` command.
- The application gains a per-installation coordinator and persisted update-check metadata; no new binary downloader or installer is introduced.
