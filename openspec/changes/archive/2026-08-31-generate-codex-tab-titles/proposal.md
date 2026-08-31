## Why

Codex Agent Tabs currently keep a shortened copy of the opening prompt as their permanent title, which makes long or conversational requests hard to scan. The desktop Codex application instead shows an immediate provisional title and replaces it with a concise generated title without overwriting user edits.

## What Changes

- Show a local provisional Codex Tab title as soon as the first user turn is accepted.
- Generate a concise title asynchronously through an isolated read-only Codex thread.
- Persist the generated title through `thread/name/set` only when the provisional title is still current.
- Preserve a user-authored rename and any newer conversation title while generation is in flight.
- Keep the provisional title when generation fails, times out, or returns unusable output.
- Restore the persisted title when a Codex history thread is resumed.

## Capabilities

### New Capabilities

- `agent-conversation-titles`: Defines provisional, generated, persisted, restored, and user-authored titles for Codex Agent Tabs.

### Modified Capabilities

None.

## Impact

- Affects Codex app-server request handling in `crates/agent_utils` and Agent Tab session/title handling in `crates/app_agent` and `crates/app`.
- Adds no external dependency and reuses the configured Codex app-server host.
- Adds one bounded background model turn for the first accepted prompt of a new Codex conversation.
