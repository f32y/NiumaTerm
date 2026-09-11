## Context

See `proposal.md` for motivation and scope. This restructuring preserves ordinary Agent behavior. The `agent-session-ownership` delta specifies the implemented ownership guarantees; existing specifications remain authoritative for the other capabilities.

The current code already separates provider state into `nmt_agent::session::controller::SessionController`. It is an ordinary Rust value owned by `AgentPane`, and its state groups are publicly accessible. `crates/agent_ui/src/session/startup/mod.rs` starts providers, receives bounded event batches, and delivers them through a weak pane reference. `session/events.rs` completes readiness, applies replay, confirms pending messages, and updates the transcript. These operations still need a pane even though they affect execution.

`TranscriptView` owns `TranscriptContent<EntryPresentation>` together with row caches, folding, scroll state, and animation. Entry presentation also holds sent images. `AgentPane::drop` removes attachment scratch files. Update readiness asks the transcript whether the conversation is empty or compacting, and `crates/app/src/agent_updates/transaction.rs` discovers participants by traversing panes. Activity and notification subscriptions also identify panes.

`docs/research/agent-session-controller.md` describes the existing extraction and these remaining concerns. The six controller tests already in the working tree are useful baseline coverage, not evidence that independent session hosting is implemented. The Team planning documents use older crate names; this design uses `crates/agent` and `crates/agent_ui`.

## Goals / Non-Goals

**Goals:**

- One logical command owner, one provider event consumer, and one mutable content owner per live session.
- Session progress, readiness, recovery, and accepted-message accounting work without an attached `AgentPane` or `TranscriptView`.
- Views read retained content incrementally and keep independent presentation state.
- Application consumers discover and address sessions without depending on a specific presentation entity.
- Ordinary Agent behavior remains verifiable throughout the migration.

**Non-Goals:**

- Team surfaces, live Team transfer actions, discussion control, summary generation, permission ceilings, and durable Team ownership records.
- A new provider abstraction, another copy of `SessionController`, a general event framework, or changes to provider protocols.
- Keeping ordinary conversations running after the user closes their owning tab, introducing a background-session product feature, or changing saved tab interpretation.
- Redesigning transcript appearance, changing workflow refresh policy, or reorganizing unrelated files.

## Decisions

### 1. Separate pure state, application execution, and presentation

Keep provider operations, the existing controller, and conversation data in `nmt_agent`, without GPUI dependencies. Add `AgentSession` in `nmt_agent_ui` as the GPUI execution host. It owns the controller, conversation state, effective launch/workspace snapshot, attachment resources, and asynchronous work. It has no composer, focus handle, list state, or renderer.

`AgentPane` becomes the ordinary conversation presentation. It receives access to an existing session and supplies user actions through session operations. The normal new-tab path creates a session first and attaches the pane. A separate attach path accepts an existing session without starting or resuming another provider.

```text
Logical tab owner --> AgentSession --> SessionController --> Backend
                           |
                           +--> ConversationState
                           |
                           +--> Committed changes --> AgentPane / TranscriptView

Session registry --> weak lookup of AgentSession
```

The future Team owner can use the same host after this change. No hidden ordinary pane is created to keep a session running. A second state implementation for Team would duplicate provider behavior; putting GPUI into the core controller would couple deterministic execution tests to the presentation runtime.

### 2. Distinguish ownership, observation, and addressing

Give each application session a stable `SessionId` for its lifetime, separate from provider conversation identity and backend generation. The shell's logical tab entry holds the owning session handle. Presentation attachment does not grant another logical owner. Read handles and registry entries provide weak lookup or borrowed state; they do not expose unrestricted mutable controller access.

Only the current logical owner grants command access to its active composer. Bind asynchronous UI commands to the session ID, current command binding generation, and, where needed, backend generation and request identity. Detaching or replacing the command binding invalidates old callbacks. Multiple readers can inspect one session, but a detail renderer cannot send prompts or answer another owner's requests merely because it can read content.

Rebinding presentation retains the same session and backend generation. Starting a different provider conversation advances the existing backend generation and invalidates conversation-scoped work. These are distinct operations. This change tests rebinding within a logical owner; durable transfer between ordinary tabs and Teams remains downstream.

