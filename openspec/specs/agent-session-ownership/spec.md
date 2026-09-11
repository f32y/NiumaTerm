# Agent Session Ownership Specification

## Purpose

Define independent Agent session lifetime, shared conversation ownership, command access, resource cleanup, and application discovery while preserving ordinary conversation behavior.

## Requirements

### Requirement: Logical session lifetime is independent of presentation
The system SHALL retain one logical owner and one provider event consumer per Agent session. Attaching or replacing presentation SHALL preserve the provider identity, backend generation, and retained conversation without starting, resuming, or sending provider work. Detaching all views SHALL leave execution active while the logical owner remains open.

#### Scenario: Output and interaction arrive without a renderer
- **WHEN** every view detaches from an open session and its provider emits output and an input request
- **THEN** the session retains the output and pending request without requiring a renderer
- **AND** a newly attached view reads that same conversation and can answer the request once without another provider start

### Requirement: Session transitions complete before presentation
The system SHALL apply provider events, delivery confirmation, readiness, replay, and recovery accounting to canonical session state before publishing presentation changes. It SHALL preserve bounded event processing and ordered adjacent-delta merging, and reject events from an obsolete backend generation.

#### Scenario: Readiness repeats during accepted work
- **WHEN** readiness repeats after the session has accepted a turn
- **THEN** the session preserves the active turn and selected settings
- **AND** accepted messages are confirmed once without requiring paint or animation progress

#### Scenario: An obsolete backend emits an event
- **WHEN** an event arrives from a backend generation that has been replaced
- **THEN** it changes neither retained content nor delivery and interaction state

### Requirement: Command access follows the current presentation binding
The system SHALL distinguish read access from command access. Only the current command binding of an open session SHALL admit composer and provider-control commands. Delayed presentation effects SHALL be rejected when their backend generation or command binding is obsolete.

#### Scenario: A detached composer attempts to send
- **WHEN** a replacement composer has acquired the current binding and an old composer callback attempts a submission
- **THEN** the old submission is rejected without provider dispatch or canonical content mutation

### Requirement: Readers share content and retain independent presentation
The system SHALL retain root and detail conversation content in session-owned storage. Readers SHALL keep independent layout, folding, selection, scroll, animation, and preview state. Streaming updates SHALL update indexed content without copying the full conversation for each reader, using bounded revision tracking to invalidate derived state.

#### Scenario: A reader misses streamed revisions
- **WHEN** two readers share a long conversation and one misses more updates than the bounded revision history retains
- **THEN** that reader recovers current content from shared storage
- **AND** unchanged retained text is not copied and each reader preserves its own folding state

#### Scenario: Retention changes entry indices
- **WHEN** old entries are removed from the front of retained content
- **THEN** readers invalidate presentation associated with the previous content generation so it cannot be applied to different entries

### Requirement: Accepted attachments survive view replacement
The system SHALL retain accepted immutable image resources and required provider scratch files for the session lifetime. Unsent images and GPUI previews SHALL remain presentation resources. Rejected submissions SHALL preserve pending images.

#### Scenario: The provider reads an accepted image after detachment
- **WHEN** a view submits an image successfully and detaches before the provider reads its scratch file
- **THEN** the accepted bytes and file remain available
- **AND** a replacement view can present the accepted image from the same conversation

### Requirement: Logical close ends execution and releases owned resources
Closing a logical session SHALL invalidate command bindings and pending execution, remove its registry entry, prevent maintenance restart, and release its backend with bounded shutdown before removing its own scratch resources. Observers SHALL NOT keep closed execution alive. Other sessions using a shared provider host SHALL remain active.

#### Scenario: A closed session receives a late callback
- **WHEN** a session closes while observers or maintenance handles still exist and a late callback attempts to mutate or restore it
- **THEN** the callback cannot dispatch provider work or restore retained content
- **AND** the closed session releases its backend reference and scratch resources without removing another session's resources

### Requirement: Detail refresh belongs to the session
The system SHALL retain child and workflow conversations separately from root content and scope reads by parent and provider identity. Visible readers SHALL express refresh interest while the session owns one refresh loop per detail family, preserving the existing cadence and avoiding overlapping loop reads. Reader selections SHALL remain independent.

#### Scenario: Two readers select different workflow members
- **WHEN** two readers select different members of the same session
- **THEN** each reads its selected member's retained content through the shared refresh work
- **AND** a late result cannot replace another member's or the root conversation's content

### Requirement: Application consumers discover session identity
The system SHALL register sessions weakly before startup and discover installation-maintenance participants by session identity, including sessions without renderers. Lifecycle publication SHALL originate from the session rather than each reader. Notification navigation SHALL resolve the current ordinary tab for the session.

#### Scenario: Completion has multiple or no attached readers
- **WHEN** a session completes a turn with multiple readers or no attached reader
- **THEN** it publishes one semantic completion for that turn
- **AND** attaching a later reader does not repeat completion publication

#### Scenario: Maintenance enumerates a session without presentation
- **WHEN** installation maintenance enumerates an open session with no renderer
- **THEN** that session participates once and readiness is determined from canonical work, compaction, interaction, and pending-operation state
- **AND** attaching another reader does not add a participant or change readiness
