## Why

NiumaTerm currently starts one `codex app-server` process for every Codex Agent Tab, even though one initialized app-server can host many independent threads. Sharing one process removes repeated startup and runtime cost while the existing thread-scoped provider configuration allows profiles to keep different models, gateway URLs, and credentials.

## What Changes

- Add an application-wide Codex host that starts lazily, initializes app-server once, and is retained by open Codex sessions.
- Make each Codex tab own only its conversation state and root thread while the host owns the child process, JSON-RPC request IDs, response routing, and process diagnostics.
- Route responses, server requests, root-thread notifications, and descendant-thread notifications to the owning tab without allowing activity from another root thread to alter its state.
- Start and resume threads with the profile's model and generated provider definition so different tabs on the shared host may use different LLM gateways.
- Give each custom Codex profile a stable credential environment name and build the shared host environment from compatible configured profiles without placing API keys in RPC payloads or logs.
- Send absolute thread and history working directories because the shared process can no longer use one tab's workspace as its process working directory.
- Unsubscribe a closing tab's thread, keep the host alive while another Codex tab holds it, and stop the owned process tree after the final session releases it.
- Coordinate unexpected host exit, explicit retry, Agent Tab reset, and Codex binary updates across every attached session.
- **BREAKING**: concurrently active Codex profiles must resolve to one compatible app-server executable and process-level environment. An incompatible profile is rejected visibly instead of starting a second app-server or silently using another profile's runtime settings.

## Capabilities

### New Capabilities

- `agent-codex-shared-host`: Application-wide app-server lifecycle, multiplexed session routing, profile-specific gateway selection, credential injection, isolation, and recovery.

### Modified Capabilities

- `agent-workspace-access`: Codex workspace identity moves fully to thread and turn requests instead of relying on a per-tab process working directory.
- `agent-binary-updates`: A Codex update suspends attached tabs, stops the shared host once, starts one replacement host, and resumes every recoverable thread through it.

## Impact

- `crates/agent_utils/src/codex/app_server/`: split process ownership and JSON-RPC routing from per-tab thread state; add shared-host lifecycle and routing tests.
- `crates/agent_utils/src/subprocess.rs`: expose the process operations needed by a host shared across callbacks, if the current `JsonLineProcess` API cannot support them directly.
- `crates/app_agent/src/session/` and `crates/app_agent/src/profile.rs`: acquire the shared host, supply compatible host configuration and profile-specific thread configuration, and coordinate replacement or update recovery.
- Codex profile runtime configuration: generate stable per-profile credential environment names and validate process-level compatibility.
- Existing Codex app-server RPC usage remains v2 and requires no new third-party dependency.