Closing the logical owner first closes command admission, invalidates pending callbacks, and begins bounded cleanup. Presentation replacement alone does not close the session. The registry removes closed entries and keeps no strong session reference. Cleanup cannot depend on dropping every observer: stale observers must not keep a backend running after its owner closes it.

This explicit lifecycle avoids both accidental shutdown during view replacement and leaked providers retained by a global registry.

### 3. Commit execution and content together before notifying readers

Move startup, inbox consumption, provider event reduction, branch/restore readiness completion, and accepted-message publication into the session path. Retain bounded inbox slices and adjacent-delta merging; input requests, errors, and lifecycle events still flush preceding text in order.

For each admitted event:

1. Verify the backend generation, including after another event in the same batch changes it.
2. Apply controller transitions and canonical content changes in the same synchronous session update.
3. Finish required session work, including replay ordering, settings selection, restored questions, naming, and command queue advancement.
4. Publish content invalidation and semantic activity only after the new session state is consistent.

The current `prepare_ready` / `finish_ready` sequence remains ordered internally. The host supplies resolved profile and remembered settings, applies any selected replay, and finishes readiness before processing the next event. No view callback is needed between those steps. Provider-specific repeated readiness must still preserve a running turn and current settings.

Visibility for unanswered-prompt recovery means admitted non-hidden output, not pixels painted or typewriter progress. Echo suppression, queued message confirmation, turn numbering, terminal outcome, and compaction state are computed without a renderer. Preserve each provider's existing busy-input behavior.

Session commands return actual admission or completion results. The view applies draft clearing, focus, scrolling, feedback, and other user reactions. Deferred UI notifications carry session/content generation and revision so events queued before replacement cannot affect a newer view. Semantic lifecycle notifications are emitted once by the session; attaching another reader does not replay old completion notifications.

### 4. Give retained conversation data one owner

Build `ConversationState` around the existing indexed `TranscriptContent`. Retain entries, turn identity and observed outcomes, timestamps, usage/context readings, compaction status, and references to sent attachments. Keep absent provider measurements absent. Store semantic timestamps and durations; views format labels and schedule display-only timers.

`TranscriptView` borrows canonical entries through a content read interface. It owns derived rows, syntax and layout caches, measured heights, folding, selection, scrolling, preview state, and typewriter progress. A mounted view preserves those states across ordinary session updates and maintenance. A newly attached view initializes from retained content without replaying provider work or typing old text again.

Changes identify a content generation, revision, and affected entry/turn region. Combine invalidation ranges across a batch instead of copying a full transcript per event. A reader that missed revisions rebuilds its derived index from current borrowed content; it does not require an unbounded event history. Replacements invalidate old item references, while append/merge preserves indices within a content generation. Duplicate provider item IDs continue to follow the existing compatible-item lookup rules.

The root conversation and each child/workflow conversation remain separate content sources. Reuse the rendering path, but retain child/workflow data once and keep its parent/provider identity on reads and refresh results. Do not copy child output into the root transcript or rebuild full child content for every delta.

### 5. Separate pending attachments from accepted resources

The composer owns unsent images, placeholders, annotations, and its editable draft. Submission borrows or shares their immutable bytes. On acceptance, the session retains the attachment resource and associates it with the accepted message before the view clears its pending state. Rejection leaves the original draft and images available.

Use session-owned attachment IDs and immutable encoded resources; GPUI image handles and preview caches belong to rendering. Provider scratch files belong to the session operation that needs them, not to `AgentPane::drop`. Keep the files for their required live lifetime and clean them during explicit session cleanup without affecting another session's directory.

This introduces no durable image store or new resume guarantee. Existing replay with unavailable historical images remains supported; durable Team attachments belong to the Team change.

### 6. Move application consumers to the session registry

Add a registry with weak session lookup and stable route identity. Register before startup can publish events, and unregister on logical close. In `crates/app`, map session identity to the current ordinary tab destination. Notification preferences and navigation remain application responsibilities; renderers do not each register another lifecycle consumer.

Installation coordination discovers sessions through the registry, deduplicates by session identity, and derives installation membership from each session's effective launch configuration. A maintenance transaction may retain its participant handles for the transaction lifetime, but explicit close still prevents a participant from restarting after its owner has closed. Release those temporary handles at the terminal outcome.

