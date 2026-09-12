# Agent Session Ownership

Updated: 2026-09-12

## Implemented ownership

`nmt_agent::session::controller::SessionController` owns provider runtime,
delivery, restore, controls, input requests, branches, commands, child state,
and workflows. Its shared `ConversationState` owns indexed entries, turn
outcomes, timing, usage, compaction, and accepted immutable image references.
The core crate has no GPUI dependency.

`app::agent_tab::execution::AgentSession` owns asynchronous execution. An ordinary
tab retains its `SessionOwner`; `AgentPane::attach` creates presentation against
that owner. Attaching does not start or resume a provider. Replacing a composer
invalidates the preceding command binding. A detached pane cannot send through
stale callbacks. Read-only transcript readers share content and cannot obtain a
command binding.

The registry stores weak session entities under runtime `SessionId` values.
Registration precedes startup; logical close removes the entry immediately.
Application routes are separate from GPUI presentation entity identifiers.
Installation maintenance discovers sessions through this registry, including
sessions without a renderer, once per session.

## Operations and lifetime

- `AgentSession::create` establishes logical ownership and registration;
  `SessionOwner::start` starts an unstarted owner once.
- Controller submission, event application, readiness, replay, interruption,
  input response, and failure operations update related state synchronously.
  Accepted messages enter canonical content before presentation callbacks.
- The execution host owns startup, inbox consumption, checkpoint and resume
  reads, optional-question expiration, and child/workflow refresh tasks.
- Batches retain the 64-message limit and two-millisecond processing slice.
  Only adjacent compatible deltas merge. Each event checks backend generation;
  presentation delivery also checks backend and command-binding generations.
- Lifecycle events originate in the execution host. Completion publication is
  deduplicated by backend generation and turn, independent of reader count.
- `SessionOwner::close` invalidates commands and pending startup, removes the
  registry entry, clears retained content, and releases its backend with bounded
  shutdown. Backend release precedes removal of that session's scratch directory.
  Ordinary view destruction does not perform this cleanup.
- Maintenance readiness, wait, stop, suspend, and restoration operate on session
  state. Closed owners cannot restart. Replacement retains the outgoing shared
  host reference until the incoming backend has acquired its reference.
- Configured and active workspaces remain separate. Existing profile resolution,
  protected credential handling, input-history scope, and saved-tab formats are
  preserved; registry entries do not add resolved credential fields.

## Presentation and detail readers

`TranscriptView` borrows the same conversation allocation. Each reader retains
its own layout, folding, selection, scroll position, code cache, image previews,
and typewriter state. A bounded 64-entry revision log identifies invalidation
ranges. Readers that miss that log rebuild derived state from current shared
content. Clear and front retention advance generation so old row indices cannot
reuse disclosure or image state for different entries.

Streaming appends to indexed text in place. Production child/workflow detail
views attach shared conversation state instead of cloning the item list.
`show_items` remains only in test fixtures. GPUI image decoding belongs to the
reader; accepted immutable bytes belong to the session. Rejected input retains
its unsent images in the composer.

Child readers register interest by backend generation and child identity.
Workflow readers retain independent member keys. One session-owned refresh task
per detail family preserves the one-second cadence and sequential reads; dropping
reader interest stops unnecessary refresh. Parent and generation checks reject
late data from a different conversation. Detail content never replaces root
conversation content.

## Current source map

