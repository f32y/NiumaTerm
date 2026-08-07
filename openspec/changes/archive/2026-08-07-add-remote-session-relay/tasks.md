# Tasks: add-remote-session-relay

## 1. remote_protocol: protocol and encrypted channel

- [x] 1.1 Create `crates/remote_protocol`, add it to the workspace, and add dependencies on snow, postcard, serde, rand, sha2, and base32.
- [x] 1.2 Define transport mirror types for SessionOptions', SessionInfo', and SessionSnapshot' plus control-frame enums; implement postcard encoding and decoding with round-trip tests.
- [x] 1.3 Implement data-frame encoding and decoding for 0x01 Output, 0x02 Input, 0x03 Resize, and 0x04 Exited, with one frame per transport message and first-byte dispatch. Return errors for unknown frame types and add round-trip tests.
- [x] 1.4 Wrap Noise channels with IK and XX handshakes, transport encryption and decryption, and access to `remote_static` during the handshake so the Host can check its authorization list.
- [x] 1.5 Add loopback security tests requiring one-byte modification, replay, and an unauthorized Client public key in an IK handshake to fail.
- [x] 1.6 Encode and decode base32 pairing codes containing `{relay_url, host_id, host_pubkey, pairing_token}`, including tests for damaged input.

## 2. relay: Cloudflare Durable Object server

- [x] 2.1 Create the `relay/` Worker project with TypeScript, wrangler, and workers-types. Route upgrade requests by `host_id` to the DO instance from `idFromName(host_id)`, and reject missing parameters or invalid roles.
- [x] 2.2 Implement the Host control socket with access-token validation through a Worker secret, replacement of duplicate registrations, and `connected`, `disconnected`, and `sync` control messages.
- [x] 2.3 Assign connection_id values as `conn_<uuid>`, pair Client sockets with Host data sockets by connection_id, relay opaque bytes in both directions, and use clear close codes when the target Host is offline, including 404, 409, and 401 during upgrade.
- [x] 2.4 Buffer frames until the data socket is ready, up to 200 frames, and disconnect with 4429 on overflow. Cascade Host control-socket disconnection to all of its Clients; close the data socket and send `disconnected` when one Client disconnects.
- [x] 2.5 Support hibernation by accepting every socket through `acceptWebSocket`, storing routing ownership with `serializeAttachment`, and looking up routes by tag without depending on in-memory state. Accept the designed loss of frame buffers during hibernation.
- [x] 2.6 Add integration tests against a local DO from `wrangler dev`: complete a Noise handshake from both Rust endpoints through the relay, including buffer flushing and an encrypted echo round trip; expect 401 for invalid or missing tokens, 404 for an offline Host, and a 4429 disconnection on buffer overflow.

## 3. Host: main-process hosting

- [x] 3.1 Create `crates/remote_net` as a headless-testable network engine without GPUI dependencies. Make `HostHandle::start` launch a dedicated tokio runtime thread, create and persist the Host static key with DPAPI, and derive host_id.
- [x] 3.2 Register an outbound relay control socket, keep it alive with WebSocket pings, and reconnect with `min(30s, 1s × attempt)` backoff. Handle `connected`, `disconnected`, and `sync` by opening or closing each connection_id data socket.
- [x] 3.3 Map ListSessions, Open, Attach, Detach, Kill, and Error control frames to RemoteSessionHub; map Input and Resize data frames to the hub; use the first message's mode byte to distinguish IK from pairing.
- [x] 3.4 Pump Output and Exited events through one standard-library bridge thread per subscription into a tokio channel, splitting Output by MAX_DATA_LEN. Remove only the subscriber on overflow or disconnection so the shell remains alive.
- [x] 3.5 Implement one-time pairing codes with a five-minute TTL and `take()` invalidation, pairing_token validation inside an XX handshake, persistence to `authorized_devices.json`, and disconnection from `revoke_device`.
- [x] 3.6 Add an end-to-end test through the `wrangler dev` relay: pair, open cmd.exe, exchange an echo marker through encryption, disconnect, and reconnect to a new snapshot containing interim output. Reject unauthorized IK handshakes and forged tokens.

## 4. Client: connection runtime and remote-tab UI

- [x] 4.1a Generate and persist the Client device key with DPAPI by reusing keys.rs, and implement the XX pairing connector `client_connect_pair` with Host public-key pinning.
- [x] 4.1b Implement synchronous runtimes for `open_remote_session` and `list_remote_sessions`. Run tokio on a dedicated thread and expose `RemoteSession { output: std mpsc<SessionByteEvent>, send_input, send_resize }`, matching the shape required by NetPty. Validate byte-stream round trips and session listing end to end.
- [x] 4.2 Add Client pairing through `pair_with_code` in Settings, display known Hosts with a Forget action, perform connections in a background task, and show failures as notifications without blocking the UI.
- [x] 4.3 Implement `NetPty: EventedPty` with a SoftReady signal. Feed snapshot.vt before Output in the reader, send writes through `send_input`, send size changes through `send_resize`, and map channel closure to Exited. Connect it through the generic PtyPipe path. Add `TerminalSession::new_remote` and surface and pane support for `spawn_remote`, reusing the existing render, wake, and pump path. Add a NetReader unit test and `remote_session_renders_through_net_pty` against a real relay.
- [~] 4.4 Implement `NewRemoteTab` on Ctrl+Shift+R to connect to the first paired Host and open a remote tab. Defer a multi-Host session picker using `list_remote_sessions` and `AttachTarget::Existing`; add it when users need more than the first Host.
- [x] 4.5 Reconnect automatically with five bounded retries, perform another IK handshake, Attach the original session_id, rebuild the screen from a new snapshot, and discard Output already included by base_seq. Apply 15-second timeouts to all Client network waits and manually test a relay restart with `NMT_RELAY_BOUNCE=1`.

## 5. Pairing management and completion

- [ ] 5.1 Add a Host authorized-device UI with listing and removal, disconnect active connections after removal, reject later handshakes, and notify users when a new device connects.
- [ ] 5.2 Add the Host service toggle and pairing-code display.
- [x] 5.3 Document relay deployment in `relay/README.md`, including `wrangler deploy`, `wrangler secret put ACCESS_TOKEN`, custom-domain binding, local development, and the protocol table.
- [ ] 5.4 Run the full test set and launch two instances with `--testing` to manually validate pairing, a remote session, and device revocation.
