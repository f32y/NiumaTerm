# workspace-sidebar-information-hierarchy

## Purpose

Define concise, accessible workspace and provider-usage information for the sidebar using theme-derived visual semantics.

## Requirements

### Requirement: Semantic workspace status indicators
Each workspace row SHALL reserve a stable status slot. Idle SHALL render no visible status glyph. Running SHALL use a theme warning-colored progress indicator, Needs Input SHALL use a theme information or primary indicator, and unread activity SHALL remain a numeric badge. The UI SHALL NOT display a failure indicator unless its status source explicitly reports failure.

#### Scenario: Idle workspace
- **WHEN** a workspace has Idle agent status and no unread activity
- **THEN** its status slot paints no colored dot and its label remains aligned with rows that have visible status

#### Scenario: Running workspace
- **WHEN** a workspace has Running agent status
- **THEN** its status slot shows an animated progress indicator using the theme warning semantic color

#### Scenario: Workspace needs input
- **WHEN** a workspace has Needs Input agent status
- **THEN** its status slot shows a non-idle attention indicator using the theme information or primary semantic color

#### Scenario: Workspace has unread activity
- **WHEN** a workspace has one or more unread notifications
- **THEN** the row shows the existing numeric unread badge independently of its runtime status

#### Scenario: Failure is not reported
- **WHEN** a workspace is idle, disconnected, or missing activity without an explicit failure status
- **THEN** the UI does not infer or display a danger-colored failure glyph

### Requirement: Compact dual-provider quota summary
When agent usage is enabled, the sidebar SHALL render one progress-free usage summary in the fixed sequence `<CodexIcon> <Codex5hRemaining%> <CodexWeekRemaining%>  |  <ClaudeIcon> <Claude5hRemaining%> <ClaudeWeekRemaining%>`. The visible layout SHALL use one space between the icon and values inside each provider group and two spaces on each side of the ASCII `|` separator. Both provider icons SHALL retain their positions; each unavailable window SHALL display `—`. The summary SHALL NOT display progress bars, inline period labels, reset text, or Claude Fable usage. Tooltip and accessibility text SHALL name both providers, identify the five-hour and weekly values in that order, and state that percentages are remaining.

#### Scenario: Codex and Claude values are available
- **WHEN** Codex reports 25 percent and 60 percent remaining and Claude reports 97 percent used for the current session and 17 percent used for the current all-model week
- **THEN** the visible summary follows the fixed layout and displays Codex `25% 60%` and Claude `3% 83%` without progress bars or reset text

#### Scenario: One Claude window is unavailable
- **WHEN** Codex has both values and Claude has a parsed current-session value but no parsed all-model weekly value
- **THEN** the Claude group retains its icon and displays the session remaining percentage followed by `—` in the weekly position

#### Scenario: Usage refresh is in progress
- **WHEN** the user requests a usage refresh or automatic refresh is active
- **THEN** the control retains the last successful values, exposes a non-displacing loading state, and does not erase one provider because the other provider is still refreshing or failed

### Requirement: Claude quota acquisition through print-mode CLI
The application SHALL obtain Claude subscription usage only by running the logical command `claude -p "/usage"` through the project's existing platform launch convention with fixed argument boundaries and no user-controlled command interpolation. On Windows, the launcher MAY use `cmd.exe /D /C` solely to resolve an installed `claude.cmd` shim. The feature SHALL NOT contact an Anthropic usage endpoint, read Claude OAuth credentials, or derive subscription quotas from local token/session contribution statistics. Execution SHALL be asynchronous, cancellable, time-bounded, and output-bounded.

The parser SHALL normalize line endings, remove terminal control sequences if present, and extract the used percentage only from lines anchored by `Current session:` and `Current week (all models):`. For each successfully parsed integer from 0 through 100 immediately followed by `% used`, the application SHALL expose `100 - used` as the remaining percentage. It SHALL ignore `Current week (Fable):`, reset descriptions, contribution sections, and all other percentages. Each missing or invalid required line SHALL make only that Claude window unavailable; command launch failure, timeout, non-zero exit, or fully unrecognized output SHALL make Claude unavailable without invalidating Codex usage.

#### Scenario: Parse the supplied Claude output
- **WHEN** `claude -p "/usage"` emits `Current session: 97% used` and `Current week (all models): 17% used`, followed by a Fable line and contribution percentages
- **THEN** Claude five-hour remaining is 3 percent, Claude weekly remaining is 83 percent, and the Fable and contribution percentages do not affect either value

#### Scenario: Claude command is unavailable
- **WHEN** the `claude` executable cannot be resolved, exits unsuccessfully, exceeds its execution bound, or emits no recognized subscription-window lines
- **THEN** the Claude group displays `— —`, the failure is available for diagnostic logging without exposing secrets, and valid Codex values remain visible

### Requirement: Provider usage icons
The application SHALL provide at least Codex and Claude usage icons as SVG assets registered through the same icon mechanism as existing project icons. Each SHALL use a normalized 24-by-24 viewport, render as a single theme-derived color through `currentColor`, remain recognizable at the sidebar icon size, and contain no embedded text, raster payload, external reference, or fixed brand color. Downloaded assets SHALL come from official or license-compatible sources with provenance and applicable usage terms recorded, and their proportions SHALL NOT be distorted.

#### Scenario: Render provider icons across built-in themes
- **WHEN** the compact usage summary is viewed in any built-in light or dark theme
- **THEN** the Codex and Claude icons remain visually distinct, align with existing sidebar icons, inherit the intended foreground color, and retain accessible provider names

### Requirement: Recognizable generated workspace labels
When a workspace still uses the generated `New Workspace` name and has a cwd, the sidebar SHALL present the final cwd component as its primary display label. An explicitly renamed workspace SHALL retain the user's chosen name.

#### Scenario: Generated workspace name
- **WHEN** a workspace is named `New Workspace` and its cwd is `C:\Workspace\NiumaTerm`
- **THEN** the primary sidebar label is `NiumaTerm`

#### Scenario: Explicitly renamed workspace
- **WHEN** the user has renamed that workspace to `Terminal UI`
- **THEN** the primary sidebar label remains `Terminal UI` regardless of cwd

### Requirement: Tail-preserving workspace paths
The workspace path line SHALL preserve the final path components when horizontal space is constrained. The complete path SHALL remain available through tooltip and accessibility text.

#### Scenario: Long path in a narrow sidebar
- **WHEN** a workspace path is wider than the available secondary-label area
- **THEN** the visible label elides leading components and keeps the path tail, including the final directory, visible

#### Scenario: Inspect full path
- **WHEN** the user hovers the truncated workspace row or assistive technology reads it
- **THEN** the full untruncated path is available

### Requirement: Theme-derived sidebar semantics
Workspace runtime indicators, unread badges, provider usage icons, usage text, the usage separator, and selected-workspace treatment SHALL use theme semantic colors rather than fixed RGB values.

#### Scenario: Change built-in theme
- **WHEN** the user changes to any built-in theme
- **THEN** running, attention, unread, selected, and quota states remain distinguishable without retaining colors from the previous theme
