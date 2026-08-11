## MODIFIED Requirements

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

### Requirement: Transcript interaction preservation
Agent transcripts SHALL preserve transcript virtualization, selectable user and assistant text, context-menu copying, tail-follow behavior, jump-to-latest behavior, keyboard composer actions where a composer exists, and expansion state keyed to existing transcript entries and turns. Expansion and scroll state SHALL be tracked per conversation, so one conversation's state does not affect another's.

#### Scenario: Long virtualized conversation
- **WHEN** a conversation contains enough entries to require transcript virtualization and the user scrolls, selects text, expands a work row, and jumps to the latest item
- **THEN** those interactions retain their current behavior while the visual hierarchy is applied

#### Scenario: Two conversations are shown in turn
- **WHEN** the user expands rows in one agent conversation and then views another
- **THEN** the second conversation shows its own expansion and scroll state rather than inheriting the first's
