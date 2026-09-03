## Why

Claude Agent Tabs already ask Claude Code to generate a persisted session title, but they do not show an immediate provisional title, can retry from a later prompt after generation returns nothing, and ignore persisted `ai-title` records in history. This makes live and restored titles diverge from the behavior users see in Claude Desktop.

## What Changes

- Show a bounded provisional title as soon as a new Claude conversation accepts its first ordinary prompt.
- Submit at most 2,000 characters to Claude Code's existing title-generation control request and make the first accepted prompt the only automatic naming attempt.
- Retain the provisional title when generation returns no usable title, while allowing a generated title to replace it asynchronously.
- Treat resumed Claude conversations as already named so a follow-up prompt cannot rename them.
- Read persisted Claude `ai-title` metadata in session history, while keeping a user `custom-title` authoritative.
- Keep Codex and DeepSeek title behavior unchanged.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `agent-conversation-titles`: Extend the two-stage title behavior and restored-title requirements to Claude Agent Tabs.

## Impact

- `crates/app_agent`: Claude provisional-title creation, one-shot title state, and resume handling.
- `crates/agent_utils`: Claude title request bounds and JSONL title metadata parsing.
- Existing Claude Code stream-json control messages remain the only provider integration used for title generation and persistence.
- No new dependencies or persisted formats are introduced.
