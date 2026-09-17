# Unified Agent Backend Interfaces

Date: 2026-09-17

Status: recommended design. Landed: step 1, the submission merge of step 2,
and the task-history read of step 3. Open: delivery events, steps 4 and 5.

This review covers the current Codex, Claude Code, and DeepSeek Harness
integrations, including the uncommitted queued-message corrections. It uses
local source and existing behavioral evidence. No additional live model calls
or application launches were performed for this review.

## Recommendation

Keep the `Backend` enum as the dispatcher for a live conversation. Move
provider rules out of shared code by letting every operation report what
happened as a typed result or a normalized event. Hand disk reads to the
execution owner as opaque jobs the session prepares. Reserve a trait object for
a stateless reader that has more than one implementation, as `WorkflowSource`
does.

The useful outcome is that a caller can submit, stop, answer an interaction, or
change settings without understanding how a particular provider performs that
operation. The measured problem is rule placement: provider arms occur in one
file, `session/backend/mod.rs`, while provider rules occur in `delivery.rs`,
`restore.rs`, the controller, and eight application files. Changing the dispatch
mechanism leaves those rules where they are. Changing what operations return
moves them.

An earlier draft of this document recommended a core `AgentBackend` trait with
optional interfaces reached through accessors. The section "Why the live
conversation stays an enum" records why that was set aside.

## Current evidence

- `crates/agent/src/session/backend/mod.rs` has 885 lines and 45 public or
  restricted-public associated methods: two creation functions and 43 instance
  operations. The count excludes `RecoveryIdentity::new` and private methods.
- Its responsibilities include dispatch, image conversion, initial title
  ordering, restoration, background tasks, workflows, Team integration, and
  test behavior.
- `session/capabilities.rs` has 16 fields. User abilities such as file rewind
  sit beside implementation choices such as `filesystem_session_history`,
  `repeats_ready_during_init`, and `model_selection_is_a_request`.
- `session/delivery.rs` selects its confirmation policy by `AgentKind`.
  `session/restore.rs` selects resume behavior by kind and directly invokes
  Claude history readers. `session/history.rs` also imports Claude readers.
- `app/agent_tab/execution/mod.rs` imports Claude session readers. Its inbox
  carries `serde_json::Value`, and `execution/inbox/mod.rs` interprets a raw
  output-failure marker.
- `send_user_message` and `send_user_message_with_title` duplicate attachment
  preparation. `rename_session` and `rename_conversation` expose separate title
  paths with different provider coverage and result types.
- Several optional methods return `Ok(())`, `false`, or nothing for other
  providers. Some mean deferred application elsewhere; others mean unsupported.
  The caller must already know which interpretation applies.

Two further measurements decide the direction:

- Provider arms such as `Backend::Codex(..)` occur in no file other than
  `session/backend/mod.rs`. Dispatch is already contained.
- Of the 16 capability fields, 9 have a single reader outside tests and none has
  more than 3. Four of them describe how a provider transports a setting or a
  resolution rather than what the user can do: `model_selection_is_a_request`,
  `approval_selection_is_a_command`, `async_approval_resolution`, and
  `filesystem_session_history`. Their readers are the controller, the backend
  itself, and `app/agent_tab/mod.rs`.

There is also substantial structure worth preserving: shared `chat::Event` and
`Item`, `SessionController`, `SessionRuntime`, retained conversation storage,
guarded command bindings, and a GUI-independent agent crate. The proposed
changes fit below that controller rather than replacing it.

The August research in `agent-harness-refactor.md` recommended retaining an enum
when there were 18 methods and no DSH adapter. Its counts no longer describe the
current implementation, and its objection about generic arguments is a design
choice that borrowed slices and a separate creation function would answer. Its
central objection still holds: a trait whose methods are mostly defaulted
answers for other providers is a capability table in another form, and it
trades the exhaustive `match` checked by the compiler for a silent default.

This recommendation preserves the accepted ownership of `AgentKind` and profile
serialization in ADR 0002. It does not introduce another identity enum.

## Differences the adapters must retain

