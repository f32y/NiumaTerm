// NiumaTerm remote-session relay.
//
// A host keeps a control socket open here. When a client connects, the
// host's room tells the host over that socket, the host opens a data socket
// for that client, and the room forwards binary frames between the two
// without reading them. Every frame is sealed end to end by the Noise
// channel the host and client run, so the relay sees only ciphertext and a
// compromised relay can cost availability, never confidentiality.
//
// Several users can share one relay. Each has their own access key, and a
// host belongs to the user whose key registered it: another user's key can
// neither reach it nor take its host id, and pairing slots are looked up
// among the presenting user's hosts only. Each user holds at most
// MAX_CLIENTS_PER_USER client connections at once, across all their hosts.
//
// Endpoints (WebSocket upgrades, all require `Authorization: Bearer
// <access key>`):
//   /v1/host/{host_id}               host control socket (+ X-Host-Token)
//   /v1/host/{host_id}/accept/{conn} host data socket for one client
//   /v1/client/{host_id}             client connection
//   /v1/pair/{slot}                  client connection to the host showing
//                                    a pairing code with this slot
//
// One more endpoint is a plain POST without the access key, because hosts
// that use other relays reach it too; see push.ts:
//   /v1/push                         forward a sealed push to the phone app

import { DurableObject } from "cloudflare:workers";

import { handlePush, type PushEnv } from "./push";

export { PushGateway } from "./push";

export interface Env extends PushEnv {
  /// The users and the SHA-256 of each one's access key, as a JSON object:
  /// `{"alice": "<64 lowercase hex digits>"}`. Only hashes are stored, so the
  /// secret itself holds nothing that opens a socket.
  ACCESS_KEYS?: string;
  HOST_ROOM: DurableObjectNamespace<HostRoom>;
  DIRECTORY: DurableObjectNamespace<PairingDirectory>;
  USER_QUOTA: DurableObjectNamespace<UserQuota>;
  VERSION_METADATA?: WorkerVersionMetadata;
}

/// Client connections one user may hold at once, over all of their hosts.
/// Every client connection makes the host open a data socket and run a
/// handshake, so this also bounds the work one key can cause.
const MAX_CLIENTS_PER_USER = 8;

/// A quota entry this young is never dropped as closed: its room accepts the
/// socket only after the quota answers, so a recount in between would find
/// no socket for it yet.
const QUOTA_GRACE_MS = 10_000;

/// Largest frame forwarded. Channel frames are at most 64 KiB of ciphertext.
const MAX_MESSAGE = 128 * 1024;

/// A client that sees no `open` within this long gets close code 4404.
const OPEN_TIMEOUT_MS = 15_000;

/// Connection attempts allowed per client IP per minute. Counted per
/// isolate, so this bounds abuse without a shared store.
const ATTEMPTS_PER_MINUTE = 30;

const HOST_ID = /^[0-9a-z]{16}$/;
const SLOT = /^[0-9A-Z]{3}$/;
const CONN = /^[0-9a-f]{16}$/;
const USER = /^[0-9a-z_-]{1,32}$/;
const KEY_HASH = /^[0-9a-f]{64}$/;

const attempts = new Map<string, { windowStart: number; count: number }>();

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    if (new URL(request.url).pathname === "/v1/push") {
      return handlePush(request, env);
    }

    const user = await userOf(request, env);

    if (user === null) {
      return new Response("unauthorized", { status: 401 });
    }

    if (request.headers.get("Upgrade") !== "websocket") {
      return new Response("expected a WebSocket upgrade", { status: 426 });
    }

    if (!admit(request.headers.get("CF-Connecting-IP") ?? "unknown")) {
      return new Response("too many attempts", { status: 429 });
    }

    const parts = new URL(request.url).pathname.split("/").filter(Boolean);

    if (parts[0] !== "v1") {
      return new Response("not found", { status: 404 });
    }

    // /v1/pair/{slot}: resolve the slot to its host, then join as a client.
    if (parts[1] === "pair" && parts.length === 3 && SLOT.test(parts[2])) {
      const hostId = await directory(env, user).resolve(parts[2]);

      if (hostId === null) {
        return new Response("no host is showing that code", { status: 404 });
      }

      return room(env, hostId).fetch(roomRequest(request, "client", hostId, user));
    }

    const hostId = parts[2];

    if (!HOST_ID.test(hostId ?? "")) {
      return new Response("not found", { status: 404 });
    }

    if (parts[1] === "client" && parts.length === 3) {
      return room(env, hostId).fetch(roomRequest(request, "client", hostId, user));
    }

    if (parts[1] === "host" && parts.length === 3) {
      return room(env, hostId).fetch(roomRequest(request, "control", hostId, user));
    }

    if (parts[1] === "host" && parts.length === 5 && parts[3] === "accept" && CONN.test(parts[4])) {
      return room(env, hostId).fetch(roomRequest(request, `accept:${parts[4]}`, hostId, user));
    }

    return new Response("not found", { status: 404 });
  },
};

