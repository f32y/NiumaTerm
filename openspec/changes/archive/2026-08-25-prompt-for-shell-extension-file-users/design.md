## Context

NiumaTerm downloads and unpacks an update on a background executor, compares version resources, and swaps selected payload files on the UI thread immediately before relaunch. The swap renames each installed file aside before moving its staged replacement into place. This allows the running NiumaTerm executable and a shell-extension DLL mapped by Explorer to be replaced without a helper process.

The shell extension is an in-process COM server registered for the current user. Explorer can therefore keep the renamed DLL mapped and continue using its old context-menu implementation until Explorer exits. The update package is a ZIP consumed by the built-in updater, so Windows Installer cannot supply its own file-use dialog.

Windows Restart Manager can identify processes using a registered file and request orderly shutdown and restart. Its calls are synchronous, process lists can change between queries, some users cannot be restarted, and shutdown is limited by user and terminal session permissions.

## Goals / Non-Goals

**Goals:**

- Keep operating-system resource inspection in the Windows platform layer and return structured outcomes to the application layer.
- Decide whether the shell-extension check is needed before any installed file is changed.
- Keep application shutdown user-initiated and avoid forced termination.
- Preserve the current fast file swap and relaunch ordering.
- Make every failure point recoverable without leaving programs closed unnecessarily.

**Non-Goals:**

- Repackage NiumaTerm as an MSI or introduce a separate updater executable.
- Inspect users of every payload file; the running NiumaTerm executable continues to use the existing self-replacement path.
- Guarantee that every affected application can be reopened automatically.
- Make an already-running Explorer process execute the new DLL without unloading the old module.
- Remove the rename-and-replace fallback or require a system restart.

## Decisions

### 1. Wrap Restart Manager in the Windows platform layer

Add `windows::restart_manager` backed by the existing `windows-sys` dependency with `Win32_System_RestartManager` enabled. The module exposes domain data such as affected application name, process identity, application kind, terminal session, restartability, status, and reboot reasons. Win32 numeric results remain available in a typed error for logging and application decisions.

A session owns the handle returned by `RmStartSession`, registers absolute file paths as one group, and calls `RmEndSession` from `Drop`. Listing applications handles the normal size-query pattern and retries a bounded number of times when `RmGetList` reports that the process list grew.

Only the installed `NmtShellExtension.dll` path is registered. Registering the running `NiumaTerm.exe` would cause the updater to discover itself as a process that cannot be shut down.

Alternatives considered:

- Enumerating loaded modules with Tool Help or Process Status APIs duplicates operating-system installer behavior, encounters access and architecture restrictions, and provides no orderly restart path.
- Moving updates to Windows Installer would replace the existing release and self-update model for a single Windows-only concern.

### 2. Use a short inspection session and a fresh action session

The initial usage check opens a Restart Manager session, obtains the affected-application list, and ends the session before showing the dialog. This avoids consuming one of the limited system sessions while a user leaves a modal open.

Choosing automatic close starts a fresh session for the DLL. It queries current users again and then calls `RmShutdown`, whose own processing refreshes the registered resource users. The original displayed list is informative rather than authoritative.

Shutdown uses action flags `0`. `RmForceShutdown` is excluded because it can terminate an unresponsive application with unsaved work. `RmShutdownOnlyRegistered` is also excluded because an unregistered Explorer instance would prevent the requested release. Applications marked as not restartable are disclosed before shutdown.

### 3. Split installation planning from file mutation

Refactor the installer so version comparison produces an immutable installation plan containing the selected payload names. Applying a plan performs the existing copy, rename, rollback, and cleanup operations without repeating selection.

The staged update flow checks whether the plan contains `NmtShellExtension.dll`:

```text
staged package
      |
      v
installation plan
      |
      +-- shell extension unchanged --> apply plan
      |
      +-- shell extension changed ----> inspect DLL users
                                           |
                                           +-- clear --> apply plan
                                           |
                                           +-- used/unknown --> await user
```

The plan keeps the prompt aligned with the exact files that will be installed and preserves the existing optimization that avoids replacing an unchanged shell extension.

### 4. Represent a staged but paused update explicitly

