# DeepSeek Harness Integration Research

| Field | Value |
| --- | --- |
| Status | Research; Phase 0 measured on Windows (section 11); plugin path proven by writing one (section 12) |
| Date | 2026-08-14; revised 2026-08-15 |
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
  **Superseded by section 12**: a plugin we write reaches the same data over
  stdio, so the upgrade path is now a real choice between the two rather than
  the Web interface by default.
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

### 3.1 The Python SDK is a client of this, not a fourth interface

Re-checked 2026-08-15 against the repository at `47f9438`. `python/sdk`
(`deepseek-harness-sdk`, 873 lines) drives the runtime as a subprocess over the
same newline-delimited JSON-RPC, so it inherits all three failures above
unchanged. The server's dispatch still answers exactly `initialize`,
`session/prompt`, and `shutdown` and throws on anything else, and
`getOrCreateSession` still calls `ctx.agents.create`, never `resume`. The client
does carry `next_request`/`respond` plumbing for server-initiated requests, but
the server contains no code that sends one, matching the protocol package's own
statement.

It is also unusable here for a second, simpler reason. The bundled runtime
binaries exist only for Linux and macOS —
`_PLATFORM_TAGS = {"linux": "linux", "darwin": "macos"}` — and the resolver
raises on any other platform, naming `linux/macos on x64/arm64` as the supported
set. NiumaTerm is Windows-only. Pointing the SDK at a Node-run
`dsh-jsonrpc-agent` instead is possible, but then the Python layer adds a Python
runtime while providing nothing a Rust client would not do directly against the
same three methods.

The event side really is rich: `session.event`, `session.status`,
`subagent.started`, and `subagent.finished`. So this interface offers the data
and withholds the operations, which is the same verdict as section 3 and not a
new option.

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

  **Correction (section 12).** The views are indeed not persisted, but the
  conclusion drawn here is wrong. They are a pure derivation from data the log
  does carry, computed through a presenter that any plugin can reach, so
  reconstruction from raw arguments is not the alternative.
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

**Correction (section 12).** The reason given for ranking the plugin below the
Web interface does not hold: the render views are reachable from a plugin. The
remaining objection, maintaining TypeScript against a pre-release runtime, is
real and is what section 12 weighs instead.

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

## 12. Writing our own plugin

Measured 2026-08-15 by reading the published type declarations and package
documentation of `@deepseek-ai/*` at `0.1.0-rc.6`, installed from npm. Every
package ships `lib/types/*.d.ts` and a README, so the extension points are
readable without building the repository.

### 12.1 The answer

A NiumaTerm-owned plugin can expose everything the requirements ask for,
including the two capabilities section 5 listed as reachable only through the
Web interface. Sections 5 and 6 rank the plugin last partly on a claim that does
not survive checking, and the ranking should be redone.

The plugin would be an application plugin in the same position as the ACP
server: `apply(ctx, config)` opens a transport of our choosing on stdio,
subscribes to the session event stream, and drives the same services the Web
host drives. `dsh-acp` compiles to 530 lines, which is the closest available
size comparison.

### 12.2 Why the render-view claim fails

Three declarations settle it, and they agree with each other.

- `ToolDefinition.presentCall(args)` and `presentResult(args, result)` live on
  the tool registry (`ctx.tools`), which is host-plane and therefore visible to
  every plugin in the composition. Their declarations require both to be pure
  and side-effect-free, with the stated reason that a UI may call them during
  live streaming and during a session-log replay.
- The registry's `get(name, scope)` is public and returns the definition
  including those two methods. Its own documentation is addressed to presenter
  callers: it explains that they pass the calling agent so the rendered card
  matches the definition that executed.
- `ToolEventView` in `dsh-host-apiproxy` describes what the Web host attaches to
  a tool event as a pure derivation of args and result through the presenter,
  and says it is never persisted because the session log carries only the event.

So the Web host holds no privilege here. It computes a card from the event plus
the registry, and any plugin holding `ctx.tools` computes the identical card
from the identical inputs. Those inputs are in the log: `tool/call` carries
`name` and the raw `arguments` string as the model produced it, and
`tool/result` carries the model-facing message, an optional failure identity,
and the tool-private `meta`. The `meta` declaration states outright that the
durable log reproduces the identical card on replay, and names `dsh-tool-fs`
carrying its result-time contextual diff there.

Section 5's wording was accurate — the views are not persisted — but persistence
was the wrong question. What matters is whether the derivation is reproducible,
and it is designed to be.

### 12.3 What a plugin reaches