/// The user whose access key the request presents, or null.
async function userOf(request: Request, env: Env): Promise<string | null> {
  const presented = request.headers.get("Authorization") ?? "";

  if (!presented.startsWith("Bearer ")) {
    return null;
  }

  const hash = await sha256Hex(presented.slice("Bearer ".length));
  let user: string | null = null;

  // Every entry is compared, so the time taken does not tell which matched.
  for (const [name, expected] of accessKeys(env)) {
    if (constantTimeEqual(hash, expected)) {
      user = name;
    }
  }

  return user;
}

let parsedKeys: { raw: string; users: [string, string][] } | undefined;

/// `ACCESS_KEYS`, parsed once per isolate and again only when it changes.
function accessKeys(env: Env): [string, string][] {
  const raw = env.ACCESS_KEYS ?? "";

  if (parsedKeys?.raw !== raw) {
    parsedKeys = { raw, users: parseAccessKeys(raw) };
  }

  return parsedKeys.users;
}

/// Any malformed entry rejects the whole list, and without a list the relay
/// admits nobody. A typo then locks every user out at once and gets noticed,
/// instead of quietly dropping one user's key.
function parseAccessKeys(raw: string): [string, string][] {
  let parsed: unknown;

  try {
    parsed = JSON.parse(raw);
  } catch {
    console.error("ACCESS_KEYS is not JSON");

    return [];
  }

  if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
    console.error("ACCESS_KEYS is not a JSON object");

    return [];
  }

  const users = Object.entries(parsed);

  for (const [name, hash] of users) {
    if (!USER.test(name) || typeof hash !== "string" || !KEY_HASH.test(hash)) {
      console.error(`ACCESS_KEYS has a malformed entry for "${name}"`);

      return [];
    }
  }

  return users as [string, string][];
}

function constantTimeEqual(a: string, b: string): boolean {
  if (a.length !== b.length) {
    return false;
  }

  let difference = 0;

  for (let i = 0; i < a.length; i++) {
    difference |= a.charCodeAt(i) ^ b.charCodeAt(i);
  }

  return difference === 0;
}

function admit(ip: string): boolean {
  const now = Date.now();
  const entry = attempts.get(ip);

  if (entry === undefined || now - entry.windowStart >= 60_000) {
    attempts.set(ip, { windowStart: now, count: 1 });

    return true;
  }

  entry.count += 1;

  return entry.count <= ATTEMPTS_PER_MINUTE;
}

function room(env: Env, hostId: string): DurableObjectStub<HostRoom> {
  return env.HOST_ROOM.get(env.HOST_ROOM.idFromName(hostId));
}

/// Pairing slots are kept per user: a slot resolves only to hosts of the
/// user asking, so one user cannot connect to another's pairing code and
/// spend the few attempts the host allows each code.
function directory(env: Env, user: string): DurableObjectStub<PairingDirectory> {
  return env.DIRECTORY.get(env.DIRECTORY.idFromName(`directory:${user}`));
}

function quota(env: Env, user: string): DurableObjectStub<UserQuota> {
  return env.USER_QUOTA.get(env.USER_QUOTA.idFromName(user));
}

function roomRequest(request: Request, kind: string, hostId: string, user: string): Request {
  const forwarded = new Request(request);

  forwarded.headers.set("X-Relay-Kind", kind);
  forwarded.headers.set("X-Relay-Host", hostId);
  forwarded.headers.set("X-Relay-User", user);

  return forwarded;
}

async function sha256Hex(text: string): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text));

  return [...new Uint8Array(digest)].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}

function randomConn(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(8));

  return [...bytes].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}

/// Refused sockets use the plain accept, not the hibernation API: a close
/// on a hibernatable socket whose upgrade has not completed never reaches
/// the client, which would then wait out its whole open timeout instead of
/// learning at once why it was refused.
function refuse(code: number, reason: string): Response {
  const [client, server] = Object.values(new WebSocketPair());

  server.accept();
  server.close(code, reason);

  return new Response(null, { status: 101, webSocket: client });
}

interface ClientAttachment {
  conn: string;
  user: string;
  opened: boolean;
  since: number;
}

interface QuotaEntry {
  roomId: string;
  since: number;
}