| Concern | Codex | Claude Code | DSH |
| --- | --- | --- | --- |
| Connection | Shared app-server host, registered conversation | Owned JSON-line process | Shared HTTP/WebSocket host, subscribed conversation |
| Busy submission | `turn/steer`; explicit closed-turn refusal can start a new turn | Write a user record to the running process | Submit in `steer` mode; host manages pending input |
| Input confirmation | User-message event | Replayed user-message event | Pending-inbox updates and user-message events |
| Queued withdrawal | Not exposed by this integration | Not exposed by this integration | Remove a pending item by provider ID |
| Images | Local file paths | Inline image bytes | Inline image bytes |
| Settings | Turn-start overrides | Launch defaults plus control requests before submission | Separate selection requests or commands |
| Resume | Reopen a thread on the host | Read history and restart with a session ID | Switch the live subscription; startup recovery remains incomplete |
| History and details | Host requests, including paging | Filesystem readers | Host requests and projections |

The current Claude implementation sends `set_model`, `set_permission_mode`, and
effort requests before user messages. Its startup model also influences the
initial prompt. The capability name `model_baked_into_launch` must not be read
as proof that runtime model switching is entirely unavailable.

DSH's live `resume_thread` exists, but `Backend::spawn` currently ignores the
recovery identity for DSH. Interface migration must expose that distinction and
preserve the current limitation until a separate behavioral change supplies
startup resume. It must not report a fresh conversation as successfully resumed.

Both Codex and DSH share hosts. Closing a conversation must release its own
registration and subscriptions without terminating a host used by another
conversation. A common process handle or common `kill_process` method would
model the wrong resource.

## Proposed ownership

```text
AgentPane / Team view
        |
SessionController + SessionRuntime
        |
Backend enum: typed results and normalized events
        |
Codex Session | Claude Session | DSH Session
        |
Provider protocol, host, process, files

Disk reads: opaque jobs prepared by the session,
run by the execution owner on a background thread
```

The controller continues to own canonical content, pending presentation state,
input recovery, desired settings, and admission of events for the current
generation. The execution owner continues to schedule work and wake the UI.
Each adapter owns protocol translation, provider input correlation, effective
settings, request tracking, and its host or process reference.

### 1. Results carry the provider rule

Every command-style operation reports what happened, and the caller decodes
that result with one `match`. The caller stops asking a capability field how
the provider would have performed the operation.

```rust
pub enum SettingsOutcome {
    /// The provider answered the request; the new value is in force.
    Effective,
    /// Nothing was sent; the value travels with the next submission.
    RidesNextSubmission,
    Refused { message: String },
}

pub enum ResumeOutcome {
    /// The live connection switched conversations; a replay follows.
    SwitchedInPlace,
    /// History has to be read and the process restarted with an identity.
    NeedsReplayRead,
    Rejected,
}
```

`select_model`, `select_approval`, and `select_agent_preset` return
`SettingsOutcome`. Today they return `Ok(())` for Codex and Claude, where the
meaning is "applied with the next submission", beside operations such as
`rename_conversation` that return `Unsupported` for the same two providers.
The controller paths that read `model_selection_is_a_request` and
`approval_selection_is_a_command` branch on the outcome instead, and both
fields are removed.

No adapter currently sends a settings request whose confirmation arrives
later, so there is no pending variant. Add one together with the first adapter
that needs it. After `Effective` or `Refused`, the selection held by the session
is the authority and the pickers are restored from it. After
`RidesNextSubmission` the recorded picks stay as chosen. Before this change,
calling the model operation on Codex or Claude would have replaced the picks
with the empty selection those sessions report, and only the capability check
at every call site prevented it.

`resume_thread` returns `ResumeOutcome`, which replaces the `AgentKind` match
in `restore.rs`. Recent Claude conversations are read from disk and stay listed
after a failed start, so a resume can be requested with no session running.
`ResumeOutcome::without_session(kind)` answers that case beside the enum, in
the one file that already owns provider arms. The approval path reads whether
resolution is deferred from the adapter arm, which removes
`async_approval_resolution`.

A provider command that changes a setting reports it the same way.
`SlashCommandOutcome::Completed` carries the permission preset the command put
in force. The DeepSeek adapter fills it for a successful `/permission` with an
argument, and the application remembers that preset without checking the
command name or the provider kind.

`send_user_message` and `send_user_message_with_title` merge into one `submit`
taking a `PromptRequest`. It groups a controller-assigned local submission ID,
text, a borrowed image slice, settings for the submission, an optional skill
reference, and optional initial-title intent. Image storage and provider title
ordering stay behind `submit`. Initial title generation remains conditional on
submission admission.

Settings reach an adapter by one route per provider. Codex reads them from the
`PromptRequest`. Claude sends its control requests inside `submit` from the same
request. DSH applies picks through the `select_*` operations when the user makes
them. `SettingsOutcome` tells the controller which of those happened, so the
controller never sends the same value twice.

