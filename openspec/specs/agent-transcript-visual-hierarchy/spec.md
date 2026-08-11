# agent-transcript-visual-hierarchy

## Purpose

Define a readable and consistent visual hierarchy for Agent transcripts and composer controls while preserving existing transcript behavior.

## Requirements

### Requirement: Readable prose and full-width technical content
The Agent transcript SHALL constrain natural-language prose to a readable measure of approximately 70-90 configured Agent-font characters when the pane is wide enough. Code blocks, tables, diffs, command output, and tool output SHALL remain eligible to use the full transcript width. Content SHALL shrink to the available width without forcing pane-level horizontal overflow when the pane is narrower than the preferred prose measure.

#### Scenario: Long prose on a wide pane
- **WHEN** an assistant reply contains a long natural-language paragraph and the pane is wider than the preferred reading measure
- **THEN** the paragraph wraps within approximately 70-90 configured Agent-font characters and does not span the full pane width

#### Scenario: Wide technical block
- **WHEN** the same reply contains a code block, table, diff, command output, or tool output wider than the prose measure
- **THEN** that technical block can use the available transcript width and any required overflow is handled within the block rather than by widening the transcript pane

#### Scenario: Narrow pane
- **WHEN** the Agent pane is narrower than the preferred prose measure
- **THEN** prose uses the available width with normal wrapping and remains readable without pane-level horizontal scrolling

### Requirement: Consistent disclosure rows
Turn-duration disclosures, collapsed tool-run disclosures, and expandable work rows SHALL use one chevron-first geometry with consistent height, indentation, muted foreground treatment, hover target, and expanded-content rail. Rows without expandable detail SHALL preserve alignment without displaying a misleading expansion control. These rules SHALL apply to every agent conversation the application renders, including a child agent's conversation shown outside the Agent pane.

#### Scenario: Compare disclosure types
- **WHEN** a transcript simultaneously shows a completed-turn disclosure, a collapsed tool-run disclosure, and an expandable command row
- **THEN** their chevrons occupy the same leading slot, their labels share the same reading inset, and their expanded content begins on the same detail rail

#### Scenario: Expand and collapse detail
- **WHEN** the user activates any expandable disclosure row
- **THEN** its chevron reflects the new state, only the corresponding stable transcript key changes expansion state, and surrounding rows retain their alignment

#### Scenario: Compare a parent and a child conversation
- **WHEN** the same kinds of rows appear in an Agent pane conversation and in a child agent's conversation
- **THEN** they use the same geometry, indentation, and expanded-content rail in both places

### Requirement: Clear transcript viewport edge
The transcript SHALL reserve 16-24 logical pixels above its first row. When the transcript is scrolled away from its beginning, it SHALL display a 24-pixel theme-derived top fade indicating additional content, and that fade SHALL NOT intercept text selection, link activation, or row interaction.

#### Scenario: Transcript at beginning
- **WHEN** the first transcript item is at the logical scroll origin
- **THEN** the first row has visible top breathing room and is not cut by the tab or pane boundary

#### Scenario: Transcript scrolled down
- **WHEN** the logical scroll origin is below the first item or inside a later offset
- **THEN** a top-edge fade indicates hidden earlier content while interactions with content beneath the fade continue to work

### Requirement: Distinct send and stop actions
The composer SHALL render Send and Stop in the same stable layout slot with a minimum 32-by-32 logical-pixel target. Send SHALL use a solid primary treatment and an upward-send glyph; Stop SHALL use a solid danger treatment and a square stop glyph. Each state SHALL expose an accessible name and tooltip that describes its action.

#### Scenario: Agent is idle
- **WHEN** the Agent pane is ready to accept a new turn
- **THEN** the composer shows the primary Send action with an upward-send glyph and accessible Send description

#### Scenario: Agent is running
- **WHEN** an Agent turn is running
- **THEN** Stop replaces Send without shifting the composer layout and uses the danger treatment, square glyph, and accessible Stop description

### Requirement: Hierarchical thread-setting controls
The composer SHALL present the model as the primary thread-setting control. Approval and sandbox settings SHALL be visually grouped as execution policy, while reasoning effort and service tier SHALL be visually grouped as quality and cost. The grouping SHALL NOT change option values, provider requests, persisted defaults, or keyboard interaction, and tooltips SHALL avoid covering their trigger controls.

#### Scenario: Codex thread controls
- **WHEN** a Codex Agent pane exposes model, approval, sandbox, effort, and service-tier options
- **THEN** model has the strongest control weight, approval and sandbox form one secondary group, and effort and service tier form another secondary group

#### Scenario: Claude thread controls
- **WHEN** a Claude Agent pane exposes model, permission mode, and reasoning effort
- **THEN** model has the strongest control weight and permission mode and effort remain distinguishable secondary controls

#### Scenario: Open setting tooltip or menu
- **WHEN** the user hovers a bottom-row setting or opens its menu
- **THEN** the tooltip or menu remains within the viewport, does not cover the trigger's readable value, and preserves existing selection behavior

### Requirement: Transcript interaction preservation
Agent transcripts SHALL preserve transcript virtualization, selectable user and assistant text, context-menu copying, tail-follow behavior, jump-to-latest behavior, keyboard composer actions where a composer exists, and expansion state keyed to existing transcript entries and turns. Expansion and scroll state SHALL be tracked per conversation, so one conversation's state does not affect another's.

#### Scenario: Long virtualized conversation
- **WHEN** a conversation contains enough entries to require transcript virtualization and the user scrolls, selects text, expands a work row, and jumps to the latest item
- **THEN** those interactions retain their current behavior while the visual hierarchy is applied

#### Scenario: Two conversations are shown in turn
- **WHEN** the user expands rows in one agent conversation and then views another
- **THEN** the second conversation shows its own expansion and scroll state rather than inheriting the first's