Compute readiness from session state: retained conversation existence, running/accepted work, compaction, interactions, pending provider operations, and the backend's existing active-work reporting. Preserve wait-until-idle, explicitly approved stop-now, recovery identity validation, and independent restoration behavior. A view detaching or attaching does not change readiness or duplicate maintenance registration.

Child/workflow panels issue scoped reads and commands through session handles. Views report whether detail is visible; the session host owns the corresponding asynchronous task. Keep current visibility-driven disk-refresh cadence and avoid overlapping refresh loops when two readers attach. Required readiness uses session/backend activity, not whether a panel happens to be open. Late filesystem results retain generation and parent checks.

### 7. Preserve ordinary lifecycle and configuration behavior

Keep profile resolution and remembered-setting precedence as currently implemented. The configured workspace and the running conversation's active workspace remain distinct snapshots; presentation changes cannot widen roots or change input-history scope. Resolve credentials through existing protected profile handling and expose no resolved secrets through registry entries, content notifications, or diagnostics.

Closing an ordinary tab still closes its session. Switching tabs keeps it running. Closing one session releases only its backend/shared-host reference. An in-place conversation replacement retains the outgoing host reference until the replacement has acquired the reference it needs. Simultaneous compatible Codex starts and recovery continue to share one host; incompatible configurations retain their current rejection behavior.

Saved terminal and Agent tab formats remain unchanged. Stable application session IDs and view binding generations are runtime details in this change. Durable ownership generations and restart reconciliation for Team transfers are deliberately handled by the dependent change.

## Risks / Trade-offs

- Moving event application changes ordering -> Keep controller/content mutation synchronous, test repeated readiness and mixed text/interaction batches, and filter stale generations before any publication.
- A new host becomes a field-forwarding wrapper -> Expose operations for transitions that must advance together and borrowed read views; remove production callers' direct multi-field mutation as they migrate.
- Content extraction changes rendering cost -> Keep indexed in-place updates, bounded invalidation, virtualization, and independent render caches; verify a long mixed transcript and overlapping readers without full-content copies per delta.
- Session handles create retention cycles -> Use weak registry/subscriber links, explicit logical close, bounded orphan cleanup, and release tests with multiple shared-host users.
- Attachment extraction loses accepted images or clears rejected drafts -> Test resource ownership at acceptance, view replacement, delayed provider reads, rejection, and final owner close.
- Display timers become execution dependencies -> Keep presentation timers in views; channel and timer callbacks notify subscribed GPUI entities so a quiet window wakes. Validate this in an isolated launch because the GPUI harness does not model the frame pump.
- Child/workflow refresh grows when readers multiply -> Keep one session-owned refresh loop with scoped reader interest and preserve existing cadence and unavailable states.

## Migration Plan

1. Establish focused baseline scenarios and identify all production controller/content mutations that still originate in views. Reuse current controller and ordinary-tab tests.
2. Extract conversation data and semantic event application, adapting the existing ordinary pane immediately so there is one authoritative path at each stage.
3. Introduce the session host, ownership handles, and registry; migrate startup, recovery, and attachment lifetime together with their callers. Remove retired pane-owned workers as their replacements become active.
4. Move transcript and child/workflow presentation to borrowed session content; preserve mounted view state and existing provider-specific controls.
5. Migrate installation coordination, activity, notification routing, and close/shutdown consumers to session handles. Remove remaining duplicate subscriptions and pane-discovery dependencies.
6. Verify ordinary behavior, presentation replacement without another provider session, and bounded cleanup. Run the relevant Rust checks and isolated `--testing` UI validation, then update the session ownership documentation with observed results.
7. After this change's acceptance, reconcile the dependent Team plan: reference this prerequisite, reuse verified work for its session-extraction tasks, and leave Team-specific tasks incomplete. This planning run does not alter that change's progress or implement Team behavior.

No persisted format migration is needed. Keep implementation in reviewable stages that preserve a single active path. If a stage must be reverted, revert its implementation and dependent integration together; do not leave both ownership paths running. Existing provider histories and tab records remain usable.

Acceptance is complete when deterministic tests can detach all renderers while the logical owner remains, process output and interactions, attach a new renderer, and observe the same provider identity and retained conversation without a second send or event consumer. Logical owner close must still prevent further execution and release only its own backend resources. Ordinary-tab and isolated UI scenarios must also pass before Team implementation resumes.
