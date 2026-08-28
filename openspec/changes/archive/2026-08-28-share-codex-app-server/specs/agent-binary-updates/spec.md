## MODIFIED Requirements

### Requirement: Conversation restoration with the updated launcher
The system SHALL restart every recoverable suspended Claude tab through its configured launcher and reconnect it to the captured provider conversation. For suspended Codex tabs using the updated installation, the system SHALL start and initialize one replacement app-server, attach every recoverable tab to that host, and resume each captured thread independently.

#### Scenario: Claude conversation is restored
- **WHEN** a suspended Claude tab has a captured session ID
- **THEN** the system starts the configured launcher with that session ID, preserves the existing pane transcript, and enables input after the resumed session is ready

#### Scenario: Codex conversation is restored
- **WHEN** one or more suspended Codex tabs have captured thread IDs
- **THEN** the system starts app-server once through the updated launcher, initializes it once, sends one `thread/resume` per captured thread, restores each tab's provider context, and does not duplicate retained transcript rows

#### Scenario: One Codex resume fails
- **WHEN** the replacement host starts but one captured Codex thread cannot resume
- **THEN** that tab retains its transcript and recovery controls while other tabs finish resuming on the same host

#### Scenario: Untouched tab is restored
- **WHEN** a suspended tab had no conversation identity because it was untouched
- **THEN** the system starts a fresh provider conversation in the same retained tab, using the replacement shared host when the tab is Codex
