## Why

The composer currently reduces agent context reporting to a used-token count and an optional limit, hiding the input, cache, output, reasoning, and cumulative accounting that providers already expose. A compact hover surface can make context pressure understandable without adding permanent visual weight to the composer.

## What Changes

- Preserve provider-reported context accounting instead of collapsing it to two values.
- Show a hover panel from the composer context indicator with current usage, capacity, and available token categories.
- Present provider-specific cumulative accounting with an explicit scope so thread totals and query totals are not confused.
- Hide unavailable categories and keep account rate limits in their existing titlebar surface.

## Capabilities

### New Capabilities

- `agent-context-usage`: Defines live agent context accounting and its compact hover presentation.

### Modified Capabilities

None.

## Impact

- Extends the provider-neutral chat event model in `crates/agent_utils`.
- Updates Codex app-server and Claude Code stream-json parsing.
- Updates the agent composer status UI in `crates/app`.
- Adds focused parser and presentation tests without adding new dependencies.
