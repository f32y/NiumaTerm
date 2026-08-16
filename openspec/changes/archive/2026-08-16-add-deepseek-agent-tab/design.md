## Context

Agent Tabs today dispatch through `Backend`, a plain enum in
`crates/app/src/agent_pane/session/backend.rs` that forwards to the Codex and
Claude session types. An earlier refactor removed harness-identity comparisons
from the pane in favor of a capability table
(`crates/app/src/agent_pane/capabilities.rs`), so a third harness now plugs in
by adding a variant and its capability constant rather than by hunting silent
`==` checks.

Both existing adapters interpret a vendor stream: Codex is about 3,500 non-test
lines, Claude about 6,600. `dsh` is different in kind. Its Web host already
normalizes the session event stream and computes render cards for tool calls, so
this adapter translates an already-normalized model instead of reconstructing
one. The transport and the event mapping were measured against a running host,
not read off declarations; `docs/research/deepseek-harness-integration.md`
sections 13 and 14 carry the captures and the resulting mapping table.

The constraint that shapes everything here: users install `dsh` themselves and
NiumaTerm supplies only the GUI. That rules out the ACP server, the SDK server,
and a Cordis plugin of our own, because each requires NiumaTerm to author and
maintain a `cordis.yml` composition. Only the `web` and `headless` profiles
auto-initialize, and `headless` cannot carry an interactive turn.

## Goals / Non-Goals

**Goals:**

- A DeepSeek tab a user can create, prompt, watch stream, and stop.
- One managed host process for the whole application, not one per tab.
- Reuse the existing transcript vocabulary; add no new `chat::Item` variant.
- Fail legibly when `dsh` is missing, fails to start, or is an unsupported
  version.
- Leave Codex and Claude behavior untouched.

**Non-Goals:**

- Tool call rendering, usage gauges, model selection, history and resume,
  subagents, workflows. Each is reachable on this transport and each is a
  follow-up change.
- Installing, updating, or configuring `dsh`, including its profiles, plugins,
  credentials, and sandbox policy.
- Supporting other ACP or Web-interface agents. The identity is the vendor here,
  not a protocol, because this interface is DeepSeek's own.

## Decisions

### The Web `/api` interface over the alternatives

`dsh` exposes three machine-drivable interfaces. The SDK stdio interface has no
way to cancel a turn, failing the stop requirement outright. The ACP server has
`session/cancel` but emits only committed assistant text, so a cancelled turn
shows nothing at all, and its approval request carries a bare tool-call id with
no tool name or command. Both also need a composition we would own.

The Web interface answers all three objections, measured: cancel is accepted in
4 ms and its turn-end reason distinguishes a user abort from completion, the
partial answer survives both live and in history, and approvals carry the tool
name, call id, and the asker's reason.

The cost is that `JsonLineProcess` and `AgentCli` do not apply. That is a real
loss of reuse, and it is priced: `tokio-tungstenite` is already a workspace
dependency through `crates/remote_net`, and the alternative was maintaining a
TypeScript component and a plugin composition.

### One host process, sessions per tab

The host's event stream is all-session aggregated — opening it replays every
attached session — so the natural mapping is one `dsh web` process with one
session per tab, and frame routing on the session id. One host per tab would
multiply a 1.1 s Node cold start by the tab count and gain nothing.

This makes the host a shared resource with a lifetime spanning tabs. It starts
with the first DeepSeek tab and stops with the last. Termination reuses the
existing Windows Job Object process-tree containment rather than trusting the
host to exit on its own; the ACP server was observed outliving its stdin EOF
after a turn had run, so a graceful-exit wait should be bounded.

Alternative considered: let the user run `dsh web` themselves and point
NiumaTerm at a port. Rejected — a GUI that requires a server started by hand in
another terminal before a tab works is not the integration users asked for.

### Port discovery from the host's own output

The host is launched with `--port 0` and prints `dsh web: http://127.0.0.1:PORT`
on stdout. Reading that line is the discovery mechanism; picking a port
ourselves would race other local software. A bounded wait for that line is also
the start-failure detector, since a host that dies during boot never prints it.

### Dispatch the transcript on the render card, not the tool name

