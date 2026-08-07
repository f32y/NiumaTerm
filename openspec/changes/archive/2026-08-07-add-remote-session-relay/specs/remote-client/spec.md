# remote-client

Remote tabs in the Client side of the `crates/app` remote module.

## ADDED Requirements

### Requirement: Connect with a pairing code
The Client SHALL accept a pairing code pasted or entered by the user and parse its relay_url, host_id, Host public key, and pairing_token. After completing an XX pairing handshake, it SHALL persist the Host information and local device key. Later connections SHALL use an IK handshake without requiring another pairing operation.

#### Scenario: First pairing
- **WHEN** the user enters a valid pairing code
- **THEN** the Client creates or loads its device key, completes pairing, and shows the Host in the known-host list

#### Scenario: Connect again
- **WHEN** the user connects to a paired Host
- **THEN** the Client completes an IK handshake directly, without a pairing-code step

### Requirement: Render and control a remote tab
The Client SHALL host a remote session in a tab. After Attach, it SHALL initialize terminal rendering from the snapshot's VT state, then apply Output frames in seq order. It SHALL encode keyboard input in Input frames and send Resize frames when the tab size changes. Rendering SHALL reuse the existing terminal rendering pipeline.

#### Scenario: Render after attach
- **WHEN** the Client completes Attach
- **THEN** the tab immediately displays the snapshot content and applies later output incrementally

#### Scenario: Input and resize
- **WHEN** the user types in a remote tab or resizes the pane
- **THEN** the remote shell receives the input or new dimensions, and echo output returns in Output frames

### Requirement: Reconnect after disconnection
After a connection closes, the Client SHALL reconnect automatically with bounded backoff. After reconnecting, it SHALL perform a new handshake, Attach the original session_id, and rebuild all terminal state from a new snapshot. The tab SHALL display a disconnected state while reconnection is in progress.

#### Scenario: Brief network interruption
- **WHEN** the connection between the Client and relay is interrupted for several seconds and then restored
- **THEN** the tab briefly shows a reconnecting state and then restores display from a new snapshot without corrupted content

#### Scenario: Remote session has ended
- **WHEN** the session_id requested by Attach has exited before reconnection completes
- **THEN** the tab reports that the session ended and lets the user close it or open a new session

### Requirement: List remote sessions
After connecting, the Client SHALL be able to request SessionList and display remote sessions with their title, shell, exit state, and attach count. The user can then Attach an existing session or open a new one.

#### Scenario: List and attach an existing session
- **WHEN** the Host has a running session and the Client requests the list
- **THEN** the list shows that session and the user's selection attaches successfully
