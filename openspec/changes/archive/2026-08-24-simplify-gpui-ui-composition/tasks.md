## 1. Composition Foundation

- [x] 1.1 Create the directory-form `crates/app/src/ui/composition` module with focused child files for shared metrics, static style recipes, and presentation components, and expose only items with confirmed callers.
- [x] 1.2 Add typed `StyleRefinement` recipes for the shared surface, frame, and row visuals identified in current call sites, resolving semantic colors from the active theme on every render and preserving current measurements.
- [x] 1.3 Migrate the main floating surface and right-side panel to the shared recipes, then remove only the style chains replaced in those views.
- [x] 1.4 Migrate matching settings frames and rows to the shared recipes while retaining settings-specific border edges, overflow behavior, and layout widths.

## 2. Agent Overlay Composition

- [x] 2.1 Add an Agent-view blocking-overlay component that owns full-size placement, input occlusion, backdrop styling, centering, and caller-selected padding while accepting caller-provided body content.
- [x] 2.2 Migrate the Agent update and start overlays to the blocking-overlay component without moving suspension, startup, retry, or close decisions out of `AgentPane`.
- [x] 2.3 Add or update focused Agent view tests for overlay visibility and retry or close action routing where those behaviors are not already covered.

## 3. Shared Shell Presentation

- [x] 3.1 Add a status-mark component with explicit semantic visual variants, caller-provided element identity, and caller-provided accessibility wording; migrate equivalent tab and sidebar marks without changing precedence.
- [x] 3.2 Add a hover-action component for stable close or auxiliary targets with explicit group-hover behavior, labels, glyphs, and callbacks; migrate matching horizontal-tab, vertical-tab, and workspace actions.
- [x] 3.3 Add a Shell-local inline-rename presentation helper that accepts the existing input entity and completion callbacks while preserving mouse propagation stops, Escape cancellation, text sizing, and appearance settings.
- [x] 3.4 Migrate horizontal tab rename, close, pending, unread, busy, and hover presentation to the shared pieces while keeping tab activation, scrolling, menus, and drag state in the tab-bar owner.
- [x] 3.5 Migrate workspace and vertical-tab rename, close, status, unread, active, and hover presentation to the shared pieces while keeping workspace activation, menus, dragging, and command outcomes in the sidebar owner.
- [x] 3.6 Extend Shell tests to cover meaningful status precedence, active and idle visual selection, and rename commit or cancellation paths affected by the shared components.

## 4. Module Organization

- [x] 4.1 Move `tab_bar.rs` to `tab_bar/mod.rs` with `git mv`, move its inline tests to `tab_bar/tests.rs`, and extract cohesive rendering responsibilities into child modules while retaining existing import paths.
- [x] 4.2 Move `workspace_sidebar.rs` to `workspace_sidebar/mod.rs` with `git mv`, move its inline tests to `workspace_sidebar/tests.rs`, and extract item, tab-row, and drag presentation responsibilities into child modules while retaining existing import paths.
- [x] 4.3 Review the new module boundaries, keep production files near the repository's size guideline, anchor edited imports at the crate root, and remove helpers that ended with only one meaningful caller.

## 5. Verification

- [x] 5.1 Run formatting, `cargo test -p app`, and `cargo clippy -p app --all-targets`, resolving all failures introduced by the change.
- [x] 5.2 Launch `target/debug/NiumaTerm.exe --testing` and verify shared surfaces plus Agent start and update overlays in light and dark themes.
- [x] 5.3 In the testing instance, verify horizontal and vertical tabs, active and idle status treatments, hover-close actions, rename commit and cancel, context menus, and tab or workspace dragging preserve their prior geometry and outcomes.
- [x] 5.4 Review accessibility roles and descriptions, live theme switching, element ids, group names, listener order, and repaint behavior across every migrated call site.
