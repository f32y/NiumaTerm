## Context

NiumaTerm already uses a VS Code Modern-inspired shell with floating workspace and pane surfaces, a virtualized Agent transcript, Markdown rendering through the vendored `gpui-component` TextView, and theme-provided semantic colors. The current composition does not consistently expose that structure: the workspace sidebar has an explicit surface while the Agent pane can read as uncontained, prose spans excessively wide panes, and several unrelated expandable rows have independent geometry.

The change crosses `agent_pane.rs`, shell and pane composition, workspace status projection, Codex usage presentation, Claude CLI execution and output parsing, provider-icon assets, and the shared Markdown renderer. It must preserve virtual-list behavior, selectable transcript text, command and tool expansion, keyboard input, tab/workspace dragging, narrow-window usability, and all built-in themes. NiumaTerm remains Windows-focused, and any future manual launch used to validate this work must use `--testing` so it cannot reuse a normal running process.

## Goals / Non-Goals

**Goals:**

- Keep prose readable on wide panes without reducing the usable width of code, tables, diffs, or command output.
- Make the workspace surface and pane surface read as related cards without changing the established independent tab-strip styling.
- Use color only for meaningful status and action semantics, with the same rules across built-in themes.
- Make provider quota, workspace identity, and path information understandable through a stable visible order, with complete semantics available to assistive technology and tooltips.
- Preserve current behavior and serialized configuration while changing presentation.

**Non-Goals:**

- Redesign terminal cell rendering, the Settings dialog, the Git diff sidebar, or agent protocol behavior.
- Add avatars, sender names, chat-style speech bubbles, or per-message reaction controls.
- Invent an agent failure state when the runtime monitor has not reported one.
- Contact Anthropic OAuth usage endpoints, read Claude OAuth credentials directly, scrape claude.ai, or infer subscription quotas from local token statistics.
- Display quota progress bars, inline period labels, reset timestamps, or Claude Fable usage in the compact sidebar summary.
- Replace GPUI, `gpui-component`, the transcript virtual list, or the existing theme system.

## Decisions

### 1. Add an opt-in prose measure to TextView block rendering

`TextViewStyle` will gain an optional prose maximum width, expressed in rems so it scales with the configured Agent font. The default remains unset, preserving every existing TextView caller. Agent assistant replies will enable a measure tuned to approximately 70-90 monospaced characters. Paragraphs, headings, lists, and blockquotes will honor the prose measure; code blocks, tables, diffs, and tool-output containers will remain full width.

Applying `max_w` to the entire assistant row was rejected because it would also constrain the content that most benefits from horizontal space. Splitting Markdown into independent TextViews in `agent_pane.rs` was rejected because it would duplicate parsing, complicate selection across blocks, and weaken TextView's document semantics.

When a pane is narrower than the preferred measure, prose will shrink to the available width. Any new style field must participate in the TextView style equality or invalidation path so cached layout is remeasured when the effective style changes.

### 2. Build all expandable transcript rows from one disclosure geometry

Turn-duration rows, collapsed tool-run rows, and expandable work rows will share an application-level disclosure primitive. The primitive will provide a fixed leading chevron slot, optional type-icon slot, label and preview area, optional trailing status slot, one row height, one hover treatment, and one content inset for expanded detail.

The primitive belongs in the application UI layer because it carries Agent transcript semantics that are not yet shared by the wider component library. Existing state sets (`expanded_turns`, `expanded_groups`, and `expanded_rows`) remain unchanged; only rendering is consolidated. Moving all state into a generic accordion component was rejected because the transcript uses three different stable keys and virtual-list row specifications.

### 3. Treat the transcript top edge consistently

The transcript list will have 16-24 logical pixels of top breathing room. When its logical scroll top is no longer the first item at zero offset, the viewport will paint a theme-background-to-transparent gradient over the top edge. The overlay must not create an interactive hit region, so selection, links, and disclosure clicks remain available beneath it.

### 4. Keep tabs independent and frame the pane body as a floating surface

The workspace sidebar card and pane-body card will use the same surface background, semantic border, large radius, and horizontal/bottom gutter rules. The tab strip retains its established independent styling immediately above the pane card. A single pane will not draw a second outer card. Split layouts may retain internal pane borders and the focused-pane accent, clipped by the outer pane surface.

Wrapping the tab strip into the pane card was tried and rejected because the extra card background and divider weakened the established tab hierarchy. Removing both cards in favor of a flat split view was also rejected because it would abandon the repository's Modern UI direction.

### 5. Group composer controls by consequence

In the Agent composer, the model picker becomes the primary thread-setting control. Approval and sandbox controls form an execution-policy group; reasoning effort and service tier form a quality/cost group. The control requests, stored values, menu contents, and keyboard behavior remain unchanged. Tooltips continue using the managed tooltip system and must resolve above the bottom composer without overlapping their triggers.

The send and stop actions retain their existing command paths but use a stable icon-button size, solid semantic background, recognizable arrow/square glyphs, and explicit accessible names. Stop replaces Send in the same layout slot so state changes do not shift the composer.

### 6. Present workspace status and metadata semantically

The workspace status slot remains a fixed width so labels do not move, but Idle paints no glyph. Running uses a theme warning-colored progress indicator, Needs Input uses a theme information or primary indicator, and unread activity keeps its numeric badge. A failure glyph will only be rendered if a future or existing source explicitly reports a failure state; the UI will not infer failure from absence or idleness. Hard-coded RGB status colors will be removed.

