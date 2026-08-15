// Probe whether one Claude Code child agent can still be stopped on its own.
//
// The CLI accepts a `stop_task` control request naming a task id, which is what
// the Background Tasks row's Stop control sends. Two things have to hold, and
// both belong to the CLI rather than to this repo, so they are worth
// re-checking whenever Claude Code is upgraded:
//
//   1. a `task_started` frame names the child with both a `task_id` and the
//      parent's `tool_use_id`, which is what lets a row keyed by either one
//      resolve to the id the task registry knows, and
//   2. `stop_task` ends that child alone, leaving the parent's turn running.
//
// The CLI answers a task it cannot find, or one already settled, as a success,
// so the control response alone proves nothing. The `killed` status patch and
// the `stopped` notification are the actual evidence.
//
// Usage:  node tests/claude_stop_probe.mjs

import { spawn } from "node:child_process";
import readline from "node:readline";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const child = spawn(
  "claude",
  [
    "-p",
    "--output-format", "stream-json",
    "--input-format", "stream-json",
    "--verbose",
    "--include-partial-messages",
    "--allow-dangerously-skip-permissions",
  ],
  // A scratch cwd: the probe is about the control protocol, and an agent loose
  // in the repo is not part of the question.
  {
    stdio: ["pipe", "pipe", "pipe"],
    shell: true,
    cwd: mkdtempSync(join(tmpdir(), "nmt-probe-")),
  },
);

const stream = [];
const controls = new Map();
let nextControl = 1;

readline.createInterface({ input: child.stderr }).on("line", (l) =>
  console.log(`stderr: ${l.slice(0, 200)}`),
);

readline.createInterface({ input: child.stdout }).on("line", (line) => {
  let msg;
  try {
    msg = JSON.parse(line);
  } catch {
    return;
  }
  stream.push({ at: Date.now(), msg });
  if (msg.type === "control_response") {
    const id = msg.response?.request_id ?? msg.request_id;
    if (controls.has(id)) {
      controls.get(id)(msg);
      controls.delete(id);
    }
  }
});

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const write = (o) => child.stdin.write(JSON.stringify(o) + "\n");

function control(request, timeoutMs = 30000) {
  const request_id = `probe-${nextControl++}`;
  const done = new Promise((r) => controls.set(request_id, r));
  write({ type: "control_request", request_id, request });
  return Promise.race([
    done,
    new Promise((r) => setTimeout(() => r({ timeout: true }), timeoutMs)),
  ]);
}

const frames = (subtype) =>
  stream.filter((e) => e.msg.type === "system" && e.msg.subtype === subtype);

// A child that runs long enough to be caught mid-flight. Counting finishes in
// one burst, which would make a stop indistinguishable from normal completion.
write({
  type: "user",
  message: {
    role: "user",
    content: [
      {
        type: "text",
        text:
          "Use the Agent tool with run_in_background: true to launch ONE " +
          "background subagent. Its task: run `sleep 30` with Bash, then " +
          "report the word done, and repeat that whole cycle 10 times. " +
          "Launch it and then immediately tell me you launched it. Do not " +
          "wait for it and do not do the work yourself.",
      },
    ],
  },
});

console.log("waiting for a task_started frame...");
let launch;
for (let i = 0; i < 150 && !launch; i++) {
  await sleep(1000);
  launch = frames("task_started").find(
    (e) => e.msg.task_type === "local_agent",
  )?.msg;
}
if (!launch) {
  console.log("no local_agent task ever started");
  child.kill();
  process.exit(1);
}

console.log(`\ntask_started: ${JSON.stringify(launch).slice(0, 300)}`);
console.log(`  task_id:     ${launch.task_id}`);
console.log(`  tool_use_id: ${launch.tool_use_id}   (both name one child)`);

await sleep(8000);
const cut = Date.now();
const reply = await control({ subtype: "stop_task", task_id: launch.task_id });
console.log(
  `\nstop_task -> ${reply.timeout ? "TIMEOUT" : JSON.stringify(reply.response ?? reply)}`,
);

await sleep(15000);

const killed = frames("task_updated").find(
  (e) => e.at >= cut && e.msg.task_id === launch.task_id,
)?.msg;
const stopped = frames("task_notification").find(
  (e) => e.at >= cut && e.msg.task_id === launch.task_id,
)?.msg;
console.log(`\ntask_updated patch:    ${JSON.stringify(killed?.patch)}   (expected status killed)`);
console.log(`task_notification:     ${stopped?.status}   (expected stopped)`);

// The parent surviving is the other half of the claim: it keeps streaming and
// settles its own turn normally.
const result = stream.find((e) => e.at >= cut && e.msg.type === "result")?.msg;
console.log(
  `parent turn settled:   is_error=${result?.is_error} stop_reason=${result?.stop_reason}`,
);

child.kill();
process.exit(0);
