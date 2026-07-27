// NiumaTerm relay: an untrusted byte forwarder between one host and its
// clients. End-to-end encryption (Noise) happens between host and client, so
// this code only ever sees ciphertext plus routing metadata.
//
// Connection model (three socket kinds, all under one DO per host_id):
//   - host control socket  (role=host, no connection_id): one per host; carries
//     JSON notifications {connected|disconnected|sync} so the host knows which
//     per-client data sockets to open.
//   - host data socket     (role=host&connection_id=cid): one per client.
//   - client socket        (role=client[&connection_id=cid]): many.
// Keeping one host data socket per client (instead of multiplexing on the
// control socket) is what lets the relay forward frames without parsing them.

export interface Env {
  RELAY: DurableObjectNamespace;
  ACCESS_TOKEN: string;
}

// Close codes surfaced to peers. 1012 (service restart) marks retryable
// conditions; 4xxx are terminal for the current attempt.
const CLOSE_BAD_REQUEST = 4400;
const CLOSE_UNAUTHORIZED = 4401;
const CLOSE_HOST_OFFLINE = 4404;
const CLOSE_BUFFER_OVERFLOW = 4429;
const CLOSE_RETRY = 1012;

// Frames a client may send before the host's data socket attaches. The window
// is one control-message round trip, so a small cap suffices; overflow closes
// the client, whose reconnect restarts cleanly.
const MAX_BUFFERED_FRAMES = 200;

type Attachment =
  | { kind: "control" }
  | { kind: "client"; cid: string }
  | { kind: "hostData"; cid: string };

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);
    if (url.pathname !== "/ws") {
      return new Response("not found", { status: 404 });
    }
    if (request.headers.get("Upgrade")?.toLowerCase() !== "websocket") {
      return new Response("expected websocket", { status: 426 });
    }
    const hostId = url.searchParams.get("host_id");
    const role = url.searchParams.get("role");
    if (!hostId || (role !== "host" && role !== "client")) {
      return new Response("missing host_id or invalid role", { status: 400 });
    }
    // Token is checked here (before the DO) so unauthorized host sockets never
    // touch instance state. Clients carry no token: pairing + Noise handshake
    // are their gate, and requiring one would put a shared secret in every
    // client config for no confidentiality gain.
    if (role === "host" && !(await tokenValid(request, env))) {
      return new Response("unauthorized", { status: 401 });
    }
    const id = env.RELAY.idFromName(hostId);
    return env.RELAY.get(id).fetch(request);
  },
};

async function tokenValid(request: Request, env: Env): Promise<boolean> {
  const auth = request.headers.get("Authorization") ?? "";
  const token = auth.startsWith("Bearer ") ? auth.slice(7) : "";
  if (!env.ACCESS_TOKEN || !token) return false;
  const enc = new TextEncoder();
  const a = enc.encode(token);
  const b = enc.encode(env.ACCESS_TOKEN);
  if (a.byteLength !== b.byteLength) return false;
  return crypto.subtle.timingSafeEqual(a, b);
}

export class RelayDurableObject implements DurableObject {
  private readonly ctx: DurableObjectState;
  // Client frames awaiting a host data socket, keyed by connection_id.
  // Deliberately in-memory only: hibernation may drop it, and the client-side
  // reconnect covers that rare loss (see design.md open questions).
  private buffers = new Map<string, (ArrayBuffer | string)[]>();

  constructor(ctx: DurableObjectState) {
    this.ctx = ctx;
  }

  async fetch(request: Request): Promise<Response> {
    const url = new URL(request.url);
    const role = url.searchParams.get("role");
    const cid = url.searchParams.get("connection_id");

    const pair = new WebSocketPair();
    const [clientEnd, serverEnd] = [pair[0], pair[1]];

    if (role === "host" && !cid) {
      this.acceptControl(serverEnd);
    } else if (role === "host") {
      this.acceptHostData(serverEnd, cid!);
    } else {
      const assigned = cid ?? `conn_${crypto.randomUUID()}`;
      const status = this.acceptClient(serverEnd, assigned);
      if (status !== null) {
        return new Response(status.reason, { status: status.http });
      }
    }
    return new Response(null, { status: 101, webSocket: clientEnd });
  }

  private controlSocket(): WebSocket | undefined {
    return this.ctx.getWebSockets("control")[0];
  }

  private clientSocket(cid: string): WebSocket | undefined {
    return this.ctx.getWebSockets(`client:${cid}`)[0];
  }

