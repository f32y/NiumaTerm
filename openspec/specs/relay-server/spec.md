# Relay Server Specification

## Purpose
Define the public relay service that routes Host and Client WebSockets through Cloudflare Durable Objects while keeping the end-to-end session protocol opaque.

## Requirements

### Requirement: Route by host_id to a Durable Object instance
The Worker SHALL route upgrade requests for `wss://<relay>/ws?host_id=<id>&role=<host|client>[&connection_id=<cid>]` to the DO instance returned by `idFromName(host_id)`. All sockets for the same host_id MUST reach the same instance.

#### Scenario: Connections with the same host_id converge
- **WHEN** a Host and multiple Clients connect with the same host_id
- **THEN** the same DO instance handles them, and Client frames can reach that Host

#### Scenario: Required parameters are missing
- **WHEN** an upgrade request omits host_id or supplies an invalid role
- **THEN** the Worker rejects the upgrade with a clear error code

### Requirement: Register the Host control socket
The DO SHALL accept a Host control socket with `role=host` and no connection_id, with at most one such socket per host_id. A new registration SHALL replace and close the previous socket. Registration MUST include a valid access token configured as a Worker secret. The DO SHALL immediately close a connection with an invalid token without registering it. Through the control socket, the DO SHALL send the Host JSON control messages: `connected` and `disconnected` when an individual connection_id comes online or goes offline, and `sync` with the complete set of online connection_id values for reconciliation after reconnection.

#### Scenario: Registration succeeds and receives notifications
- **WHEN** a Host establishes a control socket with a valid token and a Client then connects
- **THEN** the Host receives a `connected` message containing that connection_id on the control socket

#### Scenario: Token is invalid
- **WHEN** the control-socket upgrade request has a missing or incorrect token
- **THEN** the DO closes the connection without registering it

#### Scenario: Reconcile after control-socket reconnection
- **WHEN** Clients remain online while the Host control socket disconnects and reconnects
- **THEN** the Host receives a `sync` message and opens a data socket for each online connection_id

### Requirement: Pair and relay Client and Host data sockets
The DO SHALL assign a connection_id in the form `conn_<uuid>` to a Client socket with `role=client` when the request does not provide one. It SHALL relay data in both directions between that Client socket and the Host data socket with the same connection_id and `role=host&connection_id=<cid>`. The relay MUST treat frames as opaque bytes and SHALL NOT parse, modify, or persist their inner content.

#### Scenario: Relay in both directions
- **WHEN** a Client socket and its matching Host data socket are ready
- **THEN** each frame sent by either side reaches the other side unchanged

#### Scenario: Target Host is offline
- **WHEN** the host_id requested by a Client has no registered control socket
- **THEN** the DO closes that Client connection with a clear close code

### Requirement: Buffer frames until the Host data socket is ready
Until the Host data socket connects, the DO SHALL buffer Client frames for that connection_id, up to 1 MiB measured in bytes. It SHALL flush the frames in order when the data socket connects. If the limit is exceeded, the DO SHALL close the Client connection so that it reconnects.

#### Scenario: Flush buffered frames
- **WHEN** a Client sends frames before the Host data socket is ready without exceeding the limit
- **THEN** the data socket receives every buffered frame in its original order after it connects

#### Scenario: Buffer overflows
- **WHEN** the buffered byte count exceeds 1 MiB
- **THEN** the DO closes the Client connection and discards the buffer

### Requirement: Limit the number of Client sockets
Client sockets do not carry a token. The DO SHALL limit each host_id to 16 concurrent Client sockets and reject additional connections with HTTP 429, preventing a party that knows the host_id from forcing the Host to open unlimited data sockets and perform handshakes.

#### Scenario: Connection limit is exceeded
- **WHEN** a host_id already has 16 online Client sockets and a seventeenth attempts to connect
- **THEN** the DO rejects it with 429 without affecting existing connections

### Requirement: Cascade disconnections
When a Host control socket disconnects, the DO SHALL close every Client and data socket for that host_id, causing Clients to reconnect. When one Client disconnects, the DO SHALL close its matching data socket and send `disconnected` through the control socket without affecting other Clients.

#### Scenario: Host goes offline
- **WHEN** the Host control socket disconnects
- **THEN** all Client connections for that host_id close and the registration is cleared

#### Scenario: One Client goes offline
- **WHEN** one Client socket disconnects
- **THEN** the matching data socket closes, the Host receives `disconnected`, and other Clients remain connected

### Requirement: Support hibernation
The DO SHALL manage every socket with the WebSocket hibernation API through `acceptWebSocket` and `serializeAttachment`. After the instance hibernates and wakes, each socket's role and connection_id ownership MUST be recoverable and relaying MUST continue.

#### Scenario: Wake after hibernation
- **WHEN** a socket receives a new frame after the DO hibernated while idle
- **THEN** the DO restores routing ownership from the attachment and resumes relaying normally