interface ControlAttachment {
  hostId: string;
  user: string;
  connection: string;
  since: number;
}

/// The sockets of one host: its control socket, its clients, and the data
/// socket it opens for each client. Uses the WebSocket Hibernation API, so an
/// idle room costs nothing while its sockets stay open.
export class HostRoom extends DurableObject<Env> {
  constructor(ctx: DurableObjectState, env: Env) {
    super(ctx, env);

    // Keep-alive pings are answered without waking the object.
    ctx.setWebSocketAutoResponse(new WebSocketRequestResponsePair('{"t":"ping"}', '{"t":"pong"}'));
  }

  async fetch(request: Request): Promise<Response> {
    const kind = request.headers.get("X-Relay-Kind") ?? "";
    const hostId = request.headers.get("X-Relay-Host") ?? "";
    const user = request.headers.get("X-Relay-User") ?? "";
    const owner = await this.ctx.storage.get<string>("owner");

    if (kind === "client") {
      return this.acceptClient(user, owner === user);
    }

    // The room belongs to the user whose key registered the host; until
    // then, only a control socket can claim it.
    const claimable = owner === undefined && kind === "control";

    if (owner !== user && !claimable) {
      return new Response("this host id belongs to another host", { status: 403 });
    }

    // Host sockets prove they belong to the host that registered first.
    if (!(await this.verifyHost(request.headers.get("X-Host-Token") ?? ""))) {
      return new Response("this host id belongs to another host", { status: 403 });
    }

    if (owner === undefined) {
      await this.ctx.storage.put("owner", user);
    }

    if (kind === "control") {
      return this.acceptControl(hostId, user);
    }

    if (kind.startsWith("accept:")) {
      return this.acceptData(kind.slice("accept:".length));
    }

    return new Response("not found", { status: 404 });
  }

  /// Trust on first use: the first control connection for a host id stores
  /// a hash of its token, and later host sockets must present the same one.
  /// A host id is only disclosed through pairing, which happens after the
  /// host has registered, so squatting would need the id beforehand.
  private async verifyHost(token: string): Promise<boolean> {
    if (token.length < 32) {
      return false;
    }

    const hash = await sha256Hex(token);
    const stored = await this.ctx.storage.get<string>("tokenHash");

    if (stored === undefined) {
      await this.ctx.storage.put("tokenHash", hash);

      return true;
    }

    return constantTimeEqual(stored, hash);
  }

  private acceptControl(hostId: string, user: string): Response {
    // A reconnecting host replaces its previous control socket.
    for (const old of this.ctx.getWebSockets("control")) {
      this.logSocket(old, "close_requested", { code: 1000, reason: "replaced", origin: "relay" });
      old.close(1000, "replaced");
    }

    const [client, server] = Object.values(new WebSocketPair());

    this.ctx.acceptWebSocket(server, ["control"]);
    const connection = randomConn();
    server.serializeAttachment({ hostId, user, connection, since: Date.now() } satisfies ControlAttachment);
    this.logSocket(server, "control_connected");

    return new Response(null, { status: 101, webSocket: client, headers: { "X-Relay-Connection": connection } });
  }

  /// A host of another user is refused exactly like an offline one, so a
  /// key does not reveal which host ids other users have registered.
  private async acceptClient(user: string, owned: boolean): Promise<Response> {
    if (!owned || this.ctx.getWebSockets("control").length === 0) {
      return refuse(4404, "host offline");
    }

    const conn = randomConn();

    if (!(await quota(this.env, user).acquire(conn, this.ctx.id.toString()))) {
      return refuse(4429, "too many clients");
    }

    // The control socket may have gone while the quota answered.
    const control = this.ctx.getWebSockets("control")[0];

    if (control === undefined) {
      await quota(this.env, user).release(conn);

      return refuse(4404, "host offline");
    }

    const [client, server] = Object.values(new WebSocketPair());

    this.ctx.acceptWebSocket(server, ["client", `c:${conn}`]);
    server.serializeAttachment({ conn, user, opened: false, since: Date.now() } satisfies ClientAttachment);
    this.logSocket(server, "client_connected");

    control.send(JSON.stringify({ t: "conn", id: conn }));

    // The client starts its handshake only after `open`, so nothing needs
    // buffering here; one that never gets `open` is closed.
    this.ctx.storage.setAlarm(Date.now() + OPEN_TIMEOUT_MS);

    return new Response(null, { status: 101, webSocket: client, headers: { "X-Relay-Connection": conn } });
  }

