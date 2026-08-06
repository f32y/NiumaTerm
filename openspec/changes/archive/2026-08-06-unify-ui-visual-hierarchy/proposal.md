## Why

NiumaTerm's current interface mixes chat, terminal, and application-chrome conventions without a consistent hierarchy, which makes long agent conversations tiring to read and makes important actions and states harder to recognize than decorative elements. The UI audit in `target/audit.txt` identifies the same inconsistency across the transcript, workspace sidebar, composer controls, and usage metadata, so the visual system should be corrected as one coordinated change rather than as isolated styling patches.

## What Changes

- Constrain prose to a readable measure while allowing code blocks, tables, diffs, and tool output to use the available width.
- Standardize turn, tool-run, and tool-detail disclosures around one chevron-first row geometry and one expansion rail.
- Prevent transcript content from appearing clipped beneath the tab area and strengthen send and stop affordances.
- Give the workspace and pane body a shared floating-card language while retaining the established independent tab-strip styling above the pane card.
- Suppress idle status decoration and reserve semantic color for running, attention, unread, and supported failure states.
- Give the model picker stronger visual weight than execution-policy and quality/cost controls while preserving their existing behavior.
- Replace quota progress bars with one compact Codex-and-Claude usage summary in fixed icon, five-hour, and weekly order; report remaining percentages, render unavailable windows as em dashes, and pair this with meaningful default workspace labels and tail-preserving paths.
- Preserve theme configurability, transcript virtualization, text selection, keyboard behavior, tab/workspace dragging, and accessibility labels while applying the new hierarchy.

## Capabilities

### New Capabilities

- `agent-transcript-visual-hierarchy`: Defines the visual and interaction contract for readable prose, wide technical content, disclosures, transcript edges, composer controls, and send/stop actions.
- `application-chrome-visual-hierarchy`: Defines the shared floating-container system and its theme-derived hierarchy.
- `workspace-sidebar-information-hierarchy`: Defines semantic workspace status presentation, explicit usage metadata, and recognizable workspace labels and paths.

### Modified Capabilities

None.

## Impact

- Agent UI rendering in `crates/app/src/ui/agent_pane.rs`, including virtualized row composition and composer controls.
- Shared Markdown rendering styles in the vendored `gpui-component` text view, with any new prose-width option remaining opt-in.
- Shell, pane framing, tab-strip placement, and workspace sidebar rendering in `crates/app/src/ui/`.
- Codex usage presentation, direct Claude CLI usage collection and output parsing, and provider-icon assets in `crates/agent_utils`, `crates/app`, and `assets/icons`.
- Built-in themes only where semantic tokens need adjustment; the change adds no new library dependency and treats an unavailable `claude` command as unavailable Claude usage.
