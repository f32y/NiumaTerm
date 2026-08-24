## Context

See `proposal.md` for motivation. NiumaTerm currently builds GPUI element trees with fluent style calls inside each render pass. The codebase already has the pieces needed for reuse: theme semantics, `StyleRefinement`, `StyledExt::refine_style`, small render-once components, and local helpers such as the settings table styles and floating-surface frame.

Reuse is inconsistent across application views. Some repeated visuals remain inline, while larger render functions also contain state projection, element identity, accessibility descriptions, menus, drag handling, and command callbacks. A style-only layer can shorten visual chains, but repeated behavior needs a component at the narrowest common owner. Theme values must continue to be read during rendering so live theme changes do not retain stale colors.

The application layer is the correct initial home. The identified patterns carry NiumaTerm-specific visual meaning, and no missing primitive requires a change to vendored GPUI or `gpui-component`.

## Goals / Non-Goals

**Goals:**

- Make shared visual recipes named, typed, composable, and easy to locate from a call site.
- Keep state decisions and command outcomes in the view that owns them.
- Place repeated behavior in small components without hiding element ids, accessibility text, or event propagation.
- Migrate incrementally while preserving current pixel values, theme semantics, and interactions.
- Reduce oversized UI source files by responsibility, following the repository's directory-module rules.

**Non-Goals:**

- Implement CSS parsing, selectors, cascading priority, or style hot reload beyond the existing theme reload path.
- Redesign any surface, row, banner, tab, sidebar item, or interaction.
- Turn every short style chain into a helper or centralize measurements that only happen to share a value.
- Move NiumaTerm-specific components into vendored libraries.
- Introduce a universal UI framework or change application state ownership.

## Decisions

### 1. Add an application-local composition module

Create a directory-form module under `crates/app/src/ui` for shared composition code. Its module root will re-export only items with established callers, and child files will be divided by responsibility: shared metrics, static style recipes, and small presentation components. Domain-specific behavior remains under its nearest common owner, such as Agent overlays under `agent/view` and shared horizontal/vertical tab chrome near the Shell UI.

An application-local module keeps the dependency direction simple and avoids broadening vendored APIs for visuals that are not yet used outside NiumaTerm. A single large `styles.rs` at the UI root was rejected because it would collect unrelated rules and become difficult to navigate. Moving all candidates into `gpui-component` was rejected because reuse inside one application does not yet justify a public library API.

A pattern is promoted only when at least two callers share its meaning, not merely the same numeric values. One-off layout choices stay next to their view.

### 2. Represent static visual recipes with `StyleRefinement`

Static recipes will be functions that return `StyleRefinement`, using the active `App` when theme values are required. Callers apply them through the existing `StyledExt::refine_style` method. This gives a class-like call site while retaining Rust name resolution and GPUI's type checks.

Recipes may contain layout, spacing, border, radius, background, and text styling. They do not contain event listeners, element ids, child construction, or business-state tests. State-dependent styling uses a small visual enum decoded once with a `match`, or explicit active and idle recipes when the caller already knows the state. Helpers will not accept boolean switches that simply repeat a call-site decision.

Theme-derived recipes are rebuilt from `cx.theme()` during rendering. They will not cache resolved colors, because cached values could survive a live theme change. Measurements move into shared constants only when multiple consumers must remain synchronized; component-local values remain local.

An external stylesheet was rejected because GPUI has no DOM selector model and because style resolution at runtime would trade compiler diagnostics for parsing and precedence rules. A macro-based style language was also rejected: the existing fluent API and `StyleRefinement` already provide typed composition without adding new syntax.

### 3. Use components for repeated presentation behavior

Repeated element structures with presentation-level behavior will use small builders or `RenderOnce` types. Initial candidates are:

- A blocking overlay frame that owns full-size positioning, input occlusion, backdrop treatment, centering, and optional padding while the caller supplies its body.
- A status mark that owns stable glyph geometry and semantic visual variants while the caller supplies accessible wording and element identity.
- A hover action that owns a stable target and group-hover visibility while the caller supplies its id, label, glyph, and callback.
- A shared inline-rename presentation near the Shell UI that owns the input container, propagation stop, and Escape callback while the Shell retains rename state and completion commands.