  private acceptData(conn: string): Response {
    const client = this.ctx.getWebSockets(`c:${conn}`)[0];

    if (client === undefined) {
      return new Response("that client is gone", { status: 404 });
    }

    const [pair0, server] = Object.values(new WebSocketPair());

    this.ctx.acceptWebSocket(server, [`h:${conn}`]);

    const attachment = client.deserializeAttachment() as ClientAttachment;

    const openedAttachment = { ...attachment, opened: true };
    client.serializeAttachment(openedAttachment);
    server.serializeAttachment(openedAttachment);
    client.send(JSON.stringify({ t: "open", id: conn }));
    this.logSocket(client, "host_accepted");

    return new Response(null, { status: 101, webSocket: pair0 });
  }

  async webSocketMessage(ws: WebSocket, message: string | ArrayBuffer): Promise<void> {
    const tags = this.ctx.getTags(ws);

    if (tags.includes("control")) {
      if (typeof message === "string") {
        await this.onControl(ws, message);
      }

      return;
    }

    if (typeof message !== "string" && message.byteLength > MAX_MESSAGE) {
      this.logSocket(ws, "close_requested", { code: 1009, reason: "message too large", origin: "relay" });
      ws.close(1009, "message too large");

      return;
    }

    const peer = this.peerOf(tags);

    if (peer !== undefined) {
      try {
        peer.send(message);
      } catch (error) {
        this.logSocket(ws, "forward_failed", { error: String(error), origin: "send" });
        throw error;
      }
    }
  }

  async webSocketClose(ws: WebSocket, code: number, reason: string, wasClean: boolean): Promise<void> {
    this.logSocket(ws, "close_received", { code, reason, was_clean: wasClean, origin: "peer" });
    this.closePeer(ws, code, reason);
    await this.releaseClient(ws);
  }

  async webSocketError(ws: WebSocket, error: unknown): Promise<void> {
    this.logSocket(ws, "socket_error", { error: String(error), origin: "transport" });
    this.closePeer(ws, 1011, "peer error");
    await this.releaseClient(ws);
  }

  /// The conn ids of this room's open client sockets. A user's quota asks
  /// for them to drop entries whose close it never heard about.
  liveClients(): string[] {
    return this.ctx
      .getWebSockets("client")
      .filter((ws) => ws.readyState === WebSocket.READY_STATE_OPEN)
      .map((ws) => (ws.deserializeAttachment() as ClientAttachment).conn);
  }

  private async releaseClient(ws: WebSocket): Promise<void> {
    if (!this.ctx.getTags(ws).includes("client")) {
      return;
    }

    const { conn, user } = ws.deserializeAttachment() as ClientAttachment;

    await quota(this.env, user).release(conn);
  }

  async alarm(): Promise<void> {
    const now = Date.now();
    let waiting = false;

    for (const client of this.ctx.getWebSockets("client")) {
      const attachment = client.deserializeAttachment() as ClientAttachment;

      if (attachment.opened) {
        continue;
      }

      if (now - attachment.since >= OPEN_TIMEOUT_MS) {
        this.logSocket(client, "close_requested", { code: 4404, reason: "host not answering", origin: "relay" });
        client.close(4404, "host not answering");
      } else {
        waiting = true;
      }
    }

    if (waiting) {
      await this.ctx.storage.setAlarm(now + 1_000);
    }
  }

  /// Pairing slots are claimed and released over the control socket.
  private async onControl(ws: WebSocket, message: string): Promise<void> {
    if (message === "ping") {
      ws.send(JSON.stringify({ t: "pong" }));
      return;
    }
    let request: { t?: string; slot?: string; ttl?: number };

    try {
      request = JSON.parse(message);
    } catch {
      return;
    }

    const slot = request.slot ?? "";

    if (!SLOT.test(slot)) {
      return;
    }

    const { hostId, user } = ws.deserializeAttachment() as ControlAttachment;
    const slots = directory(this.env, user);

    if (request.t === "slot") {
      const ttl = Math.min(Math.max(request.ttl ?? 300, 1), 600);
      const claimed = await slots.claim(slot, hostId, ttl);

      ws.send(JSON.stringify({ t: claimed ? "slot_ok" : "slot_taken", slot }));
    } else if (request.t === "slot_release") {
      await slots.release(slot, hostId);
    }
  }

  private peerOf(tags: string[]): WebSocket | undefined {
    for (const tag of tags) {
      if (tag.startsWith("c:")) {
        return this.ctx.getWebSockets(`h:${tag.slice(2)}`)[0];
      }

      if (tag.startsWith("h:")) {
        return this.ctx.getWebSockets(`c:${tag.slice(2)}`)[0];
      }
    }

    return undefined;
  }

