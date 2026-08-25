## Why

Windows Explorer can keep `NmtShellExtension.dll` loaded while NiumaTerm installs an update. The current rename-and-replace strategy puts the new DLL on disk, but users receive no warning that Explorer will continue running the old context-menu code until it exits.

## What Changes

- Detect applications using the installed shell-extension DLL when the staged DLL actually differs from the installed version.
- Show a localized prompt that identifies affected applications and lets the user close them and update, continue without closing them, or cancel.
- Use Windows Restart Manager for graceful application shutdown and restart without forced termination.
- Preserve the current rename-and-replace path when the user continues without closing applications or Restart Manager cannot release the DLL.
- Skip the prompt when the shell-extension DLL is unchanged or no application is using it.
- Report usage-check, shutdown, and restart outcomes without treating an unknown result as an unused DLL.

## Capabilities

### New Capabilities

- `application-self-update`: Covers staged application updates, in-use shell-extension detection, user choices, and affected-application recovery.

### Modified Capabilities

None.

## Impact

- Windows platform support gains a Restart Manager wrapper and enables the corresponding `windows-sys` feature.
- The application update flow gains a pre-install usage check, pending-install state, and localized dialog content.
- Installation planning must expose the files selected for replacement before the file swap begins.
- Automated coverage expands across update planning and decision handling, with Windows validation for real DLL users and Explorer behavior.
