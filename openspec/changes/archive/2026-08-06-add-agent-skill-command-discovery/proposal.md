## Why

The Agent Tab slash palette cannot currently discover the skills and plugin-provided skills published by the active Claude CLI before the first turn. The Codex integration has neither a skill catalog nor a structured skill invocation path. As a result, users cannot discover and select installed reusable workflows from the `/` entry point as they can in Claude Code and Codex CLI.

## What Changes

- Load command names, descriptions, argument hints, and aliases from Claude's structured initialize `commands` catalog while retaining compatibility with the legacy `slash_commands` payload.
- Show Claude skills, project commands, and namespaced plugin skills in the slash palette before the first turn, and continue to execute them through the provider-command path.
- Load skills available to the current working directory through the stable Codex app-server `skills/list` method and refresh the catalog after `skills/changed` notifications.
- Merge Codex skills directly into the top-level `/` palette while retaining `/skills` as an optional skills-only view. Selecting a skill with Tab, Enter, or the mouse completes the composer with `$skill-name ` and attaches the exact `{ type: "skill", name, path }` input item when starting or steering a turn.
- Preserve Codex skills with the same name but different scopes or paths, and display each skill's scope, description, and disabled state. Namespaced plugin skills use the same catalog and invocation flow as standalone skills.
- Exclude plugin marketplace browsing, installation, removal, and enablement management because the relevant Codex app-server APIs are not yet suitable as production client dependencies.

## Capabilities

### New Capabilities

- None.

### Modified Capabilities

- `agent-slash-commands`: Extend backend-aware discovery so Claude and Codex skills, including plugin-provided skills, appear directly in the slash palette and Codex selections are invoked with structured input.

## Impact

- `crates/agent_utils/src/chat.rs`: Add backend-neutral skill metadata, catalog events, and structured skill references in user input.
- `crates/agent_utils/src/claude_code/stream_json.rs`: Parse initialize `commands`, aliases, and the legacy fallback payload.
- `crates/agent_utils/src/codex/app_server.rs`: Request and refresh `skills/list`, parse skill metadata, and attach skill items to turn input.
- `crates/app/src/ui/agent_commands.rs` and `agent_pane.rs`: Merge Codex skills into the top-level slash palette, retain the `/skills` focused view, render scope and disabled state, validate selections, and route structured input.
- No third-party dependencies are added. Regular terminal panes remain unchanged, and the change introduces neither plugin installation management nor hidden provider turns.