| Path | Responsibility |
| --- | --- |
| `crates/agent/src/session/controller/` | Domain transitions and canonical event admission |
| `crates/agent/src/transcript/conversation/` | Shared content, images, revisions, and retention |
| `crates/agent/src/transcript/turns.rs` | Live and retained turn accounting |
| `crates/agent/src/session/children.rs` and `workflows.rs` | Scoped retained detail data |
| `crates/app/src/agent_tab/execution/` | Owner, registry, startup, inbox, lifecycle, recovery, and filesystem work |
| `crates/app/src/agent_tab/session/` | Composer attachment, guarded commands, and presentation of outcomes |
| `crates/app/src/agent_tab/transcript/` | Independent rendering and presentation caches |
| `crates/app/src/agent_tab/workflows.rs` | Selected workflow reader and visibility interest |
| `crates/app/src/ui/shell/tab_surface.rs` | Ordinary tab retains owner and current pane |
| `crates/app/src/ui/shell/agent_notifications.rs` | Session subscriptions, preferences, and current-tab navigation |
| `crates/app/src/agent_updates/transaction.rs` | Registry-based installation participants |
| `crates/app/src/ui/background_tasks/` and `ui/workflows/` | Shared-content detail presentation |
| `crates/app/src/ui/shell/tabs_open.rs` and `ui/persistence.rs` | Create/start once and preserve saved-tab interpretation |

Before migration, pane startup owned the inbox and provider start; pane event
callbacks completed readiness and published accepted content; transcript views
owned entries; pane destruction removed scratch files; maintenance discovered
panes; detail views copied source item lists. Each execution responsibility now
has the session owner identified above. View timers retained for elapsed labels,
Git status, scrolling, and animation do not consume provider events.

## Observed verification

The pre-change baseline passed six controller tests and 42 UI session tests.
Final runs on 2026-09-11 passed 560 core tests, 229 UI tests (one existing ignored
performance test), and 227 application tests. Commands:

```text
cargo test -p nmt_agent -p nmt_agent_ui --lib
cargo test -p app --bin NiumaTerm
cargo clippy -p nmt_agent -p nmt_agent_ui -p app --all-targets --no-deps -- -D clippy::absolute_paths -D clippy::complexity -D clippy::perf -D clippy::style
```

Changed Rust files were formatted with the pinned nightly toolchain. The strict
clippy pass succeeded for all three affected workspace members.

The deterministic execution test retains one provider identity and generation
while all panes detach, processes output and approval, reattaches, accepts one
answer despite repeated submission, publishes one completion despite duplicate
completion events, and rejects sends and maintenance restoration after close.
Accepted image bytes and a delayed-read scratch file survive view replacement;
close releases the backend, retained bytes, registry entry, and scratch directory.

The long-content reader test shares 1,000 large reasoning entries and one live
reply across two readers. It applies 70 deltas while one reader misses updates,
then verifies current text, independent folding, shared allocation, and unchanged
storage for retained middle text. Front retention also clears obsolete disclosure
state. This is direct storage evidence against full-conversation copying on each
stream update; it is not a frame-time benchmark.

Existing focused cases cover readiness/settings precedence, repeated readiness,
replay and history, supported branch operations, busy input, rejected drafts,
interaction recovery, scoped child/workflow results, and shared-host replacement.
Core event cases reject obsolete generations and retain consistent delivery and
content without requiring rendering. Application regressions also passed.

An isolated `NiumaTerm.exe --testing` launch used a local deterministic provider
and separate test configuration. It received asynchronous mixed reasoning and
reply output. The user confirmed normal display and explicitly confirmed long
conversation scrolling, folding, text selection, images, child/workflow details,
and view switching. Captured blank images were a screenshot problem; they are
not evidence of a rendering defect. Native results are user-observed; identity,
resource release, and absence of full-content copies are supported by automated
checks above. No Team UI or live-provider end-to-end discussion was exercised.

## OpenSpec and dependent work

The change includes an `agent-session-ownership` delta covering the implemented
lifetime, state publication, command access, shared content, attachments, cleanup,
detail refresh, and application discovery guarantees. At the user's request,
this replaces the earlier `skip_specs: true` configuration and resolves the
missing-delta validation error. Both `decouple-agent-session-ownership` and
`add-agent-team-tab` passed `openspec validate <change> --strict` after this update.

`add-agent-team-tab` can reuse the verified ordinary-session ownership work.
Runtime session identity and command binding do not implement durable Team
ownership transfer. Room state, scheduling, permissions, persistence, transfer,
and Team acceptance remain unimplemented and paused.
