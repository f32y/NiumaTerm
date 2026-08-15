# DeepSeek Harness Integration Research

| Field | Value |
| --- | --- |
| Status | Research; Phase 0 measured on Windows (section 11) |
| Date | 2026-08-14 |
| Scope | Adding DeepSeek Harness (`dsh`) as a third Agent Tab harness |
| Companion | [`docs/agent-harness-integration-requirements.md`](../agent-harness-integration-requirements.md) |

Paths written as `packages/...`, `apps/...`, or `docs/...` without a `crates/`
prefix refer to the DeepSeek Harness repository, not to NiumaTerm. NiumaTerm
paths always start with `crates/`.

## 1. Recommendation

`dsh` exposes three machine-drivable interfaces, not one. They differ enormously
in fidelity, and the choice between them is the whole design decision.

- **Start on ACP.** It satisfies the requirements document's §7 minimum
  integration scope exactly, needs no DeepSeek-side code, reuses the existing
  newline-delimited JSON transport, and generalizes to every other ACP agent.
  Name the harness identity after the protocol (`Acp`), not the vendor.
- **Treat the Web `/api` interface as the upgrade path** if the full Agent Tab
  experience is wanted for DeepSeek specifically. It carries everything the
  requirements list, but over HTTP plus two WebSockets rather than stdio, so it
  is a different adapter rather than an extension of the ACP one.
- **Do not use the SDK stdio JSON-RPC interface.** It streams richer data than
  ACP but has no way to cancel a turn, which fails HR-002 outright (section 3).

