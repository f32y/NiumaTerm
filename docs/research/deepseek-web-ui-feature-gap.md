# DeepSeek Harness Web UI — Feature Inventory and Gap Analysis

| Field | Value |
| --- | --- |
| Status | Survey complete; §3.2 implemented except image attachments |
| Date | 2026-08-17 |
| Scope | What `dsh web` offers, what our DeepSeek Agent Tab omits, and what no NiumaTerm tab offers today |
| Source tree | `D:\Park\deepseek-harness` (dsh `0.1.0-rc.*`) |
| Companion | [`deepseek-harness-integration.md`](./deepseek-harness-integration.md), [`agent-harness-next-steps.md`](./agent-harness-next-steps.md) |

Paths written as `packages/...` or `apps/...` refer to the DeepSeek Harness
repository. NiumaTerm paths always start with `crates/`.

## 1. How this inventory was built

Three sources, cross-checked:

- `packages/bundle/web-app/cordis.patch.yml` — the composition the `web` profile
  boots. Every browser plugin the Web UI runs is a row there, so the roster is
  exhaustive rather than sampled.
- `packages/client/ui-*/README.md` — 30 packages, each documenting its own
  surface and its own deferred work.
- `packages/host/apiproxy/src/api/rpc-map.ts` — the 50 registered RPC methods,
  plus the typed gateway namespaces (`commands`, `goals`, `messageFeedback`,
  `pluginInventory`).

`apps/web/tests/snapshots/` holds 58 named scenarios that corroborate the list.

## 2. Web UI feature inventory

### 2.1 Layout and session organization

| Feature | Browser package | Host methods |
| --- | --- | --- |
| Three-column AppFrame (sidebar / conversation / details), draggable, collapses to a 56px rail | `ui-layout` | — |
| Workspace grouping: add, rename, delete, drag-reorder | `ui-workspace` | `workspace.list/create/rename/delete/insertBefore` |
| Session rows: grouped or flat, Manual or Last-updated ordering, drag-reorder, rename, fork, archive | `ui-workspace` | `session.rename`, `session.fork`, `workspace.archiveSession`, `workspace.insertSessionBefore` |
| Session search: immediate title and workspace substring match, plus a 250 ms debounced server-side full-text content search with ranked snippets, capped at 20 | `ui-workspace` | `session.search` |
| Directory picking, two interchangeable implementations: an OS chooser, or an in-app 680×500 Miller-column browser with breadcrumbs, editable path, new-folder creation, and a hidden-entry toggle | `ui-directory-picker-native` / `-browse` | `host.pickDirectory`, `host.listDirectory`, `host.createDirectory` |

### 2.2 Conversation views

- **Chat view** — grouped step-summary flow, streaming-tail isolation, turn status.
- **Trajectory view** — a second tab in the same view ring (the ring is a slot,
  so a plugin can add more). A turn-aware event ledger with selectable User,
  Assistant, Tool, and nested Subtool records; selection opens an inspector
  showing token usage, duration, Input, Output, and Timing. A fixed Overview
  above the ledger draws real start/duration timing left to right, splitting
  each Assistant span into **TTFT and decoding**. Wheel zooms the time domain,
  drag selects an interval and filters the ledger to records live in it, right
  drag pans, right click clears. The ledger virtualizes rows and pages older
  history in on demand.
- **Compaction rows** — one collapsed row at the checkpoint position showing the
  replaced-item and estimated-token counts, expanding to the summary. Manual
  `/compact` and automatic compaction carry different titles.
- **Markdown** — GFM, KaTeX math, images, shiki highlighting, CJK strong
  emphasis, URL-promoted inline code.
- **Message actions** — copy, branch, and a Like/Dislike pair with an optional
  note (`ui-message-feedback`, per-item compare-and-set with conflict reconciliation).