### 2. Input delivery is reported as events

The controller assigns a local submission ID. Each adapter translates its own
evidence into normalized events for consumption, withdrawal, and known failure.
Provider queue IDs remain optional and separate. `MessageDelivery` then keeps a
single state machine, `MessageDelivery::new` stops taking an `AgentKind`, and
`QueuedPromptDelivery` is removed.

- Codex and Claude adapters keep an ordered record of accepted submissions and
  match user-message echoes against it by occurrence and order. The adapter is
  the right owner because it can tell a replayed or provider-generated user
  message from an echo of accepted input; shared code cannot.
- The DSH adapter maps pending-inbox item IDs to local IDs and reports removal
  from the inbox as consumption or withdrawal according to which request
  caused it.

A local ID cannot create evidence a provider does not supply. Repeated
identical prompts, opening echoes, provider-generated messages, and replay must
be covered by adapter tests against protocol fixtures before the old policy is
removed. Unknown delivery after a timeout must not trigger an automatic resend.

Retain the user's chosen busy-input behavior: try to append promptly, with
unconsumed work continuing later. A local queue that always waits for turn
completion would change the requested behavior and reduce steering usefulness.

`submit` returning `Steered` means the integration accepted the submission for
steering. It does not prove the current model request already contains that
input; the consumption event does.

### 3. The capability table answers questions asked before a session exists

The composer needs `skill_references` and `slash_skills_are_prompts` before any
backend is spawned, and the tab shows `multi_root_access` before launch. A
table keyed by `AgentKind` therefore has to remain; an accessor on a live
object cannot answer those readers. The table keeps the fields that describe
what the user can do.

Operations on a live conversation report `OperationError::Unsupported`, as
`rename_conversation` and `fork_conversation` already do. `remove_queued_prompt`
returning `false` and the settings operations returning `Ok(())` move to that
form or to `SettingsOutcome`. The table describes abilities, and a result
describes how one call ended; neither repeats the other. Team support keeps
its generation-bound verification and stays out of the static table.

### 4. Disk reads are opaque jobs prepared by the session

Rebuilding Claude child agents reads files, runs on a background thread, and
is scheduled by the execution owner. The session prepares the read and the
execution owner runs it:
`Backend::begin_task_restoration(cwd) -> Option<TaskHistoryRead>`,
`TaskHistoryRead::run() -> TaskHistory`, and
`Backend::finish_task_restoration(TaskHistory)`. A provider whose children
arrive over its connection returns `None`, so no read is scheduled for it.
Claude's `RestoredTask` type and `load_task_history` are now visible only
inside `nmt_agent`, and `app/agent_tab/execution/mod.rs` no longer imports the
Claude reader.

A shared reader trait is not warranted yet. Claude is the only provider that
reads history from disk, so a `dyn` reader would have one implementation.
`WorkflowSource` remains the model to follow once a second disk-backed or
independently readable source exists.

`filesystem_session_history` stays in the capability table. The pane loads the
recent-conversation list while it is being constructed, before any session
exists, so no live object can answer that reader. The same reasoning as the
previous section applies.

`composer/palette.rs` and `composer/branch/rewind.rs` still name
`ClaudeCheckpoint` and `FileRestoreAvailability`. These are data types of the
file-rewind ability, which one provider supplies; renaming them into neutral
types belongs with step 4. The Claude hook settings and the usage fetcher
remain direct imports in the application, because they describe real
application choices about one product.

### Close and shared hosts

Both Codex and DSH share hosts. Closing a conversation releases its own
registration and subscriptions and leaves the host alive for other users. A
common process handle or `kill_process` operation would model the wrong
resource. Current DSH shutdown returns success before Drop schedules remote
cleanup, and cleanup failures only produce diagnostics. Preserve those
diagnostics and do not interpret success as acknowledged remote cleanup.

### Protocol processing

Do not introduce a new asynchronous runtime. The existing callbacks,
session-owned processing, and typed event reduction already provide a usable
execution model.

Removing raw frames from the application inbox is a separate step. The intended
result is an opaque backend input type with the raw failure marker interpreted
inside `nmt_agent`. Preserve the 64-message and two-millisecond processing
budgets, adjacent-delta merging, immediate control event ordering, terminal
output-failure behavior, and explicit UI wakeups. Avoid eagerly processing all
queued frames before applying their control events.