For workspaces that still have the generated `New Workspace` name, the primary label will fall back to the final cwd component. The secondary path will preserve trailing components under constrained width, while tooltip and accessibility text retain the complete path.

### 7. Render Codex and Claude quota as one compact text summary

The sidebar usage control will render one progress-free line in this fixed visual sequence: `<CodexIcon> <5h remaining%> <week remaining%>  |  <ClaudeIcon> <5h remaining%> <week remaining%>`. One visible space separates the icon and values within each provider group, and two visible spaces separate each provider group from the ASCII `|` delimiter. The fixed window order avoids repeating `5h` and `week` labels in the constrained sidebar; tooltip and accessibility text will name each provider, window, and remaining-percentage semantic. An unavailable provider or individual window will retain its position and display `—` rather than collapsing the layout. The control will not render progress bars, reset text, or Fable usage.

Codex will project its existing five-hour and weekly remaining percentages into a provider-neutral usage snapshot. Claude will be collected only by running the logical command `claude -p "/usage"` through the project's existing Windows-safe Claude launch convention. The invocation will use fixed argument boundaries and no user-controlled command interpolation; on Windows it may use `cmd.exe /D /C` solely to resolve the installed `claude.cmd` shim, matching the current Claude session launcher. The application will not read OAuth credentials or call an Anthropic usage endpoint. Execution will be asynchronous, cancellable, time-bounded, and output-bounded so a missing or stalled CLI cannot block sidebar rendering.

The Claude parser will normalize line endings, remove terminal control sequences if present, and match only lines anchored by `Current session:` and `Current week (all models):`. It will accept only an integer from 0 through 100 immediately followed by `% used` and calculate `remaining = 100 - used`. It will deliberately ignore `Current week (Fable):`, reset descriptions, `Last 24h`, `Last 7d`, contribution explanations, and every other percentage in the command output. If only one required window parses, the other window remains unavailable; a non-zero exit, timeout, missing command, or fully unrecognized output makes Claude usage unavailable without erasing a valid Codex snapshot or the last successful Claude snapshot during a refresh.

The implementation will provide at least `assets/icons/codex.svg` and `assets/icons/claude.svg`. Both assets will follow the existing small `IconNamed` convention: a normalized 24-by-24 viewport, single-color rendering through `currentColor`, no embedded text, raster data, external references, or fixed brand color, and recognizable provider silhouettes at the sidebar icon size. Assets may be downloaded from official or license-compatible SVG sources during implementation; their provenance and applicable license or brand terms will be recorded, and their proportions will not be distorted.

Direct OAuth fetching was rejected because the print-mode CLI already owns authentication and subscription behavior, while copying a private endpoint would make NiumaTerm handle credentials and couple it to an undocumented transport. Interactive hidden-PTY scraping was rejected because `claude -p "/usage"` supplies bounded non-interactive text. Deriving the quota from local token/session statistics was rejected because the output explicitly states that those statistics omit other devices and claude.ai.

## Risks / Trade-offs

- [Selective prose width changes shared TextView block layout] -> Make the style opt-in with a default of `None`, cover paragraph and wide-block behavior separately, and ensure style changes invalidate cached layout.
- [Transcript row heights change after pane resizing or style changes] -> Preserve list keys and row specs, and explicitly remeasure when the effective transcript width or TextView style is not already handled by the list layout.
- [Moving the pane frame outward can break split focus borders or clipping] -> Keep the tab strip outside the pane card, remove only redundant single-pane framing, retain internal split focus state, and verify nested radii in each built-in theme.
- [A theme can make semantic actions or states too weak] -> Use semantic theme tokens rather than fixed colors and review light, dark, gray, and Ubuntu themes at narrow and wide window sizes.
- [Claude CLI wording can change] -> Anchor parsing to the two required subscription-window labels, cover the supplied output as a fixture, and render only the affected window as unavailable when a label cannot be parsed.
- [The Claude output contains unrelated percentages] -> Require the exact session and all-model weekly line prefixes and never scan the contribution section for generic percentage tokens.
- [The Claude command can be missing, slow, or fail] -> Execute it asynchronously through the existing fixed-argument platform launcher, apply cancellation plus time and output bounds, preserve prior successful values during refresh, and keep Codex failure isolation.
- [Downloaded provider marks can conflict with project styling or usage terms] -> Use official or license-compatible monochrome SVG sources, record provenance, preserve mark proportions, and normalize display through the existing `currentColor` icon pipeline.

## Migration Plan

1. Add opt-in visual primitives, a provider-neutral compact usage snapshot, the direct Claude CLI parser, and provider-icon registrations with defaults that preserve current callers.
2. Migrate Agent disclosure rows, Markdown prose styling, and composer controls to the new primitives without changing state ownership or protocol dispatch.
3. Introduce the pane-body surface beneath the existing tab strip, then adjust single-pane and split-pane framing.
4. Update workspace status, labels, path presentation, and the compact dual-provider usage control.
5. Review all built-in themes and the defined interaction scenarios before removing any transitional rendering code.

Rollback can be performed per stage because no persisted schema changes or new library dependencies are introduced. Removing the Claude projection leaves the existing Codex usage source intact.

## Open Questions

- The exact prose measure should be selected within the specified 70-90-character range during visual review; this does not change the TextView API or requirements.