A Rust ACP library already exists: `agent-client-protocol` 2.0.0 on crates.io
(Apache-2.0, `rust-version = 1.88`, below this workspace's 1.96). Optional
features include `unstable_end_turn_token_usage` and `unstable_elicitation`,
corresponding to HR-010 usage reporting and HR-011 structured questions.

## 2. The three machine interfaces

| Interface | Transport | Executable | Fidelity |
| --- | --- | --- | --- |
| Web `/api` | HTTP POST plus two WebSocket downlinks | `dsh web` (in the published `@deepseek-ai/dsh`) | Full: streaming deltas, reasoning, tool render views with diffs and exit codes, approvals, questions, usage projections, resume, fork, models, commands, skills, subagents |
| SDK JSON-RPC | Newline-delimited JSON-RPC 2.0 over stdio | `dsh-jsonrpc-agent` (`@deepseek-ai/dsh-sdk-jsonrpc-demo`) | High event fidelity, but only three client methods and no cancellation |
| ACP | JSON-RPC over stdio | `dsh-acp-demo` (`@deepseek-ai/dsh-acp-demo`) | Very low: committed assistant text and one-shot tool approvals only |

A fourth mode, `dsh --profile headless "task"`, prints the last assistant message
and exits; its own documentation lists "one submitted task only, no interactive
follow-up surface" as a known limitation, so it is not an option.

All three are published to npm. `@deepseek-ai/dsh` is at `0.1.0-rc.6`, and the
two demo servers publish the same version under the `next` tag, so none of them
requires building the repository.

`dsh` needs Node 22 or newer and is not a self-contained executable. Windows is
supported: the tree carries `packages/sandbox/sandbox-windows-acl` and Windows
branches in the credential and filesystem packages. There is no prebuilt Windows
binary of any kind; the only single-file build in the repository targets Linux
and macOS for a Python SDK.

### How `dsh` is launched

`dsh` is a profile loader, not a fixed application. `apps/cli/src/args.ts` parses
only launcher flags — `--profile <name>`, repeatable `--patch <path>`, and the
config dumps — and hands every later argument to the booted plugin tree. A
profile is a directory under `$DSH_HOME/profiles/<name>/` holding its own
`package.json` (whose `dsh.profile.bundles` names the bundle layers) and a
`cordis.patch.yml` overlay. `dsh plugin --profile <name> add <package>` forwards
to pnpm inside that directory, so profiles can install ordinary npm packages.

Only `web` and `headless` profiles auto-initialize. The ACP and SDK servers are
separate published packages with their own executables, and both **require** an
explicit `cordis.yml` path and exit 1 without one.

Two consequences for NiumaTerm:

- A launch command is a Node package invocation with a required config path, not
  a bare executable. `LaunchConfig` (`crates/agent_utils/src/lib.rs:47`) carries
  only `executable`, `model`, `effort`, `provider`, and `env`, with no
  free-form argument list, so it needs one more field.
- Because profiles install ordinary npm packages, NiumaTerm could supply its own
  Cordis plugin through its own profile without forking `dsh`. Section 5
  explains why that is now a fallback rather than the plan.

## 3. Why not the SDK stdio interface

On paper it looks ideal: newline-delimited JSON-RPC over stdio, which drops
straight into the existing `JsonLineProcess`, and a `session.event` notification
carrying the complete unfiltered session event stream — every token delta,
reasoning delta, tool call, tool result, and usage record.

The problem is the request side. `HarnessSdkRequestMap`
(`packages/sdk/protocol/src/types.ts:101`) has exactly three methods:
`initialize`, `session/prompt`, and `shutdown`. Its own documentation states
"no cancel or session-close methods — a client abandons a turn by closing the
runtime process", and "server→client requests are dead capability — the
transport supports them, but the server never sends one".

That is three requirements failed at once, one of them Core:

- **HR-002** requires stopping a running turn without closing the tab. Killing
  the process is not that.
- **HR-011** approvals have no channel at all, so a composed policy plugin has
  to auto-answer every request.
- **HR-009** resume is impossible: `session/prompt` with an unknown session id
  always creates a fresh agent and never resumes
  (`packages/sdk/server/src/server.ts:218`).

## 4. What ACP gives, and what it does not

`packages/acp/acp/README.md` describes the server as "a transport adapter, not a
UI integration", and the implementation comment at
`packages/acp/acp/src/index.ts:152` states that raw chunks, reasoning, tools,
plans, titles, and retry markers "are presentation or trace data" and are
deliberately excluded.

Implemented: `initialize`, `authenticate` (a no-op), `session/new`,
`session/prompt`, `session/cancel`, one `session/update` variant
(`agent_message_chunk`, only for committed assistant messages), and
`session/request_permission` offering exactly `allow-once` and `reject-once`.

Not implemented, though all exist in ACP itself: `session/load`, list, resume,
delete, fork, `agent_thought_chunk`, `tool_call`, `tool_call_update`, `plan`,
`available_commands_update`, `current_mode_update`, elicitation, and any usage
reporting.

Mapped against the requirements this reaches the §7 minimum and stops there —
but it does reach it, including the stop action that the SDK interface lacks.

## 5. What the Web interface gives

The Web host serves an HTTP `/api` route plus two WebSocket downlinks,
`/api/events.mux` and `/api/events.host`, on `127.0.0.1:3080` by default. Both
sockets must be open, and a plain GET on a downlink returns 426 Upgrade
Required.

Request shape is a four-quadrant discriminated union carried as plain JSON, so a
Rust client needs no code generation — the TypeScript type generation described
in `docs/api-gateway.md` produces the browser client's types, not a protocol
requirement. `POST /api/<method>` with `{type, rpcId, method, payload}` returns
`{type, rpcId, result}`; downlink frames arrive as `server-request`; answerable
frames are replied to with `POST /api/respond` echoing the `rpcId`.

Roughly forty unary methods cover session list, search, create, history, fork,
prompt, cancel, model listing and selection, subagents, workspaces, skills,
agent presets, goals, settings, and credentials. Live frames cover session
events, approvals, questions, the inbox queue, background jobs, and projections.

Two capabilities exist only here and matter for the requirements:

- **Tool render views** (`packages/core/tools/src/presentation.ts`). The host
  computes, per tool call and result, a card describing what to display: a diff
  card carrying `FileDiff {path, oldText, newText}`, a terminal card carrying
  the command, output, exit code, and signal, plus search, read, and web cards.
  These are never persisted, so they do not appear in the raw event stream that
  the SDK interface exposes. Without them, HR-004's file changes and command
  output would have to be reconstructed by NiumaTerm from raw tool arguments.
- **Derived projections**: `tokenUsage`, `contextPressure` with the context
  window, and `contextBreakdown` — precisely the HR-010 model.

Costs and risks specific to this path:

- The transport bypasses `JsonLineProcess` and `AgentCli` entirely. NiumaTerm is
  not unequipped for it — `crates/remote_net` already uses `tokio-tungstenite`,
  which is a workspace dependency — but none of the existing agent plumbing is
  reused.
- There is no authentication. The only defense is a trust check requiring a
  loopback or explicitly trusted `Host` header, which also protects against DNS
  rebinding. A local client passes trivially, and also qualifies for the
  loopback-pinned privileged methods including settings and credentials access.
- `host.describe` carries a version string but the interface has no protocol
  version field, and the project's own position is that host and browser client
  ship together. Combined with `SESSION_FORMAT_VERSION = 0` and the README's
  statement that compatibility-breaking changes are expected, this argues for
  pinning a supported `@deepseek-ai/dsh` version range and checking
  `host.describe.version` at startup.

## 6. The underlying event vocabulary

The narrowness of ACP is a filtering choice, not a missing capability. The ACP
bridge subscribes to `ctx.on('session/event')`, which carries the full session
event union. `packages/core/session/src/known-event-types.ts` enumerates all 44
types this build understands, and they cover the requirements closely:

| DeepSeek session event | Requirement it would satisfy |
| --- | --- |
| `assistant/chunk`, `assistant/message` | HR-003 streamed assistant text |
| `tool/call`, `tool/result`, `tool/code-dispatch` | HR-003, HR-004 tool activity |
| `command/run`, `command/done` | HR-004 command output |
| `approval/asked`, `approval/decided`, `approval/policy` | HR-011 approvals |
| `turn/start`, `turn/end`, `step/start`, `step/end` | HR-006 run status and step label |
| `request/context`, `request/header` | HR-010 usage and context limits |
| `compaction/start`, `compaction/end`, `compaction/summary`, `compaction/prune` | HR-010 compaction |
| `todo/write`, `plan/mode` | ACP `plan` updates |
| `session/title` | HR-009 history titles |
| `subagent/descriptor` | HR-012 child agents |
| `tool-workflow/run-start`, `tool-workflow/run-end`, `tool-workflow/agent-start`, `tool-workflow/agent-end` | HR-012 workflows |
| `sandbox/mode`, `permission/preset`, `agent-preset/selected` | HR-007 models, permissions, effort |
| `llm/retry`, `llm/retry-started` | HR-006 status detail |

Durable history is likewise present: `ctx.sessionQuery` offers `listSessions()`,
`readSession()`, and metadata filters, with JSONL or SQLite persistence rooted at
`$DSH_HOME/sessions`.

This is why a third option exists in principle: publish our own Cordis plugin
that subscribes to the same event stream and emits richer ACP updates, installed
into a NiumaTerm-owned profile without forking `dsh`. It is now a fallback
rather than the plan, because the Web interface already exposes everything that
plugin would add — including the tool render views, which the raw event stream
does not carry — and it would commit a Rust team to maintaining a TypeScript
plugin against a pre-release plugin runtime.

## 7. NiumaTerm-side cost

### 7.1 Harness identity is spread across three enums

| Enum | Definition | Serialized as |
| --- | --- | --- |
| `AgentKind` | `crates/app/src/agent_pane/profile.rs:6` | `codex`, `claude` |
| `AgentProfileKind` | `crates/config/src/profile.rs:52` | `codex`, `claude-code` |
| `ProviderKind` | `crates/agent_utils/src/update.rs:41` | `codex`, `claude` |

Three spellings of the same concept, bridged by four conversion functions
(`profile.rs:36-48`, `session/update_recovery.rs:39-40`,
`updates/transaction.rs:457-458`). A narrower fourth,
`BackgroundTaskProvider`, lives in `crates/agent_utils/src/background_task/mod.rs`.

Adding one variant costs roughly 50 non-test match arms across about 25 files,
plus about 30 more in tests. Counts by identifier: `AgentKind::` 80 occurrences
in 17 files, `AgentProfileKind::` 49 in 8 files, `ProviderKind::` 63 in 10
files, `Backend::Codex` and `Backend::Claude` 43 together.

### 7.2 The dangerous sites are the non-exhaustive ones

The count is not the real risk. A meaningful share of these sites are `==`
comparisons or `if let` rather than exhaustive `match`, so the compiler stays
silent and a new harness inherits Codex or Claude behavior it never asked for.
Observed examples: `session/mod.rs:123, 169, 343, 345, 368, 773, 944, 945`,
`composer/mod.rs:116, 268`, `composer/palette.rs:118, 131, 246`,
`workflows.rs:146`, `transcript/format.rs:295`, `session/events.rs:80`.

Most of these are not really asking which harness this is; they are asking
whether this harness supports skills, rewind, workflows, asynchronous command
discovery, or resume. That distinction drives the refactor discussion tracked
separately.

### 7.3 There is no adapter trait

`Backend` (`crates/app/src/agent_pane/session/backend.rs:15`) is a plain enum
with about 18 methods that match and forward. The doc comment at
`backend.rs:12-14` records the intent: both backends share the
`nmt_agent_utils::chat` event vocabulary and method surface, so the pane
dispatches here and stays protocol-agnostic. That sharing is maintained by
convention; each concrete session type declares its own inherent methods.
`RecoveryIdentity` (`backend.rs:6`) is per-harness and needs a third variant.

### 7.4 Reusable plumbing

- `JsonLineProcess` (`crates/agent_utils/src/subprocess.rs:98`) already handles
  newline-delimited JSON framing, a reader thread, stderr capture, and Windows
  Job Object process-tree containment. ACP is JSON-RPC over exactly this
  transport, so it drops in without new transport code. The Web interface does
  not use it.
- `AgentCli` (`crates/agent_utils/src/launcher.rs:24`) resolves the executable
  and builds the command.
- `chat::Item::Other` (`crates/agent_utils/src/chat.rs:204`) is the existing
  escape route for tool kinds the transcript does not model, so a third harness
  can ship without adding transcript item variants.
- `crates/agent_utils/src/codex/app_server/protocol.rs` (617 lines) is a working
  template for JSON-RPC message framing and identifier routing.

For size comparison, the Codex adapter is about 3,500 non-test lines and the
Claude adapter about 6,600. An ACP adapter should land well below either,
because ACP is already a normalized event model rather than a vendor stream that
has to be interpreted.

### 7.5 Known friction

- All existing adapter code is gated `#[cfg(target_os = "windows")]`
  (`crates/agent_utils/src/lib.rs:5-14`). A new adapter inherits that gate
  unless the shared launcher is ported first.
- `LaunchConfig.provider` is a Codex-shaped field living in a shared struct
  (`crates/agent_utils/src/lib.rs:55`).
- Credentials are AES-256-GCM encrypted inside `config.toml` with a key compiled
  into the executable (`crates/config/src/credentials/mod.rs`), not an OS
  keyring. A new harness reuses this path unchanged, so HR-001 is satisfied
  without new storage work. DeepSeek credentials additionally have their own
  resolution order on the harness side, ending at `$DSH_HOME/.credentials.yaml`,
  so the profile may only need to supply `DEEPSEEK_API_KEY` through the
  environment.

## 8. Suggested phasing

**Phase 0 — validate before writing Rust.** Run
`npx @deepseek-ai/dsh-acp-demo@next --config <cordis.yml>` and drive
`initialize`, `session/new`, and `session/prompt` by hand. Confirm the server
starts on Windows, confirm what a real turn emits, and measure time to first
output. Then do the same for `dsh web --port 0`, which answers whether the
upgrade path is real. Node 24.13 is already present on the development machine,
and no repository build is needed for either.

**Phase 1 — minimum ACP adapter.** New module `crates/agent_utils/src/acp/`, a
variant in each identity enum, and mapping for `agent_message_chunk`,
`stopReason`, and `session/request_permission`. Local `/new`, `/clear`, and
`/status` only. Every unsupported capability omitted from the UI. This meets the
requirements document's §7 minimum integration scope and, being
protocol-named rather than vendor-named, also covers other ACP agents.

**Phase 2 — decision point, not a continuation.** If the Agent Tab needs tool
cards, file diffs, usage gauges, structured questions, or session resume for
DeepSeek, that means adopting the Web `/api` transport as a second adapter, not
extending the ACP one. Re-evaluate then, with Phase 0's measurements in hand.
Widening ACP through our own Cordis plugin remains available but ranks below the
Web interface for the reasons in section 6.

## 9. Out of scope

- **HR-014 managed CLI updates.** `dsh` is an npm package running on a Node
  runtime, not a self-updating single binary, so the `ProviderMaintenance`
  probe-and-update model (`crates/agent_utils/src/update.rs:256`) does not
  apply. Users update it themselves.
- **HR-015 rewind.** DeepSeek Harness has no equivalent capability, though it
  does have `session.fork`. The rewind command and its controls stay hidden, as
  the requirements document already directs for unsupported capabilities.

## 10. Open questions

- Does the published `dsh-acp-demo` composition run correctly on Windows, and
  how long does a Node-based harness take to reach its first token compared to
  the existing native harness binaries? Phase 0 answers both.
- Is the §7 minimum genuinely acceptable as a first release for this harness, or
  will the missing tool activity make the tab feel broken next to the Codex and
  Claude tabs? This decides whether Phase 2 is optional or mandatory.
- Does a NiumaTerm-owned profile need to be created and populated on first use,
  or should users install it themselves? Populating it means running pnpm on the
  user's behalf, since `dsh plugin` shells out to pnpm and requires it on PATH.
- `dsh` is at `0.1.0-rc.6`, the session log format is at version 0 with no
  migration, and the project states that compatibility-breaking changes are
  expected. What version range does the adapter declare support for, and what
  does the tab show when the installed version falls outside it?

## 11. Phase 0 results

Measured 2026-08-15 on Windows 11, Node 24.13, against
`@deepseek-ai/dsh-acp-demo@0.1.0-rc.6` installed from npm. No repository build.
The published `examples/acp-agent/cordis.yml` pulls in `dsh-hooks-claude-code`
and `dsh-hooks-codex`, which are not published, so the run used a reduced
eleven-plugin composition: the DeepSeek adapter, the local sandbox and its
policy, the local subprocess manager, sandboxed bash, one-shot approvals, the
ACP app itself, the token meter, session projections, the sandboxed filesystem
with its observation policy, and the filesystem tool.

### It runs on Windows

| Step | Elapsed from process spawn |
| --- | --- |
| `initialize` response | 261–287 ms |
| `session/new` response | +2–3 ms |
| First `agent_message_chunk`, text-only turn | 1.8 s |
| First `agent_message_chunk`, turn that runs bash | 7.1 s |
| `session/cancel` to `stopReason: "cancelled"` | 3 ms |

`initialize` reports `agentInfo` `deepseek-harness-acp/0.0.1`, empty
`authMethods`, and `promptCapabilities` with image, audio, and embedded context
all false. Stdout carried nothing but JSON-RPC; Node's SQLite experimental
warning went to stderr, so `JsonLineProcess` framing holds without a filter.

### Four findings that change the adapter

- **HR-002 is satisfied and it is fast.** `session/cancel` is a notification, and
  the in-flight `session/prompt` resolved with `stopReason: "cancelled"` 3 ms
  later. But because `agent_message_chunk` only carries committed messages, a
  cancelled turn emits **nothing at all** — the tab would show an interrupted
  turn with no text whatsoever, where Codex and Claude both leave the partial
  answer visible.
- **The approval request has nothing to approve.** `session/request_permission`
  arrived carrying only `{ toolCallId }` plus the two options. No tool name, no
  command line, no file path, no diff. An approval card built from this can only
  say "the agent wants to run something". This is worse than section 4 implied
  and it is the strongest argument that the §7 minimum will feel broken.
- **Stdin EOF does not shut the server down after a turn has run.** The README
  states EOF disposes the context and flushes sessions. It does, but only if no
  turn happened: a bare handshake exited 39 ms after EOF, while every session
  that ran a prompt was still alive 5 s later and had to be killed. The pane's
  shutdown already escalates to a kill, so this costs a timeout rather than a
  hang, but the adapter should not wait long for a graceful exit.
- **The sandbox is broken for bash on Windows.** Under `workspace-write` with
  `dsh-sandbox-local` and `dsh-bash-sandbox`, every bash invocation died before
  running anything: `fatal error - CreateFileMapping … Win32 error 5`, exit code
  256. The model retried, escalated to wider permissions, and only then got its
  output. A DeepSeek tab shipped with the sandbox on would fail every command on
  Windows until the first escalation, so the profile NiumaTerm ships needs this
  resolved or a different sandbox mode.

### The Web upgrade path is real

`dsh web --port 0` auto-initialized a profile under `$DSH_HOME/profiles/web`
with no pnpm invocation, bound a port, and served the browser UI (`200
text/html`). It prints its own URL on stdout as `dsh web: http://127.0.0.1:PORT`,
which is the discovery mechanism a launcher would use for port 0. The `/api`
route shape was not mapped; that belongs to Phase 2.

### What this does not answer

Time to first output is not comparable to the native harnesses yet — the same
prompts were not run against Codex and Claude on this machine. The 1.8 s
text-only figure includes 280 ms of Node startup, which is the part that is
structurally different from a native binary.