## Why the live conversation stays an enum

- A live conversation is stateful, has a single owner, and has three
  first-party implementations. Provider arms are confined to one file.
- An exhaustive `match` turns a missing operation on a new provider into a
  compile error. A trait method with a default body turns it into a silent
  runtime answer.
- An accessor such as `pending_input_control() -> Option<&mut dyn ...>` answers
  the same question as a capability field, and it cannot serve readers that
  run before a session exists. Keeping it in agreement with the UI feature
  snapshot would mean maintaining two sources for one answer.
- Every `impl` for a first-party type lives in the file defining the type. Six
  optional interfaces per adapter would all land in the three `Session` files.
- The test backend is already isolated by `cfg`. Its arms shrink in proportion
  as the operations merge.

Revisit a trait when an adapter has to be supplied from outside this crate, or
when a fourth provider makes unsupported arms the majority again after the
changes above.

## Alternatives

| Option | Result |
| --- | --- |
| Keep the enum and only add methods | Smallest change; shared provider rules remain |
| Convert all 43 instance methods to one trait | Removes repeated dispatch but preserves mixed responsibilities and empty methods |
| Core trait plus optional interfaces | Replaces a contained dispatch; duplicates the capability table; loses exhaustive checking |
| Enum dispatch, typed results and events, opaque read jobs | Recommended; moves each provider rule to its adapter and keeps compile-time coverage |
| One large command enum and a new worker runtime | Hides dispatch but adds result routing, cancellation, and ordering work |

Retain `AgentKind` matches for identity, registration, profile serialization,
icons, installation management, provisional titles, and background-task keys,
where they describe real application choices.

## Migration sequence

1. Landed. `SettingsOutcome` and `ResumeOutcome` are returned from the
   settings and resume operations and decoded in the controller, `restore.rs`,
   and the application. `model_selection_is_a_request`,
   `approval_selection_is_a_command`, and `async_approval_resolution` are
   removed.
2. Partly landed. `PromptRequest` exists and `Backend::submit` replaces the two
   send operations. Open: add the local submission ID and the normalized
   delivery events, move echo and inbox correlation into the adapters, and
   remove `QueuedPromptDelivery`. This part rewrites the confirmation logic
   corrected in `f2026396`, whose behavior was established with live provider
   runs. Land it with the same live runs for all three providers; the unit
   fixtures alone do not show whether a provider echoes, replays, or
   republishes a given input.
3. Landed for task history as an opaque read job. Scheduling and stale-result
   admission stay with the execution owner.
4. Align the remaining live operations on `OperationError::Unsupported`, and
   unify the two title operations without losing their persistence and timing
   differences.
5. Hide raw input and output-failure decoding within the agent crate.

Each step preserves user-visible behavior and can ship alone.

## Verification and completion criteria

Reuse the existing protocol fixtures and the recent queued-message tests.
Preserve tests that protect different layers: a parser fixture and a controller
lifecycle test cover different failure modes.

Required scenarios include:

- Busy input is consumed once, including identical text, turn completion races,
  and explicit steering rejection. Interruption prevents a later restart.
- DSH withdrawal succeeds only for pending input and never appears as consumed
  input. The other two integrations report unavailable withdrawal honestly.
- A settings pick reaches each provider once: with the next submission for
  Codex, as a control request for Claude, as its own request for DSH. A refused
  pick restores the pickers to the actual selection of the provider.
- Rejected drafts keep their text and attachments. Delayed image reads survive
  view replacement and end when their session owner releases them.
- Approval and question submissions retain pending state until the expected
  resolution; failures remain retryable without duplicate answers.
- Restores preserve identity, settings, and transcript ordering. Stale replies,
  child data, and queued updates cannot enter a different conversation.
- Closing one Codex or DSH conversation leaves other shared-host users alive.
  Logical close invalidates commands and late startup results.
- Detaching and reattaching a pane does not restart its provider. A quiet UI
  wakes when new work arrives.

Completion is measured by ownership:

| Measure | Now | Target |
| --- | --- | --- |
| `AgentKind` decision sites under `session/`, outside tests | 8, now 7 | 3 |
| Capability fields | 16, now 13 | about 11 |
| `claude_code` imports in the application crate | 8, now 7 | 4 |
| `Backend` instance operations | 43, now 42 | about 25 |

The three remaining decision sites are the capability table, provisional title
generation, and background-task key construction. No performance improvement is
claimed without measurement.
