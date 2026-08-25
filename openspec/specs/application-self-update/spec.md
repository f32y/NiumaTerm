# application-self-update Specification

## Purpose

Defines how NiumaTerm installs staged application updates while giving users control over programs that are still running the Windows shell-extension DLL.

## Requirements

### Requirement: Shell-extension usage is checked only when relevant
The system SHALL check for applications using the installed shell-extension DLL only when the staged DLL is selected for replacement. The system SHALL continue without an in-use prompt when the DLL is unchanged or no application is using it.

#### Scenario: Update does not change the shell extension
- **WHEN** an application update changes other payload files but the staged shell-extension version matches the installed version
- **THEN** the system installs the selected files without checking for or prompting about shell-extension users

#### Scenario: Changed shell extension is unused
- **WHEN** the staged shell-extension DLL differs from the installed DLL and no application is using the installed DLL
- **THEN** the system installs the update without displaying an in-use prompt

### Requirement: In-use applications are disclosed before replacement
The system SHALL display a localized prompt before replacing a changed shell-extension DLL when one or more applications are using it. The prompt SHALL identify the affected applications, explain the effect of continuing without closing them, and offer actions to close and update, continue without closing, or cancel.

#### Scenario: Windows Explorer is using the DLL
- **WHEN** Windows Explorer is reported as using the changed shell-extension DLL
- **THEN** the prompt identifies Windows Explorer and warns that open File Explorer windows can close during automatic shutdown

#### Scenario: An application cannot be reopened automatically
- **WHEN** an affected application is reported as not restartable
- **THEN** the prompt warns that the application will need to be reopened manually before the user chooses automatic shutdown

### Requirement: Affected applications are closed gracefully
When the user chooses to close affected applications, the system SHALL refresh the current usage information, request normal application shutdown without forced termination, and replace the DLL only after the current users have released it.

#### Scenario: Applications release the DLL
- **WHEN** the user chooses to close affected applications and every current user releases the DLL
- **THEN** the system installs the update and requests that the applications it closed be restarted

#### Scenario: Application usage changed while the prompt was open
- **WHEN** the affected application list changes before the user chooses automatic shutdown
- **THEN** the system acts on the current usage information instead of relying on the displayed list

#### Scenario: An application does not shut down
- **WHEN** normal shutdown does not release the DLL from every current user
- **THEN** the system does not force termination and presents the remaining users with actions to retry, continue without closing, or cancel

### Requirement: Continuing preserves the non-disruptive update path
When the user chooses to continue without closing affected applications, the system SHALL install the new DLL using the existing rename-and-replace behavior and SHALL explain that existing processes can use the old context-menu code until they exit.

#### Scenario: User continues while Explorer is running the old DLL
- **WHEN** the user chooses to continue without closing Windows Explorer
- **THEN** the new DLL is installed, NiumaTerm restarts, and the prompt has informed the user that the updated context menu becomes active after Explorer restarts

### Requirement: Cancellation leaves the update retryable
When the user cancels from an in-use or usage-check prompt, the system SHALL not replace any installed payload file and SHALL return the release to a state from which the user can start installation again.

#### Scenario: User cancels before shutdown
- **WHEN** the user cancels after the update has been staged but before affected applications are closed
- **THEN** no installed file is replaced and the release remains available for a later installation attempt

### Requirement: Indeterminate usage is not reported as clear
If the system cannot determine DLL usage or determines that normal shutdown cannot release the DLL, it SHALL disclose that result and SHALL not automatically proceed as though the DLL were unused.

#### Scenario: Usage check fails
- **WHEN** the operating system does not return a usable application list
- **THEN** the system displays a localized check failure with actions to retry, continue using rename-and-replace, or cancel

#### Scenario: Releasing the DLL requires a system restart
- **WHEN** the usage check reports that normal application shutdown cannot release the DLL
- **THEN** automatic close is unavailable and the user can continue using rename-and-replace or cancel

### Requirement: Recovery outcomes remain visible
After requesting restart of affected applications, the system SHALL keep a successfully installed update in place and SHALL display a localized warning for any application that could not be restarted automatically.

#### Scenario: An affected application cannot restart
- **WHEN** the DLL has been replaced but one or more applications cannot be restarted automatically
- **THEN** the system identifies that recovery is incomplete, retains the installed update, and lets the user complete the NiumaTerm restart
