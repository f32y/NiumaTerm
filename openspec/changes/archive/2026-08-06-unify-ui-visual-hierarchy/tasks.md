## 1. Shared Visual and Data Primitives

- [x] 1.1 Add an opt-in prose maximum width to `TextViewStyle`, apply it to prose block types while leaving code and tables full width, and include the new field in style equality or layout invalidation.
- [x] 1.2 Add focused TextView coverage for constrained paragraphs, unconstrained technical blocks, narrow containers, and the unchanged default style without executing the test suite unless explicitly authorized.
- [x] 1.3 Introduce an application-level Agent disclosure-row helper with fixed chevron, optional type-icon, content, and trailing-status slots plus a shared expanded-detail inset.
- [x] 1.4 Add pure helpers for generated workspace display labels and tail-preserving workspace paths, keeping full source strings available to callers.
- [x] 1.5 Add a provider-neutral compact usage projection with optional five-hour and weekly remaining percentages, adapting the existing Codex values without changing their source.
- [x] 1.6 Add a Claude usage collector that runs the logical command `claude -p "/usage"` through the existing platform launch convention with fixed argument boundaries, reusing the Windows `cmd.exe /D /C` compatibility path only for `claude.cmd`, and applying cancellation plus explicit execution and output bounds without OAuth access.
- [x] 1.7 Add a pure Claude output parser that normalizes line endings and terminal control sequences, anchors only `Current session:` and `Current week (all models):`, converts 0-100 `% used` integers to remaining percentages, and ignores Fable, reset, and contribution percentages.
- [x] 1.8 Source license-compatible `codex.svg` and `claude.svg` assets, record their provenance, normalize them to the project's 24-by-24 single-color `currentColor` convention without distorting proportions, and register them through the existing icon mechanism.
- [x] 1.9 Add focused pure-logic coverage for workspace label/path formatting, Codex projection, the supplied Claude output, partial/malformed output, unrelated percentage rejection, and CLI failure mapping without executing the test suite unless explicitly authorized.

## 2. Agent Transcript and Composer Hierarchy

- [x] 2.1 Apply the Agent prose measure to assistant Markdown while preserving full-width code blocks, tables, diffs, command output, and tool output at wide and narrow pane widths.
- [x] 2.2 Migrate completed-turn, collapsed tool-run, and work-detail rows to the shared disclosure geometry without changing `expanded_turns`, `expanded_groups`, `expanded_rows`, or row-spec keys.
- [x] 2.3 Increase transcript top spacing, derive scrolled-from-top state from the list's logical offset, and add a non-interactive theme-derived top fade only while earlier content is hidden.
- [x] 2.4 Give Send and Stop a stable minimum 32-by-32 target, solid semantic variants, recognizable glyphs, tooltips, and accessible names without changing send or interrupt dispatch.
- [x] 2.5 Split thread settings into a primary model picker, an execution-policy group, and a quality/cost group for Codex and Claude while preserving menus, wire values, persisted defaults, and keyboard behavior.
- [x] 2.6 Ensure transcript width or style changes remeasure virtualized rows as needed and preserve text selection, context-menu copy, tail follow, jump to latest, and disclosure interaction.

## 3. Main Pane Surface

- [x] 3.1 Introduce a floating pane-body surface beneath the independently styled tab strip, using the workspace card's semantic border, background, large radius, and applicable gutters.
- [x] 3.2 Remove redundant single-pane outer framing and retain clipped internal separators and focused-pane borders for split terminal layouts.

## 4. Workspace Status and Information

- [x] 4.1 Keep a fixed workspace status slot but render Idle with no glyph, Running with the theme warning progress indicator, and Needs Input with a theme information or primary indicator.
- [x] 4.2 Remove hard-coded RGB status colors, retain unread count badges independently of runtime status, and avoid rendering failure state without an explicit failure source.
- [x] 4.3 Display the final cwd component for workspaces that still use `New Workspace`, preserve user-renamed labels, and retain full names for rename and persistence behavior.
- [x] 4.4 Render secondary workspace paths with leading elision that preserves trailing components, while exposing the complete path through tooltip and accessibility text.
- [x] 4.5 Replace usage rows and progress bars with the exact compact sequence `<CodexIcon> <5h remaining%> <week remaining%>  |  <ClaudeIcon> <5h remaining%> <week remaining%>`, using `—` for each unavailable window and omitting Fable and reset text.
- [x] 4.6 Refresh Codex and Claude independently, preserve last successful values during refresh, keep loading feedback from displacing the compact sequence, and prevent one provider's failure from erasing the other.

## 5. Theme, Accessibility, and Regression Coverage

- [x] 5.1 Review the new surfaces, disclosure geometry, actions, status indicators, provider icons, usage text, and separator against existing semantic theme tokens and adjust built-in theme values only where the shared semantics lack sufficient contrast.
- [x] 5.2 Add or update accessibility labels for transcript disclosures, Send and Stop, workspace states, full workspace paths, and the fixed provider/window/remaining usage semantics.
- [x] 5.3 Add focused UI or render-state coverage for disclosure slot consistency, idle-status suppression, composer state replacement, peer-surface framing, and compact provider usage without executing tests unless explicitly authorized.
- [x] 5.4 Document a manual validation matrix covering Modern Light, Modern Dark, Modern Gray, and Ubuntu; narrow, standard, and wide windows; long prose plus code/table/diff output; many tool rows; single and split panes; Idle, Running, Needs Input, and unread workspaces; and full, partial, refreshing, failed, and unavailable Codex/Claude usage states.
- [x] 5.5 When explicitly authorized by the user, run the focused automated checks required for the changed modules and record the exact commands and results.
- [x] 5.6 When explicitly authorized by the user, build and launch `target\debug\NiumaTerm.exe --testing` for the manual validation matrix and record any accepted visual tuning decisions.
