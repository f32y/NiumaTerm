# Proposal: add-remote-session-relay

## Why

NiumaTerm sessions can currently be used only on the local machine. `crates/remote_session_hub` already provides a detachable headless session model with VT checkpoints and a sequenced event stream, but the main process does not use it and has no network layer. Users need to access terminal sessions on a home or office computer from another computer. Because both sides are usually behind NAT, they need a public relay that cannot inspect session content.

## What Changes

- Add `crates/remote_protocol`, a shared protocol crate with Noise IK/XX end-to-end encrypted channels through `snow`, binary frame codecs for control and terminal data, and pairing-code codecs.
- Add `relay/`, a Cloudflare Worker and Durable Object relay server in TypeScript, deployed with wrangler and adapted from paseo's cloudflare adapter. Each host_id maps to one DO instance. A Host control socket and one data socket per Client are paired by connection_id and relay opaque ciphertext bytes. A replaceable self-hosted Rust relay is deferred.
- Add `crates/remote_net`, a UI-independent network engine that supports headless testing. It provides DPAPI key storage, an authorized-device list, a Host service that manages relay control and data sockets, maps control frames to `RemoteSessionHub`, pumps events, and handles pairing, plus a Client connector for IK connections and XX pairing.
- Add a remote UI layer to `crates/app`, powered by `remote_net`:
  - Host: a service toggle, pairing-code display, and authorized-device management.
  - Client: remote tabs that connect to a Host through the relay and reuse the existing terminal rendering pipeline.
- Add dependencies on `tokio` with full features, `tokio-tungstenite` with `rustls`, `snow`, `postcard`, `rand`, and `sha2`.
- Leave `crates/remote_session_hub` unchanged because its documentation assigns networking, authentication, and encoding to the host process.

## Capabilities

### New Capabilities

- Remote transport protocol: an end-to-end encrypted Host-to-Client channel using Noise IK/XX, replay protection, forward secrecy, and a binary frame format with postcard control frames and opcode-based terminal data frames.
- `relay-server`: a public Cloudflare Durable Object relay with outbound WebSockets from both sides, one DO instance per host_id, separate control and data sockets paired by connection_id, ciphertext forwarding, access-token abuse protection, and hibernation support.
- `remote-host`: remote session hosting in the main process, including device pairing and authorization, DPAPI storage for static keys, control-frame mapping to RemoteSessionHub, and session survival across disconnections.
- `remote-client`: remote tabs with pairing-code setup, snapshot initialization followed by streamed events, reconnection from a new checkpoint, and upstream input and resize events.

### Modified Capabilities

None. The `openspec/specs/` directory was empty when this change was proposed, so no existing specification was affected.

## Impact

- Code: add `crates/remote_protocol` in Rust and `relay/` as a TypeScript Worker; add a remote module and pairing and remote-tab UI to `crates/app`; update root workspace members and dependencies in `Cargo.toml`.
- Dependencies: add tokio-tungstenite, rustls, snow, postcard, and sha2 on the Rust side; tokio, serde, and base64 were already present in the lockfile. Add the wrangler and workers-types toolchain for the relay.
- Deployment: deploy the relay to Cloudflare with `wrangler deploy`. The free quota is sufficient, and Cloudflare edge services provide TLS. Host and Client use outbound connections, so neither exposes a port.
- Security: add a per-user key file at `%LOCALAPPDATA%\NiumaTerm\host-key`, encrypted with DPAPI, and an authorized-device list file.