`AppUpdate` gains a pending-install value that owns the release, staged root, install root, installation plan, and the window that initiated installation. User-visible status distinguishes downloading, inspecting file use, waiting for a user decision, closing applications, installing, and installed-with-recovery-warning.

The pending value is separate from cloneable display status so native session ownership never leaks into rendering. Starting another check or installation remains disabled while a pending installation exists.

After download, the originating window is used for the modal. If that window no longer exists, no files are replaced and the release returns to the available state. The updater never closes applications without a live surface showing the user's choice.

### 5. Keep inspection and shutdown off the UI thread and finish installation without yielding

Restart Manager inspection and shutdown run on the background executor. The existing file swap remains a short UI-thread action after all paths and decisions have been captured. Restarting affected applications then runs synchronously in the same final UI transaction as the swap and NiumaTerm relaunch. This final call can briefly block the window, but it prevents another event from consulting the running executable path after that path names the new executable rather than the mapped image of the current process.

The automatic-close sequence is:

1. Open a fresh Restart Manager session and refresh affected applications.
2. If reboot reasons are nonzero, return to the prompt with automatic close unavailable.
3. Request normal shutdown.
4. Apply the captured installation plan on the UI thread only after shutdown succeeds.
5. Call `RmRestart` without yielding back to the event loop, including when file application fails after applications were closed.
6. End the Restart Manager session.
7. Relaunch NiumaTerm after successful installation and recovery handling.

If shutdown stops only some applications, restart is still attempted before the remaining-user prompt is refreshed. If applying the plan fails, affected applications are restarted before the existing installation error is displayed.

### 6. Treat close, continue, and cancel as separate commands

The in-use dialog lists application display names and adds process identifiers when duplicate names need disambiguation. Explorer receives specific wording about File Explorer windows closing. The footer provides:

- **Close applications and update**: run the fresh-session sequence above.
- **Continue update**: apply the plan immediately with the current rename-and-replace behavior.
- **Cancel**: discard the pending decision, replace no files, and restore the available release status.

For usage-check failure, the dialog replaces automatic close with **Retry check**. For nonzero reboot reasons, retry is unnecessary until system state changes, so the dialog offers continue and cancel. All new strings are added to both English and Simplified Chinese locale files.

### 7. Keep post-install recovery failure visible

A failed `RmRestart` does not roll back files that were installed successfully. Instead, the update enters an installed-with-recovery-warning state that identifies applications requiring manual reopening. NiumaTerm delays its own relaunch until the user acknowledges the warning through a **Restart NiumaTerm** action.

The warning targets the originating window, then another live NiumaTerm window if the origin has closed. If no GPUI window remains, the existing native Windows message path presents the localized warning before NiumaTerm relaunches. This keeps recovery trouble visible without persisting cross-version update state.

## Risks / Trade-offs

- [Explorer shutdown closes File Explorer windows and can disrupt desktop interaction] → Require an explicit user action, name Explorer in the prompt, and retain the non-disruptive continue path.
- [A process list can change while the dialog is open] → End the inspection session and start a fresh session when automatic close is chosen.
- [An application can refuse normal shutdown] → Never force termination; restart anything already stopped, refresh the list, and return control to the user.
- [Restart Manager cannot operate across some users or terminal sessions] → Surface reboot reasons or access errors and allow rename-and-replace to proceed without claiming the DLL was released.
- [Affected applications can fail to reopen] → Warn before closing non-restartable applications and report recovery failure after installation.
- [Restarting affected applications can briefly block the window] → Perform it only after explicit consent and keep it in the final non-yielding transaction so no callback observes the renamed executable path.
- [The rename fallback leaves an old DLL until Explorer exits] → Preserve startup cleanup and explain delayed context-menu activation before the user continues.

## Migration Plan

1. Add the platform wrapper and its focused tests without connecting it to the updater.
2. Introduce installation planning while preserving current update behavior.
3. Add pending-install states, localized dialogs, and Restart Manager actions.
4. Validate no-user, Explorer-user, continue, cancel, shutdown-failure, and recovery-warning paths from a disposable installation directory.

There is no persisted data migration. Rolling back the feature removes the pre-install inspection and dialog while leaving the existing rename-and-replace updater intact.
