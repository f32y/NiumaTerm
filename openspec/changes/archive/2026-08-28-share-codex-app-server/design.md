## Context

See `proposal.md` for motivation. The current Codex adapter combines two responsibilities in `crates/agent_utils/src/codex/app_server/mod.rs`: `Session` owns a `JsonLineProcess`, and the same value owns one thread's turn, approval, history, command, skill, and descendant state. Fixed JSON-RPC IDs work only because every tab has its own process.

The DeepSeek adapter already demonstrates the desired lifetime shape. Its process host is retained weakly at application scope, each tab holds an `Arc<Host>`, and the final session release stops the process tree. Codex differs in three important ways:

- app-server uses one bidirectional JSON-RPC connection, so request IDs and server-request answers must be coordinated across every tab;
- the connection emits root and descendant thread activity from all loaded session trees, so routing must prove ownership before delivery;
- Codex profiles may select different app-server providers per thread, but provider API keys are read from the shared child environment.

The existing profile adapter already creates a stable provider ID and injects a provider definition into `thread/start` and `thread/resume`. It currently names one common API-key environment variable and launches the process in each workspace. Both assumptions must change before those profiles can safely share a host.

## Goals / Non-Goals

**Goals:**

- Keep exactly one live Codex app-server process for compatible tabs in one NiumaTerm process.
- Preserve the existing per-tab `Backend` behavior while moving process and transport state into a shared host.
- Support different models, gateway URLs, and API keys on different root threads.
- Prove response, server-request, root-thread, and descendant ownership before delivering activity.
- Preserve current history, skills, commands, compaction, background-task, reset, and update behavior.
- Make startup, shutdown, and recovery single-flight and observable without blocking the UI thread.

**Non-Goals:**

- Sharing an app-server across different NiumaTerm processes or machines.
- Running two incompatible Codex executables at the same time.
- Changing Codex's persisted thread format or copying rollout files.
- Discovering a separate model catalog from every custom gateway; app-server exposes model discovery at host scope.
- Keeping an interrupted model stream alive across an unexpected app-server exit.
- Exposing the shared app-server as a network listener.

## Decisions

### 1. Split the shared host from the tab session

Add a `host` child module under `codex/app_server`. `CodexHost` owns:

- the one `JsonLineProcess` and its Job Object lifetime;
- app-server initialization state and bounded startup diagnostics;
- a process-wide request ID allocator;
- registered session callbacks and request ownership;
- root, session-tree, and descendant ownership indexes;
- process-exit publication.

`Session` keeps all conversation-specific state that is currently in the adapter, including its root thread ID, active turn, approvals, history cursor, command bookkeeping, compaction state, skill refresh state, workspace snapshot, profile provider, and `CodexTasks` projection. It replaces `process: JsonLineProcess` with `host: Arc<CodexHost>` plus a stable session registration ID.

This follows the DSH ownership pattern while keeping protocol translation close to the session state it mutates. A single application-level actor holding every tab's mutable state was rejected because it would pull UI-owned conversation state into a central lock and make unrelated turns contend on one mutation path.

### 2. Retain one weak global host and make acquisition single-flight

Use one process-local shared slot containing a `Weak<CodexHost>`. `acquire` runs on the existing background start path, holds the startup gate across lookup and creation, removes a dead generation, and returns the live `Arc` when compatible. Concurrent first acquisitions therefore join one result instead of racing two child processes.

The host starts lazily and performs `initialize` plus `initialized` once before it accepts thread creation. A small host-owned startup channel receives the initialize response from the stdout reader. Startup has a bounded timeout and retains only a bounded stderr suffix. `JsonLineProcess` gains an explicit stdout-closed callback so the host does not depend on a tab callback being dropped to detect exit.

The shared slot is a single host rather than DSH's list of launch-keyed hosts. If a live host is incompatible, acquisition returns a profile error. When the last `Arc` is released, `CodexHost::drop` closes stdin and applies the existing bounded forced fallback; the weak slot then becomes empty.

Alternatives rejected:

- one host per exact profile launch would preserve current flexibility but would not meet the one-process goal;
- a strong application-global host would leave Codex running after the final tab closed;
- starting at application launch would pay startup cost even when Codex is unused.

