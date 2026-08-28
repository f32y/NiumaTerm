## 1. Shared Host Foundation

- [x] 1.1 Extend `JsonLineProcess` with an explicit stdout-closed callback and thread-safe host-owned writing, and verify focused subprocess tests distinguish normal shutdown from unexpected exit without exposing stderr beyond its bound.
- [x] 1.2 Add `codex/app_server/host.rs` with host launch identity, weak global retention, single-flight acquisition, initialization, and last-owner shutdown; verify fake-process tests open two simultaneous sessions with one child and one initialize exchange.
- [x] 1.3 Add host compatibility validation for executable, arguments, and process-level environment, and verify compatible gateway-only differences reuse the host while an incompatible executable returns an actionable error and starts no second child.

## 2. Global JSON-RPC Routing

- [x] 2.1 Replace fixed session request IDs with a host-wide allocator and typed pending request purposes, and verify responses returned out of order reach only their submitting sessions.
- [x] 2.2 Route server-to-client approval and user-input requests through a separate ownership table keyed by target thread, and verify two concurrent approvals can be answered independently with their original request IDs.
- [x] 2.3 Add root-thread and session-tree ownership registration after start or resume, and verify root notifications for two active turns update only the matching session state.
- [x] 2.4 Add descendant ownership registration from thread metadata, collaboration activity, and descendant discovery, plus bounded early-notification retention; verify an early child update is delivered after its parent is proven and unrelated activity is discarded.
- [x] 2.5 Broadcast only process-scoped notifications to registered sessions and remove every route on detach, and verify late responses, closed descendants, and unowned thread notifications cannot mutate another session.

## 3. Codex Session Refactor

- [x] 3.1 Change Codex `Session` to hold an `Arc` host and registration ID while retaining conversation-specific state, and verify existing command, approval, compaction, usage, replay, and skill parsing tests pass through routed messages.
- [x] 3.2 Move app-server initialization into the host and make session creation send only `thread/start` or `thread/resume` after host readiness; verify a second session becomes ready without another initialize request.
- [x] 3.3 Connect `CodexTasks` descendant confirmation to the host ownership index, and verify two roots with nested child agents keep Background Tasks and transcripts isolated.
- [x] 3.4 Implement idempotent session detach with root-turn interruption when needed, pending server-request cleanup, `thread/unsubscribe`, and route removal; verify closing one of two sessions leaves the other active on the same host.
- [x] 3.5 Preserve the host across `/new`, profile restart, branch, and history restore until the replacement session owns a host reference, and verify session reset does not stop or reinitialize app-server.

## 4. Profile Gateways and Credentials

- [x] 4.1 Derive a stable environment-safe credential name from each Codex provider ID and separate its secret value from `CodexProviderConfig`; verify two profiles produce distinct names and collision detection fails visibly.
- [x] 4.2 Build host bootstrap environment entries from every compatible configured Codex profile while merging identical process settings and rejecting conflicts; verify raw API keys appear only as child environment values and never in host identity or diagnostics.
- [x] 4.3 Keep generated provider definitions on both `thread/start` and `thread/resume`, and verify two sessions on one fake app-server send different provider IDs, gateway URLs, models, and credential environment names.
- [x] 4.4 Make custom-provider model presentation retain the profile's configured model without attributing the host model list to that gateway, and verify default profiles still receive the host-scoped model catalog.

## 5. Workspace-Explicit Requests

- [x] 5.1 Always include the workspace's absolute primary directory in Codex `thread/start`, including single-directory workspaces, and verify multi-root requests preserve the ordered runtime root list.
- [x] 5.2 Replace `.` and process-cwd assumptions in `thread/list`, `skills/list`, and other cwd-sensitive Codex requests with the session's absolute primary directory; verify history and skills remain isolated across two simultaneous workspaces.
- [x] 5.3 Stop setting the shared app-server process directory from an Agent Tab and verify changing one tab's workspace cannot change config discovery or Git context for another tab.

## 6. Agent Tab Lifecycle and Failure Recovery

- [x] 6.1 Update `Backend::spawn`, the session start pump, and Codex shutdown handling to acquire and release the shared host while preserving existing epoch filtering; verify stale messages from a replaced session are ignored.
- [x] 6.2 Publish one unexpected-host-exit event to every attached Codex tab, retain each transcript and thread recovery ID, and verify all affected composers become unavailable without losing pane state.
- [x] 6.3 Add single-flight retry that starts one replacement host and resumes each affected thread with replay suppression, and verify concurrent retries create one child while one failed resume does not block the others.
- [x] 6.4 Add visible startup errors for incompatible live-host settings and restart-required profile changes, and verify existing tabs remain ready and no fallback process starts.

## 7. Binary Update Coordination

- [x] 7.1 Change Codex update suspension to detach every affected session and stop the shared host generation exactly once before invoking the updater; verify the updater never begins while that owned process tree is live.
- [x] 7.2 Start one replacement host after the update and resume every captured Codex thread independently, and verify retained transcripts are not replayed twice and an untouched tab receives a fresh thread on the same host.
- [x] 7.3 Extend provider update recovery coverage to multiple Codex tabs and providers, and verify successful, failed, and unchanged update outcomes all attempt restoration without creating more than one app-server generation.

## 8. Integrated Regression Coverage

- [x] 8.1 Add a fake app-server integration fixture covering two workspaces, two gateways, concurrent turns, out-of-order responses, approvals, descendant activity, unsubscribe, and unexpected exit; verify `cargo test -p nmt_agent_utils codex::app_server` passes.
- [x] 8.2 Add Agent Tab integration tests for shared startup, close-one-keep-one, reset handoff, host failure, retry, and update restoration; verify `cargo test -p nmt_app_agent session` passes.
- [x] 8.3 Run `cargo test -p nmt_config`, `cargo test -p nmt_agent_utils`, and `cargo test -p nmt_app_agent`, and verify the changed profile, routing, lifecycle, and recovery behaviors pass together.
