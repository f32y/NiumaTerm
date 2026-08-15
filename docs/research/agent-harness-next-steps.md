# Agent Harness Work — Where We Left Off

| Field | Value |
| --- | --- |
| Status | R1–R3 landed; the DeepSeek path is still unvalidated |
| Date | 2026-08-15 |
| Purpose | Pick the work back up without re-reading the long documents |

## The three documents

| Document | What it answers |
| --- | --- |
| [`agent-harness-integration-requirements.md`](../agent-harness-integration-requirements.md) | What any new harness must do (HR-001…HR-016, and the §7 minimum scope) |
| [`deepseek-harness-integration.md`](./deepseek-harness-integration.md) | How to connect DeepSeek Harness specifically |
| [`agent-harness-refactor.md`](./agent-harness-refactor.md) | How to make the third and later harnesses cheap to add |

This page is the short version of the last two.

## What we concluded

**On connecting `dsh`.** It exposes three machine-drivable interfaces, not one:

- **Web `/api`** (`dsh web`) — everything the requirements list, including tool
  render cards with file diffs and command exit codes, and the usage and
  context-window projections. HTTP plus two WebSockets.
- **SDK stdio JSON-RPC** (`dsh-jsonrpc-agent`) — rich events, but only three
  client methods and **no way to cancel a turn**, which fails HR-002 outright.
  Ruled out.
- **ACP** (`dsh-acp-demo`) — committed assistant text and one-shot tool
  approvals only, but it does have `session/cancel`.

Plan: start on **ACP**, which lands the §7 minimum with no DeepSeek-side code,
reuses `JsonLineProcess`, and also covers other ACP agents. Name the harness
identity after the protocol, not the vendor. If the tab later needs tool
activity, usage, or resume, that means adopting **Web `/api`** as a second
adapter, not extending the ACP one.

**On the refactor.** A trait for `Backend` is not worth it — only 6 of its 18
methods have matching signatures, 9 already carry stub arms, `adapter_commands`
is not object-safe and has a caller with no session, and construction cannot be
expressed as a trait method. Keep the enum.

The real problem is one layer up: **93 branch sites on harness identity, 41 of
which fail silently** for a new harness. Twenty distinct capabilities are
encoded as identity checks and written down nowhere.

## What has been done

**Option B is complete through R1.** Three commits on `dev`:

- **R2 + R3** — the pane no longer names a `Backend` variant anywhere.
  `begin_task_restoration`, `finish_task_restoration`, `resume_thread` (now
  returning whether it was accepted), and `request_more_history` are Backend
  methods; `Backend::spawn` owns construction, the stderr sink, and how a
  recovery id is used; `RecoveryIdentity` is `{ kind, id }` with the
  empty-conversation case moved into an `Option`; the background-task match is
  exhaustive on both enums.
- **R1** — `agent_pane/capabilities.rs` holds a `Capabilities` struct with one
  `const` per kind and no `Default`. Ten capabilities are named there, and no
  production `AgentKind` equality comparison remains in the crate. Every
  surviving `AgentKind::` use is an exhaustive match or a constructor.

No behavior changed and the 376 existing tests pass unmodified.

## Next

**Option A — validate the DeepSeek path (about half a day, no Rust).**

```sh
npx @deepseek-ai/dsh-acp-demo@next --config <cordis.yml>
```

Drive `initialize`, `session/new`, and `session/prompt` by hand over stdio.
Answers: does it run on Windows, what does a real turn actually emit, and how
long to first output compared to the native harness binaries. Then try
`dsh web --port 0` to check the upgrade path is real. Node 24.13 is already
installed and no repository build is needed. A starting `cordis.yml` can be
copied from `examples/acp-agent/cordis.yml` in the DeepSeek Harness tree.

It is cheap, and if the ACP path turns out not to work on Windows or to be
unacceptably slow, the whole DeepSeek plan changes.

Then land the third harness against the refactored shape. R5 (data-drive the two
hand-written UI kind lists) is what still keeps a registered kind from appearing
in settings at all, so it comes with that work rather than before it.

## Decisions still open

- Is the §7 minimum acceptable as a first release for this harness, or will a
  tab with no visible tool activity feel broken next to the Codex and Claude
  tabs? This decides whether Web `/api` is optional or mandatory.
- R4 (collapsing the five identity enums into one) needs a new dependency edge
  from `nmt_config` to `nmt_agent_utils`. Worth about 20 fewer sites. Deferrable.
- Does NiumaTerm create and populate the DeepSeek profile on first use? Doing so
  means running pnpm on the user's behalf, since `dsh plugin` shells out to it.
- `dsh` is at `0.1.0-rc.6` with an explicit expectation of breaking changes and
  a session log format at version 0 with no migration. What version range does
  the adapter support, and what does the tab show outside it?

## Not done

Nothing DeepSeek-specific: no `dsh` interface has been exercised, and no adapter
code exists. R4 through R7 from the refactor document remain open.
