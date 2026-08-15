// Probe whether one Codex child agent can still be stopped on its own.
//
// A child agent is a descendant thread, but the only interrupt the app-server
// offers is `turn/interrupt {threadId, turnId}`. Two things have to hold for the
// Background Tasks row's Stop control to work, and both are properties of the
// app-server rather than of this repo, so they are worth re-checking whenever
// Codex is upgraded:
//
//   1. a descendant thread is addressable, not just the threads this client
//      started itself, and
//   2. interrupting the child ends the child's turn alone, leaving the parent's
//      running.
//
// Stage 1 costs nothing: it interrupts threads in known states and reads the
// error shapes. Stage 2 (--live) spends a real turn, spawning a subagent and
// interrupting it mid-output.
//
// Usage:  node tests/codex_stop_probe.mjs [--live]

import { spawn } from "node:child_process";
import readline from "node:readline";
import { randomUUID } from "node:crypto";

const LIVE = process.argv.includes("--live");

const child = spawn("codex", ["app-server"], {
  stdio: ["pipe", "pipe", "pipe"],
  shell: true,
});

let nextId = 1;
const pending = new Map();
const stream = [];

readline.createInterface({ input: child.stdout }).on("line", (line) => {
  let msg;
  try {
    msg = JSON.parse(line);
  } catch {
    return;
  }
  if (msg.id !== undefined && pending.has(msg.id)) {
    pending.get(msg.id)(msg);
    pending.delete(msg.id);
  } else if (msg.method) {
    stream.push({ at: Date.now(), ...msg });
  }
});

function send(method, params, timeoutMs = 240000) {
  const id = nextId++;
  const done = new Promise((resolve) => pending.set(id, resolve));
  child.stdin.write(
    JSON.stringify({ jsonrpc: "2.0", id, method, params }) + "\n",
  );
  return Promise.race([
    done,
    new Promise((r) => setTimeout(() => r({ timeout: true }), timeoutMs)),
  ]);
}

const notify = (method, params) =>
  child.stdin.write(JSON.stringify({ jsonrpc: "2.0", method, params }) + "\n");
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function show(label, reply) {
  const body = reply.timeout
    ? "TIMEOUT"
    : reply.error
      ? `error ${JSON.stringify(reply.error)}`
      : `ok ${JSON.stringify(reply.result).slice(0, 300)}`;
  console.log(`\n=== ${label}\n${body}`);
}

await send("initialize", {
  clientInfo: { name: "nmt-probe", version: "0.0.0" },
  capabilities: { experimentalApi: true },
});
notify("initialized", {});

const started = await send("thread/start", {});
const parent = started.result.thread.id;
console.log(`parent thread: ${parent}`);

// Expected: "thread not found". A well-formed id the server never loaded is
// refused, which is what makes stage 2 meaningful rather than tautological.
show(
  "turn/interrupt - unknown but valid uuid",
  await send("turn/interrupt", { threadId: randomUUID(), turnId: randomUUID() }),
);

// Expected: "no active turn to interrupt". The thread resolves; the turn does not.
show(
  "turn/interrupt - own idle thread",
  await send("turn/interrupt", { threadId: parent, turnId: randomUUID() }),
);

// Expected: "thread not found". Persisted on disk is not the same as loaded, so
// the lookup is against the live registry a descendant would have to be in.
const listed = await send("thread/list", { pageSize: 5 });
const persisted = (listed.result?.threads ?? listed.result?.data ?? []).find(
  (t) => (t.id ?? t.threadId) !== parent,
);
const persistedId = persisted?.id ?? persisted?.threadId;
if (persistedId) {
  show(
    "turn/interrupt - persisted, not loaded",
    await send("turn/interrupt", {
      threadId: persistedId,
      turnId: randomUUID(),
    }),
  );
}

if (!LIVE) {
  console.log("\n(pass --live to spawn a real subagent and interrupt it)");
  child.kill();
  process.exit(0);
}

console.log("\n--- live stage: asking for a subagent");
// Read-only sandbox and no approvals: the probe only needs the child to exist
// and keep running, and an agent loose in a repo is not part of the question.
const turn = send("turn/start", {
  threadId: parent,
  approvalPolicy: "never",
  sandboxPolicy: { type: "readOnly" },
  input: [
    {
      type: "text",
      text:
        "Explicitly delegate to a subagent. Spawn exactly one child agent " +
        "whose only task is to write out the numbers 1 to 3000, one per " +
        "line, slowly and without using any tools. Then wait for it. Do not " +
        "do the counting yourself.",
    },
  ],
});

// The child's identity and its active turn both arrive as a `turn/started` on a
// thread that is not the parent. The turn id lives at `turn.id`; there is no
// top-level `turnId`, which the raw dump below makes checkable after upgrades.
let descendant, descendantTurn;
for (let i = 0; i < 150 && !descendantTurn; i++) {
  await sleep(1000);
  const hit = stream.find(
    (m) =>
      m.method === "turn/started" &&
      m.params?.threadId &&
      m.params.threadId !== parent,
  );
  if (hit) {
    console.log(`\nraw turn/started: ${JSON.stringify(hit.params)}`);
    descendant = hit.params.threadId;
    descendantTurn = hit.params.turn?.id;
  }
}
console.log(`descendant thread: ${descendant}`);
console.log(`descendant turn:   ${descendantTurn}`);
if (!descendantTurn) {
  console.log("no descendant turn was ever announced");
  child.kill();
  process.exit(1);
}

// Let the child produce output first, so a stop is distinguishable from it
// never having started.
await sleep(6000);
const before = stream.filter(
  (m) =>
    m.method === "item/agentMessage/delta" && m.params?.threadId === descendant,
).length;

const cut = Date.now();
show(
  "turn/interrupt - LIVE descendant",
  await send("turn/interrupt", {
    threadId: descendant,
    turnId: descendantTurn,
  }),
);

await sleep(12000);
const after = stream.filter(
  (m) =>
    m.method === "item/agentMessage/delta" &&
    m.params?.threadId === descendant &&
    m.at > cut + 2000,
).length;

console.log(`\nchild deltas before interrupt:      ${before}`);
console.log(`child deltas >2s after interrupt:   ${after}  (expected 0)`);

console.log("\n--- stream after the interrupt");
for (const m of stream.filter((m) => m.at >= cut)) {
  if (m.method === "item/agentMessage/delta") continue;
  const who = m.params?.threadId === descendant ? "CHILD " : "parent";
  const extra = m.params?.status ? ` status=${JSON.stringify(m.params.status)}` : "";
  console.log(`${who} ${m.method}${extra}`);
}

// The parent surviving is the other half of the claim. Its error names the turn
// id it is actually running, which is only possible while that turn is live.
const parentReply = await send("turn/interrupt", {
  threadId: parent,
  turnId: "not-the-active-one",
});
console.log(
  `\nparent still running? ${JSON.stringify(parentReply.error ?? parentReply.result)}`,
);

void turn;
child.kill();
process.exit(0);
