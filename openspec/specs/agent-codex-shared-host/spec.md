# agent-codex-shared-host

## Purpose

Define how NiumaTerm shares Codex app-server processes between matching launches while keeping different endpoints and credentials independent.

## Requirements

### Requirement: Matching Codex launches share one live app-server
The system SHALL lazily start one initialized Codex app-server per distinct launch configuration. Tabs with matching launches SHALL reuse that process, and different launches SHALL be allowed to run concurrently.

#### Scenario: First tab for a launch starts its host
- **WHEN** a Codex Agent Tab opens without a running host for its launch
- **THEN** the system starts and initializes an app-server for that launch and creates or resumes the tab's thread

#### Scenario: Simultaneous tabs use the same launch
- **WHEN** two Codex Agent Tabs request the same launch before startup completes
- **THEN** both openings join the same startup attempt and receive its result

#### Scenario: Different launches start together
- **WHEN** two Codex Agent Tabs request different launches
- **THEN** each launch can start without waiting for the other's initialization

#### Scenario: Last tab for a host closes
- **WHEN** the final session releases a host
- **THEN** the system stops that process within a bounded interval without stopping other hosts

### Requirement: Host identity includes the endpoint and credentials
The system SHALL identify a launch by its executable, arguments, custom endpoint, and effective configured environment including credentials. Model, reasoning effort, and workspace directories SHALL NOT create separate hosts. Environment values stored in the host registry SHALL be represented by hashes.

#### Scenario: Profiles use different endpoints
- **WHEN** two profiles differ only in their custom endpoint URL
- **THEN** they use separate app-server processes

#### Scenario: A credential changes
- **WHEN** a profile changes its API key while its earlier tabs remain open
- **THEN** a new tab starts a separate host with the updated key and earlier tabs retain their existing host

#### Scenario: Process settings differ
- **WHEN** a profile uses another executable, arguments, or configured environment
- **THEN** it starts a separate host without requiring existing tabs to close

#### Scenario: Thread settings differ
- **WHEN** two tabs have matching launches but different models, reasoning effort, or workspaces
- **THEN** they share a host and apply their own thread settings

### Requirement: JSON-RPC ownership is isolated per session
The shared host SHALL allocate client request IDs across the whole process and SHALL route each response, error, and server request only to the session that owns it. Each session SHALL retain its own handshake state after host initialization, current turn, approvals, queued commands, compaction state, history cursor, and settings.

#### Scenario: Concurrent requests receive responses out of order
- **WHEN** two tabs send requests and app-server returns their responses in the opposite order
- **THEN** each response updates only the tab that submitted its request

#### Scenario: Approval targets one root thread
- **WHEN** app-server asks for approval with a thread ID owned by one tab
- **THEN** only that tab displays and answers the approval while every other tab remains unchanged

#### Scenario: Session closes with a pending server request
- **WHEN** a tab closes while app-server is waiting for that tab's approval or user input
- **THEN** the system cancels or interrupts only that tab's work and leaves requests owned by other tabs active

#### Scenario: Response has no live owner
- **WHEN** a late response arrives after its owning session detached
- **THEN** the host discards it without delivering it to another session or treating it as a process failure

### Requirement: Thread notifications remain isolated
The system SHALL associate every root thread with exactly one open Codex session and SHALL deliver thread-scoped notifications only to that owner. Notifications for unowned stored threads SHALL NOT alter any open tab.

#### Scenario: Two root threads run concurrently
- **WHEN** two tabs have active turns on different root threads
- **THEN** each tab receives only its own turn, item, status, usage, and error updates

#### Scenario: Unowned thread emits a notification
- **WHEN** the shared connection receives activity for a thread no open session owns
- **THEN** no tab changes its transcript, running state, approval state, or usage display

#### Scenario: Root ownership changes during resume
- **WHEN** a tab leaves one root thread and successfully resumes another
- **THEN** the old root is detached before the new root becomes the tab's notification owner

### Requirement: Descendant threads follow their root owner
The system SHALL map each Codex descendant thread to the session tree containing its root and SHALL keep descendant activity outside both unrelated tabs and the root transcript. Relationship discovery and early notification buffering SHALL be bounded.

#### Scenario: Child agent starts under one tab
- **WHEN** a root thread creates a child agent while another Codex tab is open
- **THEN** the child appears only in its root tab's Background Tasks state and does not affect the unrelated tab

#### Scenario: Child notification precedes relationship discovery
- **WHEN** a descendant notification arrives before its parent relationship is known
- **THEN** the system retains only bounded candidate state until ownership is proven and never guesses an unrelated root owner

#### Scenario: Restored root discovers descendants
- **WHEN** a tab resumes a root thread that already has descendants
- **THEN** descendant discovery assigns those threads to the resumed root and later live notifications merge without duplicates

