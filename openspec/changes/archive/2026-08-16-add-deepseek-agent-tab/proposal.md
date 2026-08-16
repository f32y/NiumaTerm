## Why

NiumaTerm's Agent Tabs support Codex and Claude Code. DeepSeek Harness (`dsh`)
is the third harness we want, and research settled both the transport and the
event mapping: `dsh web` serves a local `/api` interface that carries streamed
text and reasoning, tool render cards, fully described approvals, usage
projections, and history, against a stock user-installed `dsh` with no
DeepSeek-side code of ours. Measurements are in
`docs/research/deepseek-harness-integration.md` sections 13 and 14.

This change lands a DeepSeek tab a user can create, prompt, watch work, approve,
and stop. It started narrower — text and stopping only — and testing the first
build moved two items in: an unanswered approval blocks a turn indefinitely, and
an agent whose tool calls are invisible gives the user nothing to judge that
approval by. Usage gauges, model selection, and history remain separate changes,
so there is a running tab to judge them against.

## What Changes

- A `dsh web --port 0` host process, managed by NiumaTerm, discovered through
  the `dsh web: http://127.0.0.1:PORT` line it prints on stdout. One host serves
  every DeepSeek tab, because its event stream is all-session aggregated.
- A DeepSeek harness identity across the existing identity enums, so a DeepSeek
  agent profile can be created and launched.
- The two hand-written UI harness-kind lists become data-driven. Without this a
  registered kind cannot be selected in settings at all, so the tab would be
  unreachable.
- A client for the `/api` interface: two WebSocket downlinks, and the unary
  methods this slice needs (`host.describe`, `session.create`, `session.prompt`,
  `session.cancel`).
- Streamed assistant text and reasoning, and the user's own prompt, rendered
  through the existing backend-neutral transcript vocabulary.
- Stopping a running turn, with the partial answer preserved on screen.
- Run status and step progress driven by the harness's own turn lifecycle.
- The approval round trip. The harness blocks a turn until an approval is
  answered, so a tab that cannot answer one stalls with nothing on screen
  explaining why. That made it part of a runnable tab rather than an
  enhancement, and testing the first build is what showed it.
- Capabilities the harness does not offer in this slice are hidden rather than
  shown inert, following the requirements document's direction for unsupported
  capabilities. That now includes a decision the harness cannot express:
  granting an approval for the rest of the session.
- Tool activity in the transcript: commands with their output and exit status,
  file changes with a reviewable diff, and every other call as its own row.
  Watching an agent work in silence is what made the approval above impossible
  to interpret, so the two landed together.

Deferred to follow-up changes, listed so their ownership is unambiguous: token
and context usage, model and reasoning-effort selection, session history and
resume, subagents and workflows, and slash commands beyond the local ones.

## Capabilities

### New Capabilities

- `agent-deepseek-harness`: a DeepSeek Harness Agent Tab — locating and managing
  the `dsh web` host, creating and prompting a session over its local `/api`
  interface, rendering streamed assistant text, reasoning, and tool activity,
  answering the approvals that block a turn, stopping a turn, and reporting run
  status.
  Includes what the tab shows when `dsh` is absent, unusable, or a version this
  build does not support.

### Modified Capabilities

- `agent-binary-updates`: the update surface currently assumes every agent
  profile resolves to a vendor-managed installation that can be probed and
  updated in place. `dsh` is an npm package on a Node runtime with no such
  path, so Agent General needs to contribute no update row for it rather than
  showing a permanently unsupported one.

## Impact

- **New**: a DeepSeek adapter module under `crates/agent_utils`, and its
  transcript session type in the Agent pane.
- **Modified**: the harness identity enums (`AgentKind`, `AgentProfileKind`,
  `ProviderKind`, `Backend`, `RecoveryIdentity`) and the capability table added
  by the earlier refactor; the two hand-written UI kind lists; the update
  surface's per-installation row selection.
- **Dependencies**: `tokio-tungstenite`, already a workspace dependency used by
  `crates/remote_net`. `JsonLineProcess` does not apply to this transport.
- **External**: users install and update `dsh` themselves; NiumaTerm ships no
  DeepSeek profile, plugin, or Cordis composition. `dsh` requires Node 22 or
  newer.
- **Security**: the `dsh web` host has no authentication beyond a loopback host
  check, and loopback callers reach its privileged settings and credentials
  methods. This change binds it to loopback on an ephemeral port and does not
  widen it.