  /// Closing either side of a client connection closes the other.
  private closePeer(ws: WebSocket, code: number, reason: string): void {
    const peer = this.peerOf(this.ctx.getTags(ws));
    const forwarded = code === 1005 || code === 1006 ? 1000 : code;

    try {
      this.logSocket(ws, "close_forwarded", { original_code: code, forwarded_code: forwarded, reason, peer_present: peer !== undefined });
      peer?.close(forwarded, reason);
    } catch (error) {
      this.logSocket(ws, "close_forward_failed", { error: String(error) });
    }
  }

  private logSocket(ws: WebSocket, event: string, details: Record<string, unknown> = {}): void {
    const tags = this.ctx.getTags(ws);
    const tag = tags.find((tag) => tag.startsWith("c:") || tag.startsWith("h:"));
    const attachment = ws.deserializeAttachment() as ClientAttachment | ControlAttachment | null;

    console.log(JSON.stringify({
      event,
      room: this.ctx.id.toString(),
      relay_version: this.env.VERSION_METADATA?.id ?? "local",
      relay_connection: tag?.slice(2) ?? (attachment !== null && "connection" in attachment ? attachment.connection : undefined),
      side: tags.includes("control") ? "control" : tag?.startsWith("c:") ? "client" : "host",
      elapsed_ms: attachment !== null && "since" in attachment ? Date.now() - attachment.since : undefined,
      ...details,
    }));
  }
}

/// Maps pairing slots to the host showing the code, until the code expires.
export class PairingDirectory extends DurableObject<Env> {
  async claim(slot: string, hostId: string, ttlSeconds: number): Promise<boolean> {
    const now = Date.now();
    const existing = await this.ctx.storage.get<{ hostId: string; expires: number }>(slot);

    if (existing !== undefined && existing.expires > now && existing.hostId !== hostId) {
      return false;
    }

    await this.ctx.storage.put(slot, { hostId, expires: now + ttlSeconds * 1000 });

    return true;
  }

  async release(slot: string, hostId: string): Promise<void> {
    const existing = await this.ctx.storage.get<{ hostId: string; expires: number }>(slot);

    if (existing?.hostId === hostId) {
      await this.ctx.storage.delete(slot);
    }
  }

  async resolve(slot: string): Promise<string | null> {
    const existing = await this.ctx.storage.get<{ hostId: string; expires: number }>(slot);

    if (existing === undefined || existing.expires <= Date.now()) {
      return null;
    }

    return existing.hostId;
  }
}

/// Counts one user's client connections across all of their hosts. Each
/// entry maps a conn id to the room holding it and when it was taken.
///
/// A room reports each close, but a close can go unreported: a deploy drops
/// every socket, and a socket the relay itself closes may never see its
/// close handler run. So when the count is full, the quota asks each room
/// which of its entries are still open and drops the rest before refusing.
export class UserQuota extends DurableObject<Env> {
  async acquire(conn: string, roomId: string): Promise<boolean> {
    if ((await this.ctx.storage.list()).size >= MAX_CLIENTS_PER_USER) {
      await this.dropClosed();
    }

    // Only storage calls sit between the count and the put, and the input
    // gate holds other calls back while those run, so two calls that both
    // waited on the recount cannot both take the last place.
    if ((await this.ctx.storage.list()).size >= MAX_CLIENTS_PER_USER) {
      return false;
    }

    await this.ctx.storage.put(conn, { roomId, since: Date.now() } satisfies QuotaEntry);

    return true;
  }

  async release(conn: string): Promise<void> {
    await this.ctx.storage.delete(conn);
  }

  private async dropClosed(): Promise<void> {
    const byRoom = new Map<string, string[]>();
    const settled = Date.now() - QUOTA_GRACE_MS;

    for (const [conn, entry] of await this.ctx.storage.list<QuotaEntry>()) {
      if (entry.since <= settled) {
        byRoom.set(entry.roomId, [...(byRoom.get(entry.roomId) ?? []), conn]);
      }
    }

    const rooms = [...byRoom.entries()];
    const answers = await Promise.allSettled(
      rooms.map(([roomId]) =>
        this.env.HOST_ROOM.get(this.env.HOST_ROOM.idFromString(roomId)).liveClients(),
      ),
    );
    const closed: string[] = [];

    // A room that fails to answer keeps its entries; refusing a client is
    // better than letting a user exceed the limit.
    answers.forEach((answer, index) => {
      if (answer.status === "fulfilled") {
        const live = new Set(answer.value);

        closed.push(...rooms[index][1].filter((conn) => !live.has(conn)));
      }
    });

    if (closed.length > 0) {
      await this.ctx.storage.delete(closed);
    }
  }
}
