# Agent Harness Work — Where We Left Off

| Field | Value |
| --- | --- |
| Status | R1–R3 landed; DeepSeek transport decided and mapped; no adapter code yet |
| Date | 2026-08-15; revised 2026-08-16 |
| Purpose | Pick the work back up without re-reading the long documents |

## The three documents

| Document | What it answers |
| --- | --- |
| [`agent-harness-integration-requirements.md`](../agent-harness-integration-requirements.md) | What any new harness must do (HR-001…HR-016, and the §7 minimum scope) |
| [`deepseek-harness-integration.md`](./deepseek-harness-integration.md) | How to connect DeepSeek Harness specifically |
| [`agent-harness-refactor.md`](./agent-harness-refactor.md) | How to make the third and later harnesses cheap to add |

This page is the short version of the last two.

## What we concluded

**On connecting `dsh`.** The scope narrowed on 2026-08-16: users install `dsh`
themselves, and NiumaTerm supplies only the GUI. That single constraint decides
the transport, because the ACP server, the SDK server, and any plugin of ours
all require NiumaTerm to author and maintain a `cordis.yml` composition — which
is shipping a DeepSeek distribution, not integrating one. Only the `web` and
`headless` profiles auto-initialize, and `headless` cannot carry an interactive
turn.

So the answer is the **Web `/api` interface** served by `dsh web`, which section
13 of the DeepSeek document measured end to end against a stock npm install. It
carries everything the requirements ask for: streamed text and reasoning as
separate deltas, tool render cards with contextual diffs computed by the host,
fully described approvals, token and context projections, model and effort
selection, history, and a designed reconnect path.

The three interfaces ruled out, and why:

- **ACP** (`dsh-acp-demo`) — committed assistant text and one-shot approvals
  only. A cancelled turn emits nothing, and its approval carries a bare
  `toolCallId`. Also needs a composition we would own.
- **SDK stdio JSON-RPC** (`dsh-jsonrpc-agent`) — rich events, but only three
  client methods and no way to cancel a turn, failing HR-002 outright.
- **Our own Cordis plugin** — reaches everything (proven by writing one), but it
  is a TypeScript component to build, ship, and keep working against a
  pre-release plugin runtime, and it needs a composition too.

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

**Phase 0 (ACP) is measured**, on Windows against the published `dsh-acp-demo`.
Full numbers in section 11 of the DeepSeek document. It is now background: the
transport works and `session/cancel` is fast, but the server is too thin to
build a tab on, and it needs a composition we would own.

**The Web `/api` surface is mapped**, section 13. Three real prompted turns
against a stock `dsh web`, covering tool cards, cancel, and approvals. The
reproduction lives in `.scratch/dsh-web/` (uncommitted): an isolated
`package.json`, `drive.mjs` with three scenarios, and one frame log per
scenario. It needs `DEEPSEEK_API_KEY` in the environment.

## Next

The transport question is settled and the frames are mapped (section 14 of the
DeepSeek document), so the remaining work is Rust.

1. **The adapter.** One `dsh web --port 0` process for the whole application
   (the mux stream is all-session aggregated, so one host serves every tab),
   URL discovered from its stdout line, two WebSocket downlinks, and six unary
   methods for a first release: `host.describe`, `session.create`,
   `session.prompt`, `session.cancel`, `session.history`, `session.models`.
   `JsonLineProcess` does not apply; `tokio-tungstenite` is already a workspace
   dependency via `crates/remote_net`.
2. **R5 ships with it** — data-drive the two hand-written UI kind lists.
   Without it a registered kind cannot be selected in settings at all.
3. **Teach `Item::task_tally` DeepSeek's todo tool name.** It matches only
   Claude's `TodoWrite` (`crates/agent_utils/src/chat.rs:248`); DeepSeek's is
   `todo_write`, so the plan tally stays dark until both are recognized.

## Decisions still open

- R4 (collapsing the five identity enums into one) needs a new dependency edge
  from `nmt_config` to `nmt_agent_utils`. Worth about 20 fewer sites. Deferrable.
- `dsh` is at `0.1.0-rc.6` with an explicit expectation of breaking changes and
  a session log format at version 0 with no migration. `host.describe.version`
  cannot serve as the gate — it reports the web app plugin's own `0.0.1` — so a
  supported-version check has to read the installed package instead, and the tab
  needs something to show when the installed version falls outside the range.
- `dsh web` has no authentication beyond a loopback `Host` check, and loopback
  callers reach the privileged settings and credentials methods. Whether
  NiumaTerm should bind it more narrowly, or simply document that a `dsh web`
  host is as exposed as the browser UI users already run, is undecided.
- Section 11's Windows sandbox failure was never re-tested against the web
  profile's own composition, because no bash tool ran in the section 13
  scenarios. If it reproduces there it is upstream's to fix, but it decides how
  a first release behaves on the first command the model runs.

## Not done

No adapter code exists. R4 through R7 from the refactor document remain open.
