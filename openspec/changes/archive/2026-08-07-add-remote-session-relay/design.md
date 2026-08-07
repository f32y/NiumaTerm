# Design: add-remote-session-relay

## Context

`crates/remote_session_hub` provides a complete detachable session model. `RemoteSessionHub::attach()` returns a VT checkpoint as `SessionSnapshot { base_seq, vt, cols, rows }` and a sequenced stream of `SessionEvent` values. Subscription-queue overflow silently disconnects the subscriber, which can reconnect from a new checkpoint. The crate documentation assigns networking, authentication, and transport encoding to the host process, which does not yet use the crate.

The repository has no direct networking or encryption dependencies: `rustls`, `tungstenite`, `snow`, and `quinn` are absent from Cargo.lock. Vendored GPUI brings in `tokio`, `mio`, and `smol` with minimal features, while `serde` and `base64` are available. The application is Windows-only.

The reference implementation is paseo, a self-hosted AI agent orchestrator. Its daemon and Clients make outbound connections to an untrusted relay, use end-to-end NaCl encryption, and pair by distributing a public key out of band through a QR code. Its known weaknesses are lack of replay protection because it uses random nonces without counters, no persistent Client identity, and all-or-nothing authorization. This design directly addresses the first two weaknesses.

## Goals / Non-Goals

**Goals:**

- Let multiple NiumaTerm Clients access terminal sessions on one Host through a public relay.
- Use outbound connections from both Host and Client, crossing NAT without exposing ports.
- Treat the relay as untrusted: it sees only ciphertext and routing metadata, while either endpoint rejects replayed or modified frames.
- Give every Client a static device identity and let the Host revoke devices individually through an authorized-device list.
- Reuse the hub checkpoint model directly for reconnection, without acknowledgements or retransmission windows in the protocol.

**Non-Goals:**

- Fine-grained in-session permissions such as read-only viewing or per-session authorization. An authorized device is a full operator, matching paseo.
- A UI entry point for direct LAN connections. The Noise layer can run over direct TCP, but this change implements only the relay path.
- Cross-platform support. The Host depends on ConPTY and is Windows-only. The Client protocol is platform-neutral but is not validated elsewhere in this change.
- Horizontal relay scaling or multiple relay instances. An in-memory routing table in one instance is sufficient.
- File transfer, port forwarding, or other non-terminal features.

## Decisions

### D1: Use outbound WebSockets from both sides through a relay

Host and Client each establish an outbound WSS connection to the relay, which pairs them by `host_id` and relays bytes in both directions.

- QUIC through quinn would avoid one TLS handshake, but its ecosystem maturity, proxy traversal, and debugging tools are weaker than WebSocket. Paseo has validated the WebSocket topology.
- A directly listening Host would require firewall configuration or a public IP and would violate the zero-exposure goal.

### D2: Use the Noise Protocol through `snow` for end-to-end encryption

- Noise IK for normal connections and XX for the first pairing connection provide mutual identity authentication and forward secrecy through ephemeral keys during the handshake.
- Each direction uses an increasing nonce counter in the transport phase, so a frame replayed by the relay fails decryption.
- mTLS would impose excessive certificate management on individual users. Noise keys serve directly as identities, and the pairing code distributes trust.
- Implementing X25519 and AEAD directly would recreate Noise and risk nonce reuse.

### D3: Identify and pair devices with static keys

- The Host's static X25519 key pair is its identity. `host_id = hex(SHA-256(host_pubkey))[..16]` is the relay routing key.
- Each Client also has a static key pair as its device identity. During an IK handshake, the Host verifies that the Client's static public key appears in `authorized_devices.json`. Removing an entry revokes that device.
- For first pairing, the Host creates a one-time pairing code with a five-minute TTL, encoded in base32 from `{relay_url, host_id, host_pubkey, pairing_token(16B)}`. The user transfers it out of band to the Client. The Client uses an XX handshake to exchange static public keys, then submits pairing_token inside the encrypted channel. After validation, the Host persists the Client public key and invalidates the token.
- A shared password, used by paseo's direct mode, has no device identity. One leak forces every user to change the password and offers no selective revocation.

### D4: Use opcode-based binary frames inside the Noise channel

```
Control frame 0x00: postcard-serialized enum
  C→H: ListSessions | Open(SessionOptions') | Attach(id) | Detach | Kill(id)
  H→C: SessionList(Vec<SessionInfo'>) | Attached(SessionSnapshot') | Error(...)
Data frames: [opcode u8][session_id u64 LE][payload]
  0x01 Output  payload=[seq u64][bytes]   ← SessionEvent::Output
  0x02 Input   payload=bytes              → hub.write_input()
  0x03 Resize  payload=[cols u16][rows u16]
  0x04 Exited  payload=[seq u64]
```

- Postcard is compact, works with no_std, and fits the serde ecosystem. Raw data frames avoid wrapping terminal output in a serializer and follow paseo's demultiplexing approach.
- Frame types are transport-specific mirror types marked with a prime and remain decoupled from hub types, leaving the hub unchanged.