#### Scenario: Descendant closes
- **WHEN** app-server reports that a descendant thread closed or was deleted
- **THEN** its host routing entry is removed without removing its root session

### Requirement: Each session may select its own gateway
For every new or resumed custom-endpoint Codex thread, the system SHALL send that profile's stable provider ID, model override when configured, gateway URL, Responses API selection, and credential environment name in thread-scoped configuration. A default Codex profile SHALL continue using app-server's normal authentication and provider configuration.

#### Scenario: Two custom profiles start together
- **WHEN** two tabs use custom Codex profiles with different gateway URLs
- **THEN** each thread sends model requests to its own configured endpoint through a separate app-server

#### Scenario: Custom thread resumes after host replacement
- **WHEN** a custom-provider thread is resumed on a new host generation
- **THEN** the resume request supplies the provider definition again so the persisted provider ID resolves without relying on the prior process

#### Scenario: Default and custom profiles coexist
- **WHEN** one tab uses normal Codex authentication and another uses a custom gateway
- **THEN** each thread retains its own provider selection and neither selection changes the other

#### Scenario: Gateway fails
- **WHEN** one thread's gateway rejects or loses a model request
- **THEN** the error reaches that thread's tab without marking the shared host or unrelated threads as failed

### Requirement: Shared-host credentials remain distinct and secret-safe
The system SHALL derive a stable, distinct environment name for every custom Codex provider, SHALL place each decrypted API key only in that generated host environment entry, and SHALL reference only the environment name from thread configuration. Raw API keys SHALL NOT enter JSON-RPC payloads, normal logs, diagnostics, host identity text, or user-facing errors.

#### Scenario: Two custom profiles use different keys
- **WHEN** two custom profiles start hosts
- **THEN** each host receives only its selected launch environment, without merging credentials from other profiles

#### Scenario: Credential names match but values differ
- **WHEN** two launches use the same credential environment name with different values
- **THEN** separate hosts preserve each value without a collision error

#### Scenario: Host startup fails
- **WHEN** process diagnostics include environment-related output
- **THEN** retained and displayed diagnostics omit all raw profile credentials

### Requirement: Process-global catalogs are not misattributed
The system SHALL treat app-server model discovery as host-scoped and SHALL NOT present one custom gateway's models as another gateway's provider catalog. A model explicitly configured by a custom profile SHALL remain selectable even when the host-scoped catalog does not list it.

#### Scenario: Custom gateway is not represented by model discovery
- **WHEN** a custom profile names a model absent from the host-scoped model list
- **THEN** the tab keeps the configured model available and does not label unrelated discovered models as belonging to that gateway

#### Scenario: Default profile uses discovered models
- **WHEN** a default Codex profile opens on the shared host
- **THEN** its model picker may use the host-scoped model list as before

### Requirement: Closing one session preserves the others
When a Codex session closes or is replaced, the system SHALL interrupt only its active turn when interruption is required, send `thread/unsubscribe` for its root, remove its request and thread ownership, and release its host reference without shutting down a host retained by another session.

#### Scenario: Close one of two idle tabs
- **WHEN** one of two attached Codex tabs closes
- **THEN** its thread is unsubscribed and the other tab remains ready on the same host

#### Scenario: Close one active tab
- **WHEN** one tab closes during an active turn while another tab also has work on the host
- **THEN** only the closing tab's turn is interrupted and the other turn continues

#### Scenario: Replace a session in place
- **WHEN** `/new`, profile restart, or history restore replaces a tab's Codex session
- **THEN** the replacement obtains its host reference before the outgoing session releases its reference, avoiding an unnecessary host stop between them

### Requirement: Host failure affects only its attached sessions
The system SHALL detect unexpected shared-host exit, mark every attached Codex session unavailable, preserve each known root thread ID and visible transcript, and provide recovery that creates at most one replacement host per launch. Sessions on other hosts SHALL remain available.

#### Scenario: Shared host exits unexpectedly
- **WHEN** app-server exits while several Codex tabs are attached
- **THEN** every affected tab stops accepting prompts and displays a host-stopped error without losing its transcript or recovery identity

#### Scenario: Several tabs retry together
- **WHEN** multiple affected tabs request recovery concurrently
- **THEN** they join one replacement-host startup and resume their own saved root threads after initialization

#### Scenario: Resume after host recovery
- **WHEN** an affected tab resumes successfully on the replacement host
- **THEN** it becomes ready without creating an empty thread or duplicating transcript rows already retained by the tab

#### Scenario: Replacement host fails to start
- **WHEN** shared recovery cannot start or initialize app-server
- **THEN** all waiting tabs receive the bounded startup failure and remain recoverable for a later retry
