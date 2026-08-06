## 1. Backend-Neutral Skill Vocabulary

- [x] 1.1 Add skill metadata and replacement-catalog events to the shared Agent Tab chat types, preserving name, description, absolute path, scope, enabled state, and optional display metadata without deduplicating by name.
- [x] 1.2 Extend the shared user-message request boundary to carry an optional structured skill reference for both start and steer paths while keeping existing text-only callers behaviorally unchanged.
- [x] 1.3 Add focused tests for replacement-snapshot semantics, same-name path identity, and text-only request compatibility.

## 2. Claude Command Discovery

- [x] 2.1 Parse Claude initialize `response.commands` entries, including canonical name, description, `argumentHint`, and aliases, and publish them through the existing provider command catalog before the first turn.
- [x] 2.2 Track whether a structured Claude catalog was published so `system/init.slash_commands` is used only as a legacy fallback and missing catalogs finish initialization without a hidden turn or fatal error.
- [x] 2.3 Add Claude wire fixtures and adapter tests covering structured skills, project commands, namespaced plugin skills, aliases, legacy fallback, and protection against a legacy list overwriting structured metadata.

## 3. Codex Skill Catalog and Wire Input

- [x] 3.1 Add Codex app-server request and response types for `skills/list`, map the session working directory correctly, and publish complete replacement snapshots including disabled and duplicate-name entries.
- [x] 3.2 Convert per-working-directory catalog errors and unsupported-method responses into non-fatal skill-catalog availability state without disrupting chat, settings, or existing slash commands.
- [x] 3.3 Handle `skills/changed` with a forced reload and coalesce overlapping notifications so only the newest completed catalog can become current.
- [x] 3.4 Clear Codex skill catalog state during session replacement and prevent responses from a replaced session from repopulating the active catalog.
- [x] 3.5 Serialize a validated structured `{ type: "skill", name, path }` item alongside the original text item for both `turn/start` and `turn/steer`.
- [x] 3.6 Add Codex protocol tests for initial loading, duplicate scopes, disabled entries, invalidation/reload ordering, non-fatal errors, and structured skill items on start and steer.

## 4. Codex Skill Picker and Binding Lifecycle

- [x] 4.1 Merge Codex skills directly into the top-level `/` palette while retaining `/skills` as an optional skills-only view, without sending a provider command or creating transcript/turn state.
- [x] 4.2 Rank top-level command and skill rows together across command name, skill name, display name, and description while preserving duplicate-name skill paths.
- [x] 4.3 Render top-level Codex skill rows with slash-entry labels, description, scope, loading or unavailable feedback, and disabled state, while preserving the focused `/skills` rendering and existing navigation interactions.
- [x] 4.4 On enabled skill activation from either view with Tab, Enter, or the mouse, replace the composer with `$name ` and store the exact name/path binding without sending; reject disabled rows.
- [x] 4.5 Invalidate the binding when the first composer token changes, another skill is selected, the catalog removes or disables the entry, or the session is reset, while allowing edits to task text after an unchanged token.
- [x] 4.6 Revalidate the binding against the current catalog immediately before start or steer, preserve the composer with actionable feedback on failure, and leave manually typed unbound `$name` input as ordinary text.
- [x] 4.7 Preserve normal user-bubble, working-timer, and turn-folding behavior when a valid structured skill prompt is submitted.
- [x] 4.8 Add UI and state tests for direct top-level discovery, combined command/skill ranking, plugin-skill filtering, duplicate scopes, keyboard and mouse selection, disabled rows, binding edits, stale catalogs, manual `$name` input, and start/steer routing.

## 5. Verification

- [x] 5.1 Run Rust formatting checks and address formatting changes limited to this implementation.
- [x] 5.2 Run the focused adapter and Agent Tab test suites that cover the new Claude and Codex catalog behavior and structured skill invocation.
- [x] 5.3 Run `cargo check` for the affected workspace targets and resolve all compilation errors.
- [x] 5.4 Launch `target\debug\NiumaTerm.exe --testing` for isolated manual verification that Claude commands still appear before the first turn and Codex shows ordinary and namespaced plugin skills directly in the first-level slash palette, completes them to `$name `, and does not expose plugin management.
