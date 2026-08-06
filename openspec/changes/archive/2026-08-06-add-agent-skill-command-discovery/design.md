## Context

Agent Tab already has a backend-neutral slash-command vocabulary. The UI merges local, adapter, and provider commands and uses one palette, argument-stage flow, queue policy, and feedback model. The Claude stream-json adapter parses `system/init.slash_commands`, while the Codex app-server adapter explicitly advertises only `/compact` and `/review`.

Current Claude Code versions return a structured command catalog in the initialize control response at `response.commands`. It includes skills, project commands, namespaced plugin skills, descriptions, argument hints, and aliases. The current adapter still reads the older `response.slash_commands` field, so those entries are unavailable before the first turn. Codex has no general slash-command RPC, but app-server provides stable `skills/list` and `skills/changed` APIs and a structured `skill` user-input item. Codex invokes a skill with `$skill-name` rather than treating every skill as a slash command.

The implementation must preserve these constraints: regular terminal panes are unaffected; catalog queries do not create hidden turns; provider catalog failures do not make the chat session fatal; same-name Codex skills from different scopes are distinguished by absolute path; and unstable plugin-management APIs are not used as a production path.

## Goals / Non-Goals

**Goals:**

- Show the ordinary skills, project commands, plugin skills, and aliases published by Claude after initialization and before the first turn.
- Show Codex skills directly in the filterable top-level `/` palette, retain `/skills` as a focused skills-only view, and invoke the exact selected skill path.
- Use provider-published data for enabled state, scope, description, same-name entries, and plugin namespaces instead of reconstructing provider catalog rules.
- Prevent stale or incorrect skill paths from being sent after a catalog refresh, session rebuild, or composer edit.
- Preserve the distinct transcript and turn-lifecycle semantics of slash commands, ordinary messages, and skill prompts.

**Non-Goals:**

- Implement `/plugins` marketplace browsing, plugin details, installation, removal, upgrades, or enablement controls.
- Scan Codex or Claude plugin cache paths to infer available plugins or skills.
- Add a separate `/skills` manager for Claude, because Claude already publishes user-invocable skills as provider slash commands.
- Add independent `$`-triggered autocomplete for Codex; this change uses the `/` palette and optional `/skills` view as its discovery entry points.
- Change skill contents, permission rules, dependency installation, or the provider's own skill resolution behavior.

## Decisions

### D1. Keep skill metadata separate from slash-command metadata

The backend-neutral chat vocabulary will gain `SkillInfo` containing name, description, path, scope, enabled state, and optional display metadata; a replacement-snapshot event for the skill catalog; and a structured user-input reference containing skill name and path. `SlashCommandInfo` will continue to represent only commands routed through `/name`.

Claude skills remain slash commands because that is how the provider exposes them. Codex skills retain their `$name` and absolute-path semantics. Keeping the models separate avoids flattening two different invocation protocols into a misleading command abstraction.

An alternative was to convert every Codex skill into `SlashCommandInfo`. That would invent a `/$skill` or `/skill-name` syntax, lose reliable identity for same-name paths, and conflict with the rule that slash-command execution does not create an ordinary user bubble while a skill prompt does. This alternative is rejected.

### D2. Prefer Claude initialize `commands` and use the legacy catalog only as a fallback

The initialize control response `commands` array is the primary catalog. The adapter will parse each structured name, description, `argumentHint`, and alias. The canonical name and every alias become filterable, executable provider commands, while existing normalization and local-over-adapter-over-provider precedence resolve conflicts.

The adapter records whether it has published a structured catalog. `system/init.slash_commands` is used only when initialize did not provide `commands`, preventing a later string-only list from overwriting descriptions, argument hints, and aliases obtained during initialization. If neither message contains a catalog, the UI leaves the loading state while preserving local and adapter commands and sends no warm-up turn.

An alternative was to scan `.claude/skills`, `.claude/commands`, and plugin cache directories. That would duplicate Claude's settings scopes, visibility rules, aliases, plugin enablement, and policy decisions and could advertise commands the provider rejects. This alternative is rejected.

### D3. Use Codex `skills/list` replacement snapshots and react to invalidation notifications

The adapter sends one `skills/list` request after app-server initialization. When the request omits `cwds`, app-server uses its process working directory, which is the same cwd assigned when AgentPane starts the session. Entries from the response are flattened into one ordered replacement snapshot. Same-name skills with different paths or scopes remain distinct, and provider errors become non-fatal feedback.