### 3. Separate host launch identity from thread profile settings

Introduce an internal host launch snapshot containing the resolved executable, executable arguments, and process-level environment. It excludes the model, reasoning effort, workspace paths, generated provider definition, gateway URL, and provider credential name because those belong to a thread.

The first profile selects the live host launch identity. The app layer also supplies the compatible Codex profile catalog when starting that generation so the host environment can include every generated provider credential needed by later tabs. Distinct environment names with non-conflicting values are merged. Conflicting process-level settings or another executable make a profile incompatible and produce an error instead of a silent substitution.

Profile edits do not mutate a running child environment. If a new tab requires a value absent from the live generation, acquisition returns a restart-required error. After the current sessions release the host, the next acquisition builds a fresh generation from current settings.

This keeps the initial change deterministic. Automatically restarting active tabs on every settings save was rejected because it would need the same quiescence and recovery transaction as a binary update and could interrupt work for a settings draft.

### 4. Give each provider a distinct credential environment name

Derive the credential name from the existing stable provider ID, using an uppercase environment-safe prefix and identifier. Build the name and provider ID in one helper so collision checks cover both. `CodexProviderConfig` continues to carry only the environment name; the decrypted key value is separated into host bootstrap data.

At host creation, the app layer contributes the decrypted key for every compatible custom Codex profile. The provider object sent in `thread/start` or `thread/resume` contains the gateway URL and environment name, never the key. Default profiles contribute neither a generated provider nor a generated key and continue through normal Codex authentication.

Passing a bearer value in thread configuration was rejected because it would place credentials in JSON-RPC values and make accidental diagnostic disclosure more likely. Reusing one common API-key name was rejected because two profiles would race for one process value.

### 5. Route requests with global IDs and explicit purpose

Remove session-fixed request IDs. `CodexHost::request` allocates a monotonically increasing process-wide ID and records a `PendingRoute` before writing the JSON line. The route contains the session registration ID and a typed request purpose such as thread start, thread resume, model list, history page, skill list, command, name update, descendant query, or descendant read.

When a response or error arrives, the host removes its pending route and delivers a typed internal `RoutedMessage` to the owning callback. The session processes the request purpose instead of inferring meaning from numeric ranges. Dynamic context currently stored in maps, such as command name or requested thread title, moves into the request purpose where practical; session state that spans later notifications remains session-owned.

Server-to-client request IDs use a separate ownership map because JSON-RPC permits the two directions to choose IDs independently. The host reads `threadId` from approval and user-input requests, resolves the owning session tree, and sends the request only there. A response is written through the host with the original server request ID. Detaching a session interrupts its root before removing unresolved server-request ownership.

Recording ownership before the write prevents a fast local response from beating registration. Holding callbacks outside router locks prevents UI queue delivery from blocking other request routing.

### 6. Maintain root and descendant ownership indexes

After a successful `thread/start` or `thread/resume`, the host registers:

- root thread ID to session registration ID;
- app-server session-tree ID to that root owner;
- root ownership generation, so a late notification from an old tab epoch cannot attach to a replacement.

`thread/started` notifications and returned descendant rows carry enough thread, session-tree, and parent information to associate children. Parent collaboration items and the existing descendant discovery path provide independent confirmation. Once a session proves a descendant, it reports that relationship to the host so later notifications route directly.

Thread-scoped notifications whose owner is not yet known enter a process-bounded candidate queue keyed by thread ID. A later relationship drains them in order. Queue count and per-thread item count are capped; expiry drops unknown activity without guessing. `thread/closed` and deletion notifications remove routing entries.

Process-scoped notifications such as skill invalidation are broadcast to registered sessions. Each session then refreshes using its own absolute cwd. Unknown thread-scoped activity is never broadcast because doing so would make every tab build speculative state for unrelated roots.

### 7. Make every workspace-dependent request explicit

The shared process does not set its current directory from an Agent Tab. `thread/start` always sends the absolute primary directory, including single-directory workspaces. It sends ordered runtime roots when multiple directories are attached. `thread/list`, `skills/list`, and any later cwd-sensitive request receive the session's absolute primary directory rather than `.`.

