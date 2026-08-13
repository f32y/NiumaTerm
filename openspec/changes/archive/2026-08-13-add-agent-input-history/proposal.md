## Why

Agent Tabs do not retain composer input for reuse, so users must retype repeated prompts and cannot get consistent recall behavior from Claude and Codex. NiumaTerm should own a backend-aware input history that works across application restarts without depending on provider-private files or unavailable provider APIs.

## What Changes

- Record successfully accepted Agent composer input, including ordinary messages, accepted steering input, and successfully dispatched slash commands, in NiumaTerm-managed persistent storage.
- Isolate history by execution target, Agent backend, and working directory, retain a bounded number of recent entries, and collapse adjacent duplicate text.
- Add Codex-style Up and Down recall: empty input can enter history browsing, unchanged recalled text can continue browsing from a whole-buffer boundary, and other non-empty drafts keep normal editor movement.
- Keep command palettes and recent-session selection ahead of input history in keyboard routing.
- Restore recalled text through the existing composer input state and place the cursor at the end of the recalled entry.
- Keep terminal input history and provider-owned history files outside this change.

## Capabilities

### New Capabilities

- `agent-input-history`: Persist and recall Agent composer input with backend-aware, working-directory-scoped navigation behavior.

### Modified Capabilities

None.

## Impact

- Agent composer keyboard routing and input replacement under `crates/app/src/agent_pane`.
- Application-local persistence for bounded Agent input history.
- Agent submission and slash-command dispatch paths that decide when an entry was successfully accepted.
- Unit and integration coverage for persistence, deduplication, navigation boundaries, palette priority, and backend or working-directory isolation.
- No provider protocol changes and no changes to ordinary terminal panes.