- **Deliverables turn tail** — a row after each closing assistant message
  listing the files that turn produced (up to six chips plus `+ N files`, each
  opening through the host opener, with a **Show in folder** action on
  loopback). The vocabulary comes from the mutation tools' own reported
  locations, so a file is listed whether or not the model mentioned it. The
  same plugin links matching inline-code file references in the closing prose.
- **Composer dock stack** — Todo strip, GoalBar, Queue rows, and a session-stats
  strip (whole-log turn and step counts) sticky above the input.
- **Image attachments** — full-page drop overlay, a horizontally scrolling
  64px thumbnail rail in the composer with edge paging arrows and wheel-to-
  horizontal conversion, a chat-history image gallery, and a full-screen
  lightbox.
- **User questions** (`ask_user`) — one question at a time with progress
  navigation, single- and multi-select choices, recommendation badges, custom
  answers, markdown detail. A question declaring the `plan-review` intent
  renders instead as an approval card with Approve / Refuse / Chat about it.
- Approval cards, Toast banners, HoverCard click-to-copy.

### 2.3 Composer input

- `/` and `@` trigger pipeline: caret detection with word-boundary rules, a
  grouped candidate menu, fuzzy ordered-subsequence matching, keyboard arbitration.
- Three command dispatch kinds — `execute`, `popupSelect`, `leadingInput`. A
  client plugin may also **decorate** an existing host command, replacing only
  its bare invocation with a picker (this is how `/permission` becomes a menu).
- `/` skill source listing user-invocable skills, with a user-only marker for
  `disable-model-invocation` entries.
- `@` subagent reference source.
- **Model seat** — a two-level Model/Effort menu, models grouped by provider,
  effort levels supplied by the exact selected model rather than the provider.
- Plan-mode chip and permission-preset picker.
- Drafts survive workspace switches. With no workspace, or with no adapter
  serving the session's route, the composer goes inert and states which.
- **Two send modes**: `queue` and `steer`. `session.updateQueue` can edit,
  remove, or convert one pending queued message into a steer.

### 2.4 Session header actions

- **Background jobs** (`ui-jobs`) — a popover listing this session's jobs, badged
  with running plus stopping counts, live rows before settled rows, a
  once-per-second elapsed clock that freezes at completion, and the producer's
  own detail text replacing the generic status word. Rows are read-only today.
- **Subagent catalog tree** (`ui-subagent`) — a lazily expanded multi-level tree
  with per-child token totals and active-turn duration, full keyboard
  navigation, opening any depth's transcript. A continuable child keeps an
  ordinary composer whose prompts join the child's FIFO inbox, with an
  independent Stop; a one-shot child gets a read-only completed-record composer.

### 2.5 Settings

- **General** — language, appearance (light / dark / system), default permission
  preset, default agent preset, and an **Open configuration file** action that
  hands the resolved document to a native text editor.
- **Models** — joins three domains (`llm.providers`, `settings.describe`,
  `credentials.describe`) into one snapshot. API keys are stored **write-only**
  through `credentials.set` so `settings.yaml` never carries a key value. Per
  route: model list, `baseURL`, and for hand-declared routes a display name and
  API protocol. `llm.discoverModels` asks a provider what it actually serves.
  Green and red dots mark confirmed-configured and confirmed-missing key
  references; a Custom tag marks routes the owning adapter ships nothing for.
- **Plugins** — *Plugin configuration* renders one expandable card per host
  plugin whose settings a user owns (today `bash`, `agent-loop`,
  `web-search-deepseek`), each field marking a user override and offering a
  reset. *Plugin list* is a read-only searchable catalog of every Loader entry
  with its effective configuration and cordis status.
- **Onboarding** — a versioned internal-testing notice, then a conditional
  DeepSeek credential step that renders only when no reachable provider exists.