| Requirement | Service and entry point | Notes |
| --- | --- | --- |
| HR-003 streamed text, reasoning, tools | session event stream | The full 44-type union, which ACP filters down to committed assistant text |
| HR-004 file changes, command output | `ctx.tools.get(name, scope)` then `presentCall` / `presentResult` | Diff and terminal cards, section 12.2 |
| HR-002 stop | `ctx.agents` handle | Already reached by ACP, measured at 3 ms in section 11 |
| HR-009 resume | `AgentRegistry.resume({resumeSessionId})` | Exists on the service; ACP does not expose it |
| HR-009 history | `ctx.sessionQuery.listSessions()` / `readSession()` | |
| HR-010 usage and context | `ctx.sessionProjections` | A declaration-merged table filled by the token meter and cache packages |
| HR-011 approvals | `approval/request` waterfall listener, or an answerer | Request carries the agent, tool name, call id, and the asker's reason |
| HR-011 questions | `UserQuestionProvider.ask()` on `ctx.userQuestions` | The service takes one active provider, supplied by whichever package is the UI |
| HR-012 child agents, workflows | `ctx.subagents`, plus the subagent and workflow session events | |

### 12.4 It also repairs the worst Phase 0 finding

Section 11 found that `session/request_permission` arrives carrying only a tool
call id and two options, so an approval card could say no more than that the
agent wants to run something. That is ACP discarding data, not data the harness
lacks.

`ApprovalRequest` carries the agent, the tool name, the optional call id, and
the asker's human-readable reason. Its `callId` documentation states the intent
directly: arguments are not duplicated on the request because the id links to a
tool call the UI already presented. ACP never presents tool calls, so its
approval has nothing to attach to. A plugin that emits tool calls has a fully
described approval, which is the capability that decides whether the tab feels
broken next to the Codex and Claude tabs.

### 12.5 Loading it without publishing

`dsh plugin add` shells out to pnpm, but publishing is not required: the loader
resolves relative plugin specifiers against a configured base URL, so a
composition can name a plugin directory by path. NiumaTerm can therefore ship
the plugin inside its own installation and compose a profile that points at it,
with no npm publish and no pnpm on the user's PATH.

### 12.6 The costs that are real

Removing the render-view objection does not make this free.

- **A TypeScript component in a Rust product.** It needs a build step and a
  place in the installation layout. This is the objection section 6 raised that
  still stands. The Node dependency may not, however — see 12.6.1.
- **The interface is pre-release and elaborate.** The declarations read as a
  fast-moving design: scoped registries, execution tokens, waterfall events,
  generated invocation descriptors. `dsh` is at `0.1.0-rc.6`, the session log
  format is at version 0, and the project says compatibility-breaking changes
  are expected. A plugin binds to far more of that surface than an ACP client
  does.
- **We would own a protocol.** The plugin's output is our own format, so there
  is no upstream definition to conform to and no other client to catch drift.
  That is also the advantage: it can be shaped to what the Agent Tab already
  models, so the adapter stays small.
- **The Windows sandbox failure in section 11 is unaffected.** It belongs to the
  profile composition, not the transport, and has to be resolved either way.

#### 12.6.1 A composition can be packaged as one executable

Found while checking the Python SDK. `scripts/build-exe-for-python-sdk.ts`
bundles a whole Cordis composition into a single self-contained binary using
`@yao-pkg/pkg`, which is how the Python SDK ships a runtime without asking the
user for Node. Section 2's "not a self-contained executable" describes how `dsh`
is normally launched, not a limit of what can be built from it.

This matters because our plugin is just another entry in a composition, so the
same route would produce one Windows executable carrying the harness, our
plugin, and Node together. That answers section 7.5's Node dependency and makes
a DeepSeek tab ship like the Codex and Claude tabs do.

It has not been done, and two things stand between:

- `PLATFORMS = ['linux', 'macos']` is the script's own allowlist. `@yao-pkg/pkg`
  builds `win` targets, and the script already recognizes a `win32` host when
  choosing a package manager, so this looks like a target nobody needed rather
  than one that was refused.
- Native modules are handled per platform and Windows has no branch: macOS
  copies `node-pty`'s `spawn-helper` beside the product, and Linux stages a
  `pty.node` that must be built on the target architecture. Windows `node-pty`
  uses ConPTY and publishes prebuilds, so this is likely to be work rather than
  a wall, but it is unproven and it is the piece most likely to fail.

Worth an experiment on its own, and cheap: the build script takes explicit
`--targets`, so trying `node24-win-x64` costs one run.

### 12.7 How this changes the options

The choice is no longer capability against convenience, because the plugin and
the Web interface reach the same data. They differ in what NiumaTerm has to
carry:

- **Web `/api`.** No TypeScript to maintain, but HTTP plus two WebSockets,
  bypassing `JsonLineProcess` and `AgentCli` entirely, against roughly forty
  unary methods and a four-quadrant union that is versioned only by a host
  version string, on an interface with no authentication beyond a loopback
  check.
- **Our own plugin.** A TypeScript component to build and ship, but the
  transport is newline-delimited JSON over stdio, which the existing plumbing
  already handles, and the shape of the data is ours to choose, so the Rust
  adapter is smaller than a general client of somebody else's forty methods.

The plugin trades a Rust-side integration cost for a TypeScript-side
maintenance cost. Which is cheaper depends on a question section 10 already
asks and nothing here answers: how long DeepSeek support has to stay working
without attention.

### 12.8 Measured by writing one (all three confirmed)

