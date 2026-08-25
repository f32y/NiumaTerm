## 1. Restart Manager Platform Support

- [x] 1.1 Enable the `Win32_System_RestartManager` feature and add the Windows restart-manager module with structured application, usage, reboot-reason, and error results.
- [x] 1.2 Implement session ownership, grouped absolute-path registration, UTF-16 conversion, bounded `RmGetList` resizing, and guaranteed `RmEndSession` cleanup.
- [x] 1.3 Implement normal shutdown and restart operations that preserve partial outcomes and never request forced termination.
- [x] 1.4 Add focused platform tests for application mapping, growing process lists, Win32 errors, reboot reasons, and session cleanup through a scripted API adapter.

## 2. Installation Planning

- [x] 2.1 Introduce an immutable installation plan that selects changed payload files from version resources before any installed file is modified.
- [x] 2.2 Update file application to consume the captured plan while preserving copy, rename, rollback, previous-file cleanup, and relaunch behavior.
- [x] 2.3 Extend installer tests to cover changed and unchanged shell-extension selection and plan-driven replacement failures.

## 3. Pending Update Flow

- [x] 3.1 Extend `AppUpdate` with pending-install ownership and display states for file-use inspection, user decision, application shutdown, installation, and recovery warning.
- [x] 3.2 Capture the initiating window, inspect the DLL only when the plan selects it, proceed immediately on a clear result, and restore availability if the window closes before consent.
- [x] 3.3 Implement automatic close with a fresh Restart Manager session, current-user refresh, background shutdown, and a non-yielding UI-thread file application, affected-application restart, and NiumaTerm relaunch.
- [x] 3.4 Ensure every shutdown attempt is followed by restart before reporting file-application or partial-shutdown failure.
- [x] 3.5 Implement retry, continue-without-closing, cancel, reboot-required, and usage-check-error transitions without losing the staged plan or replacing files before consent.
- [x] 3.6 Implement installed-with-recovery-warning handling, live-window fallback, native message fallback, and the user action that completes the NiumaTerm relaunch.

## 4. Dialogs and Localization

- [x] 4.1 Add the in-use dialog with application names, duplicate-name process identifiers, Explorer-specific disruption text, and non-restartable application warnings.
- [x] 4.2 Add dialog variants for usage-check failure, remaining users after shutdown, reboot-required results, and post-install recovery failure.
- [x] 4.3 Connect About-page controls and status text to the pending update actions while preventing parallel checks or installations.
- [x] 4.4 Append matching English and Simplified Chinese strings for every new state, warning, and action.

## 5. Verification

- [x] 5.1 Add update-flow tests for unchanged DLL, unused changed DLL, affected applications, stale displayed users, failed shutdown, continue, cancel, unknown usage, and failed restart outcomes.
- [x] 5.2 Run the targeted platform and application update test suites and resolve formatting and lint findings in changed Rust files.
- [x] 5.3 From a disposable installation directory, launch NiumaTerm with `--testing` and verify the no-user, Explorer-user, close-and-update, continue, cancel, and recovery-warning paths without touching the normal installation.
- [x] 5.4 Confirm that continuing leaves the old DLL removable after Explorer exits and that a later NiumaTerm startup removes the `.nmt-previous` file.