`thread/resume` keeps the persisted thread identity and provider definition; subsequent turns continue to send the current workspace access snapshot. This preserves the existing ability to update additional directories without treating process state as conversation state.

Starting one network listener per workspace was rejected because app-server already supports explicit cwd fields and a second transport would add no isolation.

### 8. Keep model discovery host-scoped

The host may issue or cache one `model/list` result for default-provider tabs. A custom-provider session treats its explicitly configured model as authoritative and does not label the host result as that gateway's catalog. If the existing UI needs a list, it receives the configured custom model plus any provider-neutral entries whose source is unambiguous.

This limitation is surfaced in adapter behavior rather than hidden by issuing repeated `model/list` calls, because that method has no provider selector. Directly querying arbitrary gateways is outside this change.

### 9. Detach sessions explicitly

Session shutdown becomes a thread operation instead of a process operation:

1. stop accepting new tab requests;
2. interrupt the session's active root turn when required;
3. answer or clear its pending server requests;
4. send `thread/unsubscribe` for the root;
5. remove request, root, tree, and descendant ownership;
6. release the host `Arc`.

Cleanup is scheduled off the UI thread and is idempotent. Replacing a session acquires the new host reference before releasing the outgoing session, matching the existing DSH reset safeguard. Other sessions remain attached throughout.

### 10. Treat host exit and binary update as generation events

The stdout-closed callback marks the host generation stopped exactly once and sends a synthetic host-exit message to every registered callback. Tabs clear readiness and active protocol state but retain transcript, draft, profile, workspace, and root thread ID. Concurrent retries pass through the same host acquisition gate; each recovered session sends `thread/resume` with replay suppression when its pane already owns the visible transcript.

Binary update coordination captures all affected Codex thread IDs before detaching them. After every affected session is quiescent, the transaction stops the shared host once, runs the updater once, creates one new generation, and resumes each tab independently. One failed resume does not discard the new host or block successful siblings.

Automatic replay of an in-flight model stream after a crash was rejected because app-server can persist only the completed prefix and cannot promise the model or tool action is safe to repeat. Recovery resumes the thread at its durable state and reports the interruption.

## Risks / Trade-offs

- [One host failure affects every Codex tab] → Broadcast one generation failure, retain every recovery identity, and make retry single-flight.
- [A high-volume tab delays stdout handling] → Router callbacks only enqueue messages; parsing and UI mutation remain in each tab's existing pump.
- [Child activity arrives before ownership metadata] → Hold a bounded candidate queue and discard unresolved activity instead of guessing.
- [All compatible profile keys exist in one child environment] → Use distinct names, keep raw values out of RPC and diagnostics, and retain the existing documented local-process protection scope.
- [A profile edit cannot change a live child environment] → Return a restart-required error and apply the new snapshot to the next host generation.
- [Custom gateways do not have provider-specific model discovery] → Keep the configured model usable and avoid attributing the host catalog to that gateway.
- [Global routing maps retain detached sessions] → Make detach idempotent, remove all indexes by session registration ID, and test late responses and late notifications.
- [The shared process no longer inherits a workspace cwd] → Pass absolute cwd on every workspace-sensitive request and add regression coverage for two simultaneous workspaces.
- [A fixed-size request ID can eventually wrap] → Skip IDs still present in pending maps and fail host requests safely if no free ID can be allocated.

## Migration Plan

1. Add the host and router behind the current Codex `Session` public behavior, with fake-process tests before changing `Backend`.
2. Replace fixed request IDs with typed routed purposes and move initialization into the host.
3. Split Codex launch data into host-level and thread-level snapshots; generate distinct provider credential names while leaving persisted profile credentials unchanged.
4. Make cwd-sensitive requests explicit and switch `Backend::spawn` to acquire a host and create or resume a session on it.
5. Add explicit session detach and host-exit handling, then update reset and history-restore flows.
6. Change binary-update recovery to stop and recreate the shared generation once.
7. Remove the per-session process path after multi-session, multi-gateway, routing, failure, and update regression tests pass.

No on-disk conversation or credential migration is required. Rollback restores per-tab process ownership; persisted thread IDs, profile credentials, and rollout data remain readable by the prior implementation.