A plugin was written and run on 2026-08-15 against packages at `0.1.0-rc.6`,
composed with the DeepSeek provider, the local sandbox and its policy, the local
subprocess manager, the local filesystem, the string-replace editor, the
filesystem tool, the approval service, and the demo agent spine. It spends no
model call: the plugin drives the tool registry itself, so the call, its result,
and its `meta` are all real.

All three hold. The first two were reached without spending a model call; the
third needed one prompted turn, which was then run against the real DeepSeek
provider.

**Claim 1 — a plugin composed by relative path loads: CONFIRMED.** Its `apply`
ran and `ctx.tools` resolved. One detail matters for packaging: the loader hands
the specifier to an ESM import, so it must name the file (`./nmt-probe/index.js`)
and not the directory. A directory fails with `ERR_UNSUPPORTED_DIR_IMPORT`
despite a valid `package.json` beside it.

**Claim 2 — a plugin computes the render card: CONFIRMED.** `ctx.tools.get('edit')`
returned a definition carrying both presenters, and they produced real cards.
`presentCall` on the arguments alone:

```json
{ "card": "diff", "title": "Edit .../nmt-probe-target.txt",
  "diffs": [{ "path": "...", "oldText": "before", "newText": "after" }] }
```

`presentResult`, given the arguments plus `{content, isError, meta}`, returned
the same `diff` card with the *contextual* diff rather than the argument
fragments:

```json
{ "diffs": [{ "oldText": "line one\nbefore\nline three",
              "newText": "line one\nafter\nline three" }] }
```

That difference is the point: HR-004 wants the file change, and the presenter
supplies surrounding context that the raw arguments do not contain.

The same computation was then repeated against a real prompted turn, taking
arguments from `tool/call` and `{content, isError, meta}` from the matching
`tool/result` — the inputs a client reading the durable log actually holds. It
produced the same `diff` card, and a `read` card for the read call:

```json
{ "card": "read", "path": "...", "offset": 1, "totalLines": 3,
  "lines": [{ "number": 2, "text": "before" }, "..."] }
```

Two details a real adapter has to handle. A tool call that FAILED carries no
`meta` and its `presentResult` returns null, so the generic fallback is not an
edge case but the normal path for every denied or errored call. And the event's
`message.content` wraps the model-facing blocks inside one `tool-result` block;
the presenter is declared against the inner blocks, so passing the wrapper
returns null and looks like a missing card.

Which tools carry presenters is per tool, and worth knowing before promising a
card: `read`, `write`, and `edit` each declare `presentCall`, `presentResult`,
and `presentationMeta`, while `str_replace_editor` declares only `presentCall`.

**Claim 3 — an approval answerer sees the tool call identity: CONFIRMED.** It
needed the sandboxing filesystem backend (`dsh-fs-sandbox` in place of
`dsh-fs-local`, which applies no approval policy at all) and one prompted turn,
because the approval service refuses a question raised outside an open turn:

> `approval.request()` outside an open turn: the `approval/asked` +
> `approval/decided` audit pair must be turn-enclosed. Ask from inside the turn
> that needs the decision.

Under `workspace-write`, the model's edit to a path outside the workspace was
denied with `FS_SANDBOX_DENIED`, the model retried once with
`sandbox_permissions` and a justification, and the answerer this plugin
registered received:

```json
{ "toolName": "edit",
  "callId": "call_00_ET_YqTkrV7kgexqFUfbItsd8863",
  "reason": "escalate sandbox to danger-full-access: I need danger-full-access to apply the requested single-word edit ...",
  "agent": "present" }
```

That `callId` is one the plugin had already observed on a `tool/call`, which is
the whole claim: an approval card can be attached to the tool call it already
rendered. The plugin's answer was honored — `approval/decided` reported
`allowed-once` and the edit then applied — so a plugin is a real decision
maker here, not just an observer.

One trap in that path: the outcome vocabulary is
`allowed-once | rejected | cancelled | unavailable`. ACP's `allow-once` spelling
is not a member, and returning it does not fail loudly; the waterfall simply
falls through to the fail-closed `unavailable` and the escalation is refused.
The first run of this experiment did exactly that.

Smaller results worth carrying into any real plugin:

- The `session/event` listener is called as `(session, event)`, with the event
  shaped `{type, seq, time, data}`. It delivers the full vocabulary: one turn
  here produced `turn/start`, `step/start`, `user/message`, `session/title`,
  `request/header`, `request/context`, 665 `assistant/chunk`, 5
  `assistant/message`, 4 `tool/call`, 4 `tool/result`, `approval/asked`,
  `approval/decided`, `step/end`, `turn/end`.
- The DeepSeek provider registers as `deepseek-official`, not `deepseek`. An
  unknown provider does not fail at boot: the agent is created, the turn opens,
  and it ends with `NO_ADAPTER` inside `turn/end`.

- `inject` takes an array in this Cordis build. The `{required, optional}` object
  form is read as two services literally named `required` and `optional`, and
  the plugin then hangs pending forever.
- Reading a service key the plugin did not inject throws rather than returning
  `undefined`, so capability probing needs a guard.
- `ctx.agents.create()` succeeds without prompting and yields a handle with a
  live session, which is how an adapter can prepare a tab before the first
  message.
