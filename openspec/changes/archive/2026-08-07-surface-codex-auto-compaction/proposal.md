## Why

Codex app-server already performs automatic context compaction, but Agent Tab renders its `contextCompaction` lifecycle as a generic tool call. Users therefore cannot tell when the active context is being rewritten, how much space was reclaimed, or where the model-visible history boundary moved.

## What Changes

- Translate Codex `contextCompaction` item lifecycle notifications into the existing provider-neutral compaction progress and transcript model.
- Distinguish live automatic compaction from NiumaTerm-initiated manual `/compact` requests without relying on fields the app-server protocol does not expose.
- Correlate surrounding context-window usage updates into optional before/after accounting while omitting values that cannot be established safely.
- Replay persisted Codex compaction items as structural transcript boundaries, degrading unavailable trigger, accounting, and summary metadata cleanly.
- Complete manual `/compact` feedback from the item lifecycle rather than treating the immediate JSON-RPC acknowledgement as completed work.

## Capabilities

### New Capabilities

- `agent-context-compaction`: Defines progress, transcript-boundary, accounting, trigger, and replay behavior for provider-driven context compaction in Agent Tab.

### Modified Capabilities

None.

## Impact

- `crates/agent_utils/src/codex/app_server.rs`: Codex session state, notification translation, replay parsing, and focused protocol tests.
- `crates/app/src/ui/agent_pane.rs`: Non-expandable Codex compaction boundaries while retaining Claude's real summary disclosure.
- Existing provider-neutral chat types and Agent Tab compaction rendering are reused without changing the public app-server protocol or adding dependencies.
- Codex continues to own automatic-compaction thresholds through its normal configuration; Agent Tab adds no competing threshold setting.