Even though tool rendering is a later change, the mapping is decided now because
it shapes the event router. The host attaches a render card to tool events whose
kind is one of `generic`, `terminal`, `diff`, `search`, `read`, `web`. Keying the
transcript variant on the card means a tool this build has never heard of still
lands in the right row, and anything unrecognized falls to `chat::Item::Other`,
which exists for exactly that. Keying on tool names instead would need a table
updated on every harness release.

### Approvals are part of a runnable tab, not an enhancement

Testing the first build settled a question the plan had answered wrongly. The
harness blocks a turn on an unanswered approval indefinitely, and the default
sandbox reaches one as soon as the model touches anything outside the workspace.
A tab that recognized the frame and did nothing therefore stalled for as long as
the user was willing to wait, with no card, no error, and no way to tell what it
was waiting for. Deferring approvals was deferring a hang.

The harness accepts only `allowed-once` and `rejected` from a client;
`cancelled` and `unavailable` are outcomes it reaches on its own. The existing
approval card offers four decisions, one of which — allow for the rest of the
session — has no expression here. Rather than silently downgrading it to
allow-once, which would tell the user they granted more than they did, that
control is hidden through a capability. Cancelling the turn is a refusal
followed by a stop, because refusing alone would leave the turn running.

### Version support is read from the package, not the host

`host.describe` reports a `version`, but it is the web app plugin's own `0.0.1`
rather than the harness release, and the interface carries no protocol version
field. So a supported-range check has to read the installed `dsh` package
version. Because `dsh` is pre-release with an explicit expectation of breaking
changes, an out-of-range version reports what is installed and what is supported
and still lets the user proceed, rather than blocking a tab on a version
comparison this build cannot keep current.

## Risks / Trade-offs

- **The host has no authentication.** Its only fence is a loopback and trusted
  `Host` check, which any local process passes, and loopback callers reach its
  privileged settings and credentials methods. → Bind loopback on an ephemeral
  port and do not widen it. This is the same exposure as the browser UI users
  already run, and it is inherent to the interface rather than introduced here;
  it is worth stating in user-facing documentation rather than silently
  accepting.
- **`dsh` is `0.1.0-rc.6` with a session log format at version 0 and an explicit
  expectation of breaking changes.** → Keep the client to the six methods this
  slice needs, treat unknown event types as ignorable rather than fatal, and
  report an unsupported version without blocking.
- **A malformed answer to an answerable frame fails silently.** Sending the wrong
  shape returns a rejection receipt and then nothing happens — the turn hangs.
  → The response path checks the receipt and reports a refusal instead of
  assuming the answer landed.
- **Node startup makes the first tab slower than Codex or Claude.** Cold start to
  a serving host was 1.1 s. → Paid once per application run, not per tab or per
  turn; the tab should show that the harness is starting rather than appearing
  hung.
- **A shared host is a shared failure.** One host exiting affects every DeepSeek
  tab at once. → Each affected tab reports the harness stopped rather than
  appearing idle and silently dropping prompts.
- ~~**The `dsh` sandbox was observed failing every bash invocation on
  Windows**~~ under the composition used for earlier ACP measurements. Checked
  against the web profile and it does not reproduce: that composition offers
  `dsh-tool-pwsh`, so a shell command runs through PowerShell instead.

## Migration Plan

Additive. No existing profile, transcript, or setting changes meaning, and no
stored data is rewritten. Codex and Claude tabs are unaffected because the
capability table already carries per-kind constants rather than defaults.

Rollback is removing the harness from the selectable kinds; profiles created
against it would then fail to open, which is the same behavior as any profile
naming an unavailable harness.

## Open Questions

- What supported `dsh` version range does this build declare, and does it widen
  on each release or track a tested version?
- Should the host's working directory follow the tab's working directory? The
  host reports one project directory at `host.describe`, while sessions accept
  their own `cwd` at creation, so per-tab working directories look reachable
  through session creation — unverified.
- ~~Does the web profile's default composition reproduce the Windows sandbox
  failure?~~ Answered: it does not. The web profile composes `dsh-tool-pwsh`,
  and a probe turn asking for a shell command ran it through PowerShell and
  succeeded. The failure recorded earlier belonged to `dsh-bash-sandbox` in the
  reduced ACP composition, which this integration never loads.
