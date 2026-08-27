## ADDED Requirements

### Requirement: Additional directory summary
Each normal workspace row SHALL keep the primary directory in the existing path line and SHALL show a compact `+N` summary when the workspace has N additional directories. The summary SHALL NOT displace the workspace status slot or unread badge. Tooltip and accessibility text SHALL identify the primary directory and expose every additional directory in order.

#### Scenario: Workspace has no additional directory
- **WHEN** a workspace owns only its primary directory
- **THEN** the row retains the existing name and path hierarchy and shows no additional-directory summary

#### Scenario: Workspace has two additional directories
- **WHEN** a workspace owns one primary directory and two additional directories
- **THEN** the path line shows the tail-preserving primary path and a `+2` summary

#### Scenario: Inspect all workspace directories
- **WHEN** the user hovers the directory summary or assistive technology reads the workspace row
- **THEN** the full primary path and both full additional paths are available with the primary path identified

#### Scenario: Narrow sidebar
- **WHEN** the sidebar is too narrow for the complete primary path and additional-directory summary
- **THEN** the primary path preserves its tail, the `+N` summary remains visible, and status and unread indicators remain aligned
