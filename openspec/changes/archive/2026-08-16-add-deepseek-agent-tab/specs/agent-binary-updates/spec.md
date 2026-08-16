## MODIFIED Requirements

### Requirement: Update status and user control
The system SHALL present update controls under Agent General, with one automatic-check switch, one manual Check action covering all effective installations referenced by agent profiles, and one status and Update row per distinct installation. An agent profile whose harness has no vendor-managed installation SHALL contribute no installation to those controls.

#### Scenario: Automatic checking is enabled
- **WHEN** the automatic-check switch is on
- **THEN** the system checks every effective installation once shortly after startup and again every hour while the application runs

#### Scenario: Automatic checking is disabled
- **WHEN** the automatic-check switch is off
- **THEN** the system performs no automatic provider checks and keeps the manual Check and Update actions available

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

#### Scenario: A harness updates outside the application
- **WHEN** an agent profile references a harness the user installs and updates through their own package manager
- **THEN** Agent General shows no status or Update row for it, and the shared Check action does not probe it
