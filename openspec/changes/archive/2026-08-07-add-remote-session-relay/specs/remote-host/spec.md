# remote-host

Remote session hosting in the Host side of the `crates/app` remote module within the main process.

## ADDED Requirements

### Requirement: Persist the Host identity and key
When remote access is enabled for the first time, the Host SHALL create a static X25519 key pair. It SHALL store the private key under `%LOCALAPPDATA%\NiumaTerm\`, encrypted with DPAPI (`CryptProtectData`), and derive `host_id` as `hex(SHA-256(host_pubkey))[..16]`.

#### Scenario: Create a key on first use
- **WHEN** the user starts the Host service for the first time
- **THEN** the Host creates a key pair, persists it with DPAPI encryption, and loads the same identity after restart

#### Scenario: Private-key file is copied to another user
- **WHEN** the key file is read in another Windows user context
- **THEN** DPAPI decryption fails, preventing use of that identity

### Requirement: Pair and authorize devices
The Host SHALL create a one-time pairing code with a five-minute TTL. After validating the pairing_token inside the XX handshake channel, it SHALL add the Client's static public key to the authorized-device list and invalidate the token. The Host SHALL let users view and remove authorized devices. Later IK handshakes from a removed device MUST be rejected.

#### Scenario: Pairing succeeds
- **WHEN** a Client submits the correct pairing_token in an XX handshake before the TTL expires
- **THEN** its public key is persisted in the authorized-device list, the token is invalidated, and later IK handshakes are accepted directly

#### Scenario: Token expired or was reused
- **WHEN** a Client submits an expired or previously used pairing_token
- **THEN** the Host rejects pairing and closes the connection

#### Scenario: Revoke a device
- **WHEN** the user removes a device from the authorized-device list
- **THEN** existing connections from that device close and later handshakes are rejected

### Requirement: Map control frames to RemoteSessionHub
The Host SHALL map control frames to hub APIs as follows: ListSessions to `list_sessions`, Open to `open`, Attach to `attach`, and Kill to `kill`. It SHALL map Input data frames to `write_input` and Resize frames to `resize`. It SHALL encode hub `SessionEvent::Output/Exited` values as matching data frames for attached Clients and return hub errors in Error control frames.

#### Scenario: Open and interact with a remote session
- **WHEN** an authorized Client sends Open followed by Attach
- **THEN** it receives Attached(snapshot), Input frames are written to the shell, and Output frames are sent in seq order

#### Scenario: Operate on a missing session
- **WHEN** a Client sends Attach for a closed session_id
- **THEN** it receives an Error frame with SessionNotFound semantics and the connection remains open

### Requirement: Keep session lifetime independent of connections
A Client disconnection or subscription-queue overflow SHALL remove only that subscriber and SHALL NOT terminate the shell process. Sessions SHALL continue while the Host process remains alive and can be restored by Attach after reconnection.

#### Scenario: Disconnection does not terminate a session
- **WHEN** the only attached Client disconnects
- **THEN** the shell continues running and a later Attach receives a new snapshot containing output produced during the disconnection

### Requirement: Connect to and reconnect with the relay
The Host SHALL register with the relay through an outbound control socket carrying host_id and an access token. When it receives `connected`, it SHALL open a data socket for that connection_id and perform a Noise handshake on it. When it receives `disconnected`, it SHALL close the matching data socket and subscription. When it receives `sync`, it SHALL reconcile the complete list by opening or closing data sockets. After the control socket disconnects, the Host SHALL reconnect automatically with bounded backoff such as `min(30s, 1s × attempt)`. It SHALL keep the connection alive with protocol-level WebSocket pings and treat a timeout as a disconnection.

#### Scenario: Client connection opens a data socket
- **WHEN** the control socket receives `connected` for a connection_id
- **THEN** the Host opens the matching data socket, completes a Noise handshake with the Client, and enters the session protocol

#### Scenario: Recover from a relay interruption
- **WHEN** relay unavailability disconnects the control socket and service later returns
- **THEN** the Host registers again automatically and rebuilds data sockets for connection_id values still online after receiving `sync`, without user intervention