These components will not read or mutate `Shell`, `AgentPane`, settings, or session state. Stable ids, labels, domain values, and command callbacks remain explicit inputs. Type erasure with `AnyElement` is deferred until a branch genuinely needs a common return type; otherwise builders return concrete elements or `impl IntoElement`.

A generic component that owns application commands was rejected because it would hide how a user action changes view state. Keeping every repeated structure inline was rejected because event propagation, accessibility behavior, and visual geometry would continue to drift independently.

### 4. Migrate low-risk visuals before complex Shell rows

The first slice will establish recipes using existing surface frames and settings presentation, then consolidate the duplicated Agent blocking overlays. These areas exercise theme-derived recipes and reusable child composition without changing drag or rename behavior.

The second slice will consolidate status marks and hover actions. The third slice will apply the established pieces to the horizontal tab bar and vertical workspace-sidebar rows, including their shared rename presentation. Only after those pieces are stable will large source files be split by responsibility. When splitting an existing single-file module, the implementation will use `git mv <name>.rs <name>/mod.rs` before adding children, retain existing import paths through re-exports, and keep new imports anchored at the crate root.

Starting with the largest Shell render functions was rejected because style extraction, component boundaries, file moves, drag behavior, and rename behavior would all change structurally at once.

### 5. Preserve current state and event ownership

Views continue to decide whether a row is active, pending, busy, unread, closeable, or being renamed. A style recipe receives only the resulting visual choice. Command-style callbacks continue to report or perform the domain action, and view-owned reactions such as focus changes, scrolling, or repaint requests remain at the view layer.

Existing element ids, group names, listener order, propagation stops, accessibility roles, labels, and keyboard handling will be carried through migration. No new deferred frame work is planned; if an extracted component later starts work outside a frame, it must both schedule that work and notify GPUI's demand-driven frame pump.

### 6. Validate behavior preservation at each slice

Automated checks will cover any non-trivial state-to-presentation mapping introduced by a component and retain existing Shell, settings, and Agent tests. Tests will target meaningful component behavior rather than wrappers around a single comparison.

Manual validation will launch `target/debug/NiumaTerm.exe --testing` and cover light and dark themes, horizontal and vertical tab modes, active and idle status treatments, hover-close actions, inline rename commit and cancel, tab and workspace dragging, Agent start and update overlays, and settings surfaces. The review will compare geometry, colors, accessibility descriptions, and command outcomes with the pre-migration behavior.

## Risks / Trade-offs

- [A shared module becomes a collection of unrelated helpers] → Require two callers with the same visual meaning, keep domain behavior near its common owner, and split child files by responsibility.
- [Migration changes pixels or theme colors] → Copy current values into the initial recipes, migrate one visual family at a time, and review both light and dark themes before removing inline styling.
- [A component hides state changes or UI reactions] → Pass explicit callbacks and visual variants; keep application state, command results, focus, scrolling, and repaint decisions in the owning view.
- [Extracted interactions change event propagation] → Preserve ids, group names, handler order, propagation stops, and keyboard paths, then manually exercise nested click, menu, drag, and rename behavior.
- [Theme values become stale] → Resolve semantic colors from the active theme on every render and share only stable geometry as constants.
- [Generic return types increase allocation or type erasure] → Prefer concrete elements and `impl IntoElement`; use `AnyElement` only where branching requires it.
- [Large file moves obscure history] → Use `git mv` before introducing directory children and keep existing public paths through the module root.

## Migration Plan

1. Add the composition module and static recipes, then migrate existing floating surfaces and settings presentation without changing measurements.
2. Add the Agent blocking-overlay component and migrate start and update overlays while keeping their body content and state decisions in `AgentPane`.
3. Add shared status-mark and hover-action components, then migrate equivalent tab and sidebar uses.
4. Consolidate the Shell-owned inline-rename presentation and migrate horizontal and vertical tab rows without changing rename state or commands.
5. Split oversized tab-bar and workspace-sidebar modules by responsibility where the extracted boundaries make the moves clear.
6. Run focused automated checks and complete the `--testing` manual scenarios after each slice.

Each slice can be reverted independently because there are no persisted-data changes, protocol changes, or new dependencies. During migration, the old inline implementation is removed only after its replacement has been validated in the same slice.