  private hostDataSocket(cid: string): WebSocket | undefined {
    return this.ctx.getWebSockets(`hostData:${cid}`)[0];
  }

  private accept(ws: WebSocket, attachment: Attachment, tags: string[]) {
    // Hibernation API: the runtime keeps sockets alive while the instance
    // sleeps; tags + serialized attachments are how routing state survives.
    this.ctx.acceptWebSocket(ws, tags);
    ws.serializeAttachment(attachment);
  }

  private acceptControl(ws: WebSocket) {
    // One control socket per host: a re-register (host restart, network flap
    // recovery) replaces the previous socket rather than coexisting with it.
    this.controlSocket()?.close(CLOSE_RETRY, "replaced by new registration");
    this.accept(ws, { kind: "control" }, ["control"]);
    // Reconnect reconciliation: tell the (possibly restarted) host which
    // clients are currently online so it can open matching data sockets.
    const connections = this.ctx
      .getWebSockets()
      .map((s) => s.deserializeAttachment() as Attachment)
      .filter((a) => a.kind === "client")
      .map((a) => (a as { cid: string }).cid);
    ws.send(JSON.stringify({ type: "sync", connections }));
  }

  private acceptClient(
    ws: WebSocket,
    cid: string
  ): { http: number; reason: string } | null {
    const control = this.controlSocket();
    if (!control) {
      return { http: 404, reason: "host offline" };
    }
    // A duplicate cid would cross-wire two clients' ciphertext streams.
    if (this.clientSocket(cid)) {
      return { http: 409, reason: "connection_id already in use" };
    }
    this.accept(ws, { kind: "client", cid }, [`client:${cid}`]);
    control.send(JSON.stringify({ type: "connected", connectionId: cid }));
    return null;
  }

  private acceptHostData(ws: WebSocket, cid: string) {
    if (!this.clientSocket(cid)) {
      // The client vanished between `connected` and the host dialing back.
      this.accept(ws, { kind: "hostData", cid }, [`hostData:${cid}`]);
      ws.close(CLOSE_HOST_OFFLINE, "client gone");
      return;
    }
    this.hostDataSocket(cid)?.close(CLOSE_RETRY, "replaced");
    this.accept(ws, { kind: "hostData", cid }, [`hostData:${cid}`]);
    const buffered = this.buffers.get(cid);
    if (buffered) {
      for (const frame of buffered) ws.send(frame);
      this.buffers.delete(cid);
    }
  }

  webSocketMessage(ws: WebSocket, message: ArrayBuffer | string) {
    const attachment = ws.deserializeAttachment() as Attachment;
    switch (attachment.kind) {
      case "control":
        // The control socket carries relay→host notifications only; inbound
        // application data has no meaning here. Ignoring (rather than
        // closing) tolerates future host-side keepalive chatter.
        return;
      case "client": {
        const hostData = this.hostDataSocket(attachment.cid);
        if (hostData) {
          hostData.send(message);
          return;
        }
        const buffer = this.buffers.get(attachment.cid) ?? [];
        buffer.push(message);
        if (buffer.length > MAX_BUFFERED_FRAMES) {
          this.buffers.delete(attachment.cid);
          ws.close(CLOSE_BUFFER_OVERFLOW, "buffer overflow before host attach");
          return;
        }
        this.buffers.set(attachment.cid, buffer);
        return;
      }
      case "hostData": {
        this.clientSocket(attachment.cid)?.send(message);
        return;
      }
    }
  }

  webSocketClose(ws: WebSocket) {
    const attachment = ws.deserializeAttachment() as Attachment;
    switch (attachment.kind) {
      case "control": {
        // Host gone: cascade-close every client (retryable code so they poll
        // for the host's return) and drop all pending buffers.
        for (const socket of this.ctx.getWebSockets()) {
          if (socket !== ws) socket.close(CLOSE_RETRY, "host disconnected");
        }
        this.buffers.clear();
        return;
      }
      case "client": {
        this.buffers.delete(attachment.cid);
        this.hostDataSocket(attachment.cid)?.close(CLOSE_RETRY, "client disconnected");
        this.controlSocket()?.send(
          JSON.stringify({ type: "disconnected", connectionId: attachment.cid })
        );
        return;
      }
      case "hostData": {
        // Data path died while control lives: force the client into a clean
        // reconnect instead of leaving a half-open pairing.
        this.clientSocket(attachment.cid)?.close(CLOSE_RETRY, "host data socket lost");
        return;
      }
    }
  }

  webSocketError(ws: WebSocket) {
    this.webSocketClose(ws);
  }
}
