# NiumaTerm Relay

Cloudflare Worker + Durable Object that forwards end-to-end-encrypted bytes
between a NiumaTerm host and its clients. The relay is untrusted: it only ever
sees ciphertext plus routing metadata (host id, connection id, IP, timing,
sizes). Host↔client confidentiality and device authentication are enforced by
the Noise channel in the app, not here.

One Durable Object instance per `host_id` (`idFromName`), so every socket for a
host lands on the same instance regardless of which edge accepted the
connection. Sockets use the hibernation API, so an idle host costs nothing.

The worker's `package.json`, `package-lock.json`, `tsconfig.json`,
`wrangler.toml`, and `.dev.vars` live in the repository root, so every command
below runs from there rather than from this directory. Only the source stays
here, and `wrangler.toml` points at it.

## Deploy

```bash
npm install
npx wrangler deploy
```

Set the access token that hosts must present to register (any high-entropy
string; clients never send it):

```bash
npx wrangler secret put ACCESS_TOKEN
```

Bind a custom domain so hosts and clients use a stable `wss://` URL (Cloudflare
provides the TLS certificate automatically):

1. Cloudflare dashboard → your Worker → **Settings → Domains & Routes → Add
   custom domain**, e.g. `relay.example.com`.
2. Hosts connect to `wss://relay.example.com/ws`; that URL goes into each
   pairing code the host generates.

## Local development

```bash
npm run dev   # wrangler dev on 127.0.0.1:8787
```

`.dev.vars` in the repository root already holds the `ACCESS_TOKEN` the local
instance uses. It is committed on purpose: the value is a fixed local-dev
string that the ignored Rust integration tests hardcode, so the two sides
cannot drift. Production tokens are set with `wrangler secret put ACCESS_TOKEN`
and never enter the repository.

The Rust integration tests connect to this local instance:

```bash
cargo test -p nmt_remote_net --test relay_integration -- --ignored
cargo test -p nmt_remote_net  --test host_e2e          -- --ignored --test-threads=1
```

## Protocol

WebSocket upgrades at `/ws` with query parameters:

| Param           | Values             | Meaning                                             |
| --------------- | ------------------ | --------------------------------------------------- |
| `host_id`       | 16 hex chars       | Routing key = `sha256(host_public_key)[..8]`.       |
| `role`          | `host` \| `client` | Which side is connecting.                           |
| `connection_id` | `conn_<uuid>`      | Host data sockets only; pairs with a client socket. |

- **Host control socket** (`role=host`, no `connection_id`): one per host,
  requires `Authorization: Bearer <ACCESS_TOKEN>`. Receives JSON notifications
  `{type: connected|disconnected|sync, ...}`.
- **Host data socket** (`role=host&connection_id=<cid>`): one per client,
  opened by the host in response to a `connected` notification.
- **Client socket** (`role=client`): the relay assigns `conn_<uuid>` and
  notifies the host. No token required — pairing and the Noise handshake are
  the client's gate.

Close codes: `4400` bad request, `4401` unauthorized, `4404` host offline,
`4429` client buffer overflow, `1012` retryable (host gone / socket replaced).