After `skills/changed`, the adapter requests `skills/list` with `forceReload: true`. Only one refresh is in flight at a time; repeated notifications are coalesced into one subsequent refresh so stale responses cannot repeatedly overwrite a newer catalog. Rebuilding a session clears the previous snapshot until the new backend publishes one.

If `skills/list` is missing or fails, ordinary chat, existing slash commands, and settings remain available. `/skills` displays why the catalog is unavailable instead of terminating the session.

### D4. Merge skills into the top-level slash palette and retain `/skills` as a focused view

Once the Codex skill snapshot is available, the top-level `/` palette combines command rows and skill rows. Both row types participate in one exact, prefix, and substring ranking, but a skill row keeps a dedicated `Skill` action instead of becoming `SlashCommandInfo`. This preserves `$name` invocation and exact path identity while providing the same one-step discovery experience as Claude. Top-level skill rows use `/name` as the entry label and identify their scope; choosing one rewrites the composer to the provider's real `$name ` syntax.

The Codex adapter catalog also retains `/skills`. Selecting it completes the composer with `/skills ` and enters a skills-only stage without invoking a provider-command RPC or creating a transcript entry. This focused view remains useful for large catalogs and uses `$name` labels.

In both views, filtering matches name, display name, and description. Entries with `enabled=false` remain visible but disabled. Same-name entries with different paths are not deduplicated; each row action retains scope and path identity. Namespaced plugin skills use the same rendering and selection flow as ordinary skills.

Selecting an enabled skill with Enter, Tab, or the mouse changes the composer to `$name `, closes the slash palette, and stores a `{name, path}` binding without sending a message. Escape and Up/Down retain the existing palette behavior.

### D5. Send a structured skill binding only while the composer token and current catalog still match

A binding created by the picker is revalidated at submission time. The composer's first token must still equal the exact `$name`, and the current skill snapshot must still contain the same enabled name/path pair. Users may freely edit task text after the token. Editing or removing the skill token, selecting another entry, or resetting the session clears or invalidates the binding.

For a valid binding, Codex `turn/start` or `turn/steer` input contains both the original text item and a `{type:"skill", name, path}` item. The skill prompt remains ordinary user input: it displays a user bubble and follows the StartedTurn or Steered event, working timer, and turn-folding lifecycle. If a catalog refresh removes or disables the skill, submission preserves the composer text, displays an error, and does not send the stale path.

A manually typed `$name` without a picker binding remains ordinary text. App-server or the model may apply its own fallback behavior, but NiumaTerm does not guess among same-name paths.

### D6. Surface plugin skills through provider catalogs without implementing plugin management

Claude entries such as `plugin-name:skill-name` and their aliases flow directly from initialize `commands` into the slash palette. Namespaced Codex plugin skills returned by `skills/list` flow into both the top-level slash palette and the `/skills` focused view. NiumaTerm neither infers plugin provenance from filesystem paths nor calls Codex `plugin/list`, `plugin/read`, `plugin/install`, or `plugin/uninstall`.

This provides the requested ability to use plugin commands while leaving marketplace state, authorization, installation policy, and enablement to a later dedicated design.

## Risks / Trade-offs

- [Claude wire fields vary by version] -> Cover structured `commands` and legacy `slash_commands` with fixtures, ignore unknown fields, and treat a missing catalog as non-fatal.
- [A large Codex skill catalog adds palette noise] -> Preserve exact/prefix/substring ranking and the existing scroll container in the combined palette, and retain `/skills` as a focused skills-only view.
- [Same-name skill selection is ambiguous] -> Do not deduplicate by name, display scope, and bind the selected row's absolute path.
- [`skills/changed` races with submission] -> Revalidate name, path, and enabled state against the current snapshot immediately before submission and preserve the composer on failure.
- [Older Codex app-server versions lack `skills/list`] -> Treat a method error as an unavailable capability rather than a fatal session error so other Agent Tab functions remain usable.
- [Plugin skills create expectations of plugin management] -> Promise only selection of enabled skills published by the provider and do not advertise a nonfunctional `/plugins` command.

## Migration Plan

1. Extend the backend-neutral types and pure parsing/filtering helpers without changing existing command wire behavior.
2. Correct Claude initialize catalog handling and add compatibility fixtures.
3. Add Codex skill discovery, refresh, and structured input sending.
4. Connect the AgentPane top-level skill rows, `/skills` focused picker, binding lifecycle, and feedback.
5. Verify both backends with unit tests, `cargo check`, and an isolated application launch using `--testing`.

This change migrates no persisted data or configuration. A rollback can remove `/skills` and the skill events without affecting provider skill files, plugins, or session records.

## Open Questions

- None. Full plugin management remains a separate future change and does not block this design.
