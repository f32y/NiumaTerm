# application-chrome-visual-hierarchy

## Purpose

Define a cohesive floating-surface hierarchy for application chrome while preserving the established independent tab-strip presentation.

## Requirements

### Requirement: Shared floating-surface system
The expanded workspace sidebar and pane body SHALL use one floating-surface language with consistent external gutters, semantic borders, surface-level backgrounds, and large corner radii. The active workspace's tab strip SHALL retain its established independent styling immediately above the pane surface and SHALL NOT be wrapped in that surface or receive an additional card divider.

#### Scenario: Expanded workspace sidebar
- **WHEN** the workspace sidebar is expanded beside the main content region
- **THEN** the workspace and pane surfaces use matching border, background, radius, and applicable gutter rules while the tab strip remains visually independent

#### Scenario: Independent tab strip
- **WHEN** tabs are shown above the active pane
- **THEN** the tab strip retains its established background, spacing, and shape without being enclosed by the pane card or separated by a new card divider

#### Scenario: Single Agent or terminal pane
- **WHEN** the active tab contains a single Agent or terminal pane
- **THEN** the pane body uses the pane surface without drawing a redundant second outer card

#### Scenario: Split terminal panes
- **WHEN** the active tab contains multiple terminal panes
- **THEN** the pane surface remains the outer container while internal pane separation and focused-pane indication remain visible within its clipped bounds

### Requirement: Theme-derived chrome hierarchy
Floating surfaces, dividers, and focus borders SHALL derive their colors and radii from theme semantics rather than theme-specific hard-coded colors. The hierarchy SHALL remain recognizable in every built-in theme.

#### Scenario: Switch built-in themes
- **WHEN** the user switches among Modern Light, Modern Dark, Modern Gray, and Ubuntu
- **THEN** the peer-surface relationship and focused split pane remain distinguishable without layout changes