- **Agent presets** — roster management (copy, delete, set default, open the
  preset's own files) plus a new-session chip staging the next session's preset.

### 2.6 Everything else

`/export` session-log download; `/goal` creation with a GoalBar offering edit,
pause, resume, and clear; schedule tools for `after_seconds`, absolute `at`, and
`every_seconds` reminders; Code Mode (`DSH_TOOLS_MODE=native|code|both`); the
tool-card family (terminal/pwsh, diff, read, search, web, cordis, workflow run);
a design-token theme system with a server-injected pre-paint theme bootstrap;
en/zh localization throughout.

## 3. What our DeepSeek integration covers, and what it omits

### 3.1 Current coverage

`crates/agent_utils/src/deepseek/` is roughly 4,600 lines:

- One shared `dsh web` host process, address discovered from its stdout line,
  process-tree teardown when the last tab closes, version range check.
- RPC: `session.create/list/history/prompt/cancel/models/selectModel`,
  `skill.list`, `subagent.list/history/interrupt`, and the gateway's
  `commands/list` and `commands/execute`.
- Events: `user/message`, `assistant/chunk`, `assistant/message`, `tool/call`,
  `tool/result`, `turn/start`, `turn/end`, `approval/requested`,
  `approval/resolved`, `question/requested`, `question/resolved`,
  `compaction/start`, `compaction/summary`, `compaction/end`, `todo/write`,
  `llm/retry`, `llm/retry-started`, `stream/error`, `host/agent-error`.
- Projections: `tokenUsage`, `contextPressure`, `contextBreakdown`, `permissions`.
- `tool-workflow/*` folded into the existing workflow-run view.

### 3.2 Tier 1 — the RPC already exists, only Rust is missing

All of these are now implemented except image attachments.

| Gap | Method or projection | How it landed |
| --- | --- | --- |
| **Steering a running turn** | `session.prompt` with `mode: 'steer'` | A prompt sent while a turn runs steers; an idle one queues. The pane already modelled `SendOutcome::Steered` for Codex and Claude, so the adapter now answers with it. |
| **Queue visibility and editing** | `session/queue` frames, `session.updateQueue` | The pending inbox is mirrored into the composer's queued-prompt rows, each with a remove control. A row this side queued optimistically carries no control until the harness names it. |
| **Session rename** | `session.rename` | `/rename <title>`, echoing the title the harness normalized to. |
| **Session fork** | `session.fork` | `/fork`, idle-only, moving the tab into the branch through the existing resume path. |
| **Session content search** | `session.search` | `/find <text>`, joined against `session.list` for display data, replacing the recent-conversations strip with ranked rows and excerpts. |
| **Image attachments** | `session.attachment`, `imageLimits` projection | Not implemented. |
| **`plan` / `goal` / `sessionStats` projections** | Same `session/projection` stream we already read | Plan mode and the goal render as one strip above the composer; whole-log counters and model/tool wall time join the context hover card. |

The `todos` projection was deliberately left out: the adapter's `todo/write`
mapping already produces the checklist row that `Item::task_tally` counts, so
the projection would be a second source for a value that is already correct.

Two behaviors this exposed, both fixed with the work:

- **A steered message could have vanished from the transcript.** The pane only
  showed a queued prompt when the harness's echo matched the head of its own
  list, and the authoritative queue snapshot removes the entry on claim. Either
  arrival order now publishes the row, and whichever loses the race finds
  nothing left to publish.
- **A backend that reports its own queue must not also be guessed at.** The
  existing rule — assistant output means the steered message landed — would show
  a prompt as sent while the harness still lists it as waiting, so it is now
  gated on a `reports_pending_queue` capability.

### 3.3 Tier 2 — needs a new panel

- **Subagent catalog navigation.** We read subagent transcripts only as workflow
  members (`nmt/subagent-transcript`). There is no hierarchical tree, no
  per-child token or duration accounting, and we never call `subagent.prompt`,
  so a continuable child cannot be continued from our tab.
- **Deliverables row** plus inline file-reference links in closing prose.
- **Background job list.** Driven by `session/jobs` frames we do not subscribe to.
- **Trajectory timeline.** We hold every event already; only the view is missing.

### 3.4 Tier 3 — likely out of scope

`workspace.*`, `agentPreset.*`, `settings.*`, `credentials.*`, `llm.providers`,
`llm.models`, `llm.discoverModels`, `host.pickDirectory/listDirectory/openPath`,
and `pluginInventory/list` are dsh's own configuration and project-management
panels. The archived change that added the DeepSeek tab settled on "the user
installs dsh, NiumaTerm supplies only the GUI", and rebuilding a provider and
credential editor contradicts that. The cheap alternative is one action in the
DeepSeek tab that opens the running host's own Web UI — we already know its
address.

### 3.5 A defect this survey inherited and disproved

`agent-harness-next-steps.md` records that `Item::task_tally` recognizes only
Claude's `TodoWrite` and that DeepSeek's `todo_write` therefore leaves the plan
tally dark. That is no longer true: the adapter's `map_todo_write` normalizes
the item to `kind: "TodoWrite"` and emits the same `- [x]` checklist the tally
parses, so DeepSeek tabs already count their plan. The note is stale.

## 4. Features no NiumaTerm tab has, buildable without the harness

Ranked by value over cost. None of these depend on dsh; the data is already in
our process for all three harnesses.

1. **Trajectory timeline view.** We capture the full event stream with
   timestamps for Codex and Claude too. What is missing is one tab: a
   selectable per-record ledger with a token/duration/IO inspector, and a
   timeline that splits each assistant span into TTFT and decoding. All three
   vendor CLIs lack this, so it is the most differentiating item on the list.
2. **Cross-session content search.** Claude transcripts are on-disk JSONL that
   we already read (`filesystem_session_history`); Codex is comparable. A
   content grep with snippet results and jump-to-event is entirely local.
3. **Prompt queue.** Queue several prompts, auto-send the next when a turn ends,
   allow delete and reorder. Pure pane state; needs no protocol support and
   works identically for all three harnesses.
4. **Deliverables row.** Fold each turn's file-write and diff tool calls into a
   path list, render as chips, open through the existing file opener. Codex and
   Claude tool calls both carry the paths.
5. **Image attachments.** Both Claude Code and Codex CLI accept image input; our
   composer has no path to it. Pasting a screenshot is a natural terminal gesture.
6. **Per-message rating with a note.** Stored in `local_state.toml`, never in the
   transcript. Low cost; useful for reviewing which answers were worth keeping.
7. **Session stats strip.** Turn and step counts above the composer. We already
   own the usage and context panels, so this is nearly free.

### Deliberately not recommended

- **Workspace grouping** — NiumaTerm's tabs already organize sessions; a second
  grouping layer duplicates that.
- **Goal bar** — Claude and Codex have no durable goal mechanism, so building one
  means silently injecting prompt content each turn. The behavior would be
  opaque to the user.
- **Agent preset roster** — our profile system already covers this ground.

## 5. Open questions

Two of the original five were answered by building §3.2, and are recorded here
with what was decided.

1. **Steering as a mode toggle, or by default?** *Decided: by default.* Sending
   during a running turn steers, matching what the Codex and Claude tabs already
   do, so one gesture means the same thing in all three. dsh's own browser UI
   defaults to queueing and puts steering behind a modifier, which is the
   opposite choice; if the default turns out to be wrong here, the modifier is
   where it would go.
2. **Is the queue per-harness or pane-level?** *Decided: per-harness, mirrored.*
   The pane's own optimistic list stays for backends that report nothing, and a
   backend that publishes its inbox replaces it, which is what the new
   `reports_pending_queue` capability selects between.
3. Is the Trajectory view a third tab in the pane, or a detachable panel? Our
   pane has no view ring today.
4. How far does "the user installs dsh themselves" extend — does it also rule
   out surfacing dsh's Models page state read-only, so a missing API key is
   diagnosable without leaving the terminal?
5. `session.search` is server-side for dsh but would be local for Codex and
   Claude. One search surface with two backends, or two separate features? The
   `/find` command is DeepSeek-only today for exactly this reason.