### D5: Implement the relay first as a Cloudflare Durable Object

- Place a TypeScript Worker and DO under `relay/` and deploy it with wrangler. Each `host_id` maps to one DO instance through `idFromName`. WebSocket hibernation reduces idle cost.
- This choice has no VPS, systemd, or certificate maintenance because Cloudflare edge services supply TLS. The free quota covers personal use, global edge locations shorten access paths, and paseo has validated the shape in production. Its `packages/relay/src/cloudflare-adapter.ts` can guide the port.
- Follow paseo v2's connection model: one Host control socket for registration and `connected`, `disconnected`, and `sync` notifications, plus one Host data socket for each Client connection. The Client socket and data socket are paired by connection_id. This keeps inner frames opaque to the DO; multiplexing every Client over one connection would require parsing an outer wrapper.
- Buffer Client frames until the data socket is ready, up to 1 MiB. On overflow, disconnect the Client so it reconnects. Limit a host_id to 16 concurrent Client sockets and reject excess connections with 429.
- Require an access token configured as a Worker secret to prevent public abuse and host_id squatting. Under end-to-end encryption, a squatter can cause only denial of service. A signed registration challenge remains an upgrade path.
- A self-hosted Rust executable using tokio, tungstenite, and a DashMap routing table would unify the language stack and avoid provider dependence. It remains a later option because both implementations can expose the same opaque-byte relay protocol without Host or Client changes.

### D6: Store private keys with DPAPI

Store Host and Client static private keys under `%LOCALAPPDATA%\NiumaTerm\`, encrypted with `CryptProtectData` from the already-installed `windows` crate and bound to the current Windows user. Plaintext protected only by ACLs would save little code and would not resist offline copying.

### D7: Reconnect by performing a new handshake and Attach

After reconnecting, the Client performs another Noise handshake, sends `Attach`, and renders a fresh snapshot. There is no resume token or event-replay buffer because the hub checkpoint semantics already guarantee that every byte appears either in the snapshot or in a later event. Session lifetime on the Host is independent of the connection, so disconnection does not terminate the shell.

### D8: Run the Host network engine on an independent async runtime

Place the network engine in `crates/remote_net` without GPUI dependencies, leaving `crates/app` to add only UI. `HostHandle::start` launches an independent tokio runtime thread inside the main process. One standard-library bridge thread per hub `SessionSubscription` transfers events into tokio channels. This avoids adding a tungstenite adapter to GPUI's smol ecosystem and permits headless end-to-end testing of the entire Host path in `tests/host_e2e.rs`.

## Risks / Trade-offs

- [Incorrect Noise integration could defeat the security model] Remote protocol work lands first, with loopback tests requiring one-byte modification, replay, and an unauthorized Client public key to fail.
- [The relay is a single point of failure] Host and Client reconnect with exponential backoff. The relay is stateless and recovers after restart, while sessions stay alive on the Host and only online connectivity is lost.
- [A leaked access token permits host_id squatting and denial of service] This is a known limit. End-to-end encryption still protects confidentiality. A key-signed challenge during registration is the upgrade path.
- [Cloudflare pricing or policy changes create provider dependence] The relay protocol forwards opaque bytes, so a self-hosted Rust implementation can replace it without Host or Client changes.
- [Hibernation can discard in-memory state] Store all socket routing ownership with `serializeAttachment`. Persist frame buffers in DO storage or accept that hibernation can drop them during the short period before a data socket is ready. Integration tests cover waking from hibernation.
- [A pairing code can be intercepted during its five-minute lifetime] An interceptor could pair as an authorized device. Keep the TTL short, make the code one-time, notify the Host UI when a new device connects, and let users revoke devices at any time.
- [The app's TerminalSession and hub RemoteSession models overlap] Accept the duplication in this change to avoid modifying the local terminal path. Local tabs can later converge on the hub.
- [Full tokio features increase compile time and executable size] This is acceptable because only the app and relay_server use them.

## Migration Plan

This is additive and does not change existing behavior. Implement it in five independently testable steps listed in `tasks.md`. Deploy the relay to Cloudflare with `wrangler deploy` and `wrangler secret put ACCESS_TOKEN`; Cloudflare edge services provide TLS. Use `wrangler dev` to run a local DO for integration tests. Rollback consists of leaving the Host service disabled, so the new code does not affect the local terminal path.

## Open Questions

- Is a short text pairing code sufficient, or should this change also render a QR code and add its dependency? Prefer text only for the initial version.
- Should frame buffers be discarded during hibernation because the interval is short, or persisted to DO storage at the cost of one extra I/O operation? Prefer discarding them and relying on Client reconnection.
- Which `SessionOptions'` fields should a remote Client control? Arbitrary shell, cwd, and environment values let an authorized device run any command, matching the full-operator model, but the UI might still default to opening only the default shell.
