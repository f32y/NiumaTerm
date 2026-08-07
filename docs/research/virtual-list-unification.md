# Virtual List Unification in Terminal Pane and Agent Pane

**Status:** Research conclusion

**Date:** 2026-08-07

**Scope:** `TerminalPane`, `AgentPane`, vendored GPUI list elements, and the
vendored `gpui-component::VirtualList`

## Executive Summary

NiumaTerm should not replace all list virtualization in Terminal Pane and
Agent Pane with `gpui-component::VirtualList`.

The current implementations are not interchangeable copies of the same
mechanism. They cover three materially different layout models:

1. Dynamically measured, variable-height rows whose heights can change after
   insertion.
2. Very large collections of uniform-height rows.
3. Rows whose exact, potentially different sizes are already known before
   layout.

`gpui-component::VirtualList` is a good fit only for the third model. It
requires the caller to provide every item size up front and does not provide
the bottom alignment, continuous tail following, incremental splice,
remeasurement, or scroll-anchor preservation required by the primary Terminal
and Agent lists.

The recommended direction is to standardize the component-selection policy,
not the component implementation:

- Use `gpui::list` for unknown or changing variable-height content.
- Use `gpui::uniform_list` for large uniform-height content.
- Use `gpui-component::VirtualList` for pre-sized heterogeneous content.

## Current Virtualized Surfaces

| Surface | Current implementation | Important requirements | Replace with `VirtualList`? |
| --- | --- | --- | --- |
| Terminal block list | `gpui::ListState` and `gpui::list` | Variable block heights, front eviction, resize anchoring, top/bottom alignment, live-tail following | No |
| Rows inside a large terminal block | Custom visible-row calculation and custom painting | Partial engine reads, shaped-line caching, images, selection, terminal lock ordering | No |
| Agent conversation transcript | `gpui::ListState` and `gpui::list` | Layout-measured Markdown, streaming growth, expansion/collapse, bottom alignment, tail following | No |
| Expanded long technical output | `gpui::uniform_list` | Very large uniform rows, independent vertical and horizontal scrolling, widest-row width measurement | No practical benefit; behavior would regress |
| Agent session history | `gpui-component::v_virtual_list` | Known 28-pixel rows, bounded viewport, visible-range pagination | Already uses it |

The command palette is bounded to a small number of visible entries and is
not part of the large-list virtualization problem.

## What `gpui-component::VirtualList` Provides

The vendored component accepts an `Rc<Vec<Size<Pixels>>>` containing the size
of every item before layout. It then derives a full size and origin array and
renders only the calculated visible range.

This design is appropriate when item sizes are authoritative model data. The
Agent session-history list is a valid example: every row is exactly 28 pixels
high, and the component can calculate the complete scroll geometry without
rendering the rows first.

The current implementation has several constraints relevant to the proposed
unification:

- It has no item measurement feedback. A row cannot be rendered, measured,
  and then incorporated into the list's height index.
- It has no `splice` or range invalidation API.
- It has no equivalent of `ListAlignment::Bottom`.
- `scroll_to_bottom` is a one-shot scroll request, not continuous follow-tail
  behavior that disengages when the user scrolls upward and re-engages at the
  bottom.
- It has no remeasurement anchor for preserving the user's position when item
  heights change after resize or content updates.
- It has no configurable pixel overdraw comparable to `gpui::ListState`.
- It stores per-item sizes and origins and scans from the beginning to locate
  the visible range during prepaint. Uniform lists can calculate the same
  range arithmetically without per-item storage or a linear scan.
- A vertical list derives its horizontal content width by measuring the first
  item. It cannot select a known widest item in the way `uniform_list` can.

Relevant implementation:

- [`VirtualList` API and required item sizes](../../third_party/gpui-component/ui/src/virtual_list.rs#L123)
- [Full size and origin preparation](../../third_party/gpui-component/ui/src/virtual_list.rs#L384)
- [Visible-range scans](../../third_party/gpui-component/ui/src/virtual_list.rs#L627)

## Terminal Pane Assessment

### Outer Block List

Terminal Pane renders completed engine blocks followed by one live tail item.
Block heights are computable from cached engine row counts, cell height, and
presentation padding. This means the outer list could technically construct a
`Vec<Size<Pixels>>` for `VirtualList`, but known heights alone are not enough to
make it an equivalent replacement.

The block list also relies on the following `ListState` behavior:

- `ListAlignment::Bottom` for fixed-bottom presentation.
- `FollowMode::Tail` so new terminal output remains pinned while the user is at
  the live tail, but manual scrollback remains stable.
- Logical `ListOffset` positions rather than raw pixel offsets.
- Front splices when old frozen blocks are evicted.
- Tail splices when the previous live item becomes frozen and a new live item
  is appended.
- Partial remeasurement when the last frozen block or live tail grows.
- Full remeasurement with proportional anchoring after layout inputs change.

The implementation explicitly delegates visibility, clamping, resize
anchoring, and tail following to GPUI's native list:

- [Block-list state and tail mode](../../crates/app/src/terminal/block_list.rs#L40)
- [Store-to-list reconciliation](../../crates/app/src/terminal/block_list.rs#L1689)
- [Render-time remeasurement and native list construction](../../crates/app/src/terminal/view.rs#L1626)

Replacing this list with the current `VirtualList` would require application
code to reconstruct all of those behaviors around a raw pixel scroll handle.
That would increase, rather than reduce, the amount of custom list logic.

There is also a scaling difference. `VirtualList` linearly scans the item-size
array to find visible items. Native `ListState` stores measured items in a
`SumTree`, allowing it to seek and update the variable-height sequence without
walking every preceding block on every frame.

### Virtualization Inside a Terminal Block

A single terminal block can contain far more rows than the viewport. The outer
list therefore cannot be the only virtualization boundary. Once GPUI selects a
visible block item, NiumaTerm calculates which physical terminal rows intersect
the viewport plus overdraw and reads only that range from the engine.

Those rows are not ordinary UI list entries. Their custom element integrates:

- engine-block acquisition and lock-order constraints;
- shaped-line caching;
- terminal selection and hit testing;
- frozen Kitty image placement;
- the active grid and live scrollback seam;
- custom paint ordering for backgrounds, glyphs, images, and block chrome.

See:

- [Visible terminal-row calculation](../../crates/app/src/terminal/block_list.rs#L460)
- [Frozen block range acquisition](../../crates/app/src/terminal/element.rs#L382)
- [Live-history range acquisition](../../crates/app/src/terminal/element.rs#L469)

Turning these rows into nested `VirtualList` items would replace a specialized
partial-read and custom-paint path with many general-purpose elements. It would
not remove the terminal-specific state or geometry and would likely add layout
and allocation cost.

## Agent Pane Assessment

### Main Conversation Transcript

Transcript rows include Markdown, prose, code, tool disclosures, errors,
folded-turn headers, and a growing working row. Their heights are not model
data. Width-dependent text wrapping and TextView layout determine the final
height.

The transcript maintains data-only `RowSpec` values and synchronizes them with
`ListState`:

- Equal row counts with changed fingerprints trigger `remeasure_items`.
- Structural changes trigger `splice`.
- Font or viewport-width changes trigger full remeasurement.
- Bottom alignment and follow-tail implement chat-log behavior.
- A user reading older content is not moved by streamed output.
- Jump to Latest re-engages tail following and reaches the bottom of a growing
  final row.

See:

- [Transcript `ListState` initialization](../../crates/app/src/ui/agent_pane.rs#L576)
- [Width-change remeasurement](../../crates/app/src/ui/agent_pane.rs#L3003)
- [RowSpec synchronization](../../crates/app/src/ui/agent_pane.rs#L3537)

Using `VirtualList` would require an authoritative height for every row before
the row is laid out. The alternatives are all undesirable:

1. Render all rows to obtain their heights, defeating virtualization.
2. Use estimates, producing incorrect scroll geometry and jumps when the real
   heights become known.
3. Add a measurement cache, incremental size updates, splice semantics, and
   scroll anchoring to `VirtualList`, effectively rebuilding `ListState`.

### Expanded Long Technical Output

Large code-oriented tool output is segmented into UTF-8-safe rows with a fixed
line height and rendered through `gpui::uniform_list`. This is the ideal case
for uniform-list arithmetic: the visible indices can be derived directly from
the scroll offset and line height, even for very large transcripts.

The current implementation also records the widest segment and asks
`uniform_list` to use that row for horizontal width measurement. This preserves
independent horizontal scrolling for long command output, diffs, and file
contents.

See [the expanded-output uniform list](../../crates/app/src/ui/agent_pane.rs#L3996).

A `VirtualList` replacement would require a repeated size entry for every
segment, retain per-item origins, and linearly scan for the viewport. Its
vertical mode would measure the first item rather than the known widest item,
so horizontal scrollbar geometry would be wrong when a later line is longer.
The replacement is technically possible only after extending the component,
and it would still be a worse algorithm for uniform rows.

### Session History

The session-history list already uses `v_virtual_list`. All row heights are
known, only ten rows are visible by default, and the visible-range callback is
used to request another Codex history page when the final loaded row comes into
view.

See [the session-history virtual list](../../crates/app/src/ui/agent_pane.rs#L4354).

Although `uniform_list` would also fit the fixed-height geometry, there is no
compelling reason to migrate this bounded list merely to reduce the number of
component types.

## Recommended Architecture Rule

The codebase should treat virtualization choice as a property of the data and
layout contract:

| Data and interaction contract | Preferred implementation |
| --- | --- |
| Height is unknown until layout, may change later, or requires anchored remeasurement | `gpui::list` |
| All rows have the same height and the list may be very large | `gpui::uniform_list` |
| Every item has a reliable size before layout and sizes may differ | `gpui-component::VirtualList` |
| Rows are engine-owned painted primitives rather than independent UI elements | Specialized visible-range/custom-paint path |

This policy preserves the optimized implementation for each case while making
future choices consistent and reviewable.

## Reasonable Consolidation Opportunities

Consolidation can still happen above the virtualization engines:

1. Reuse a consistent overlay-scrollbar layout and styling where product
   behavior is the same.
2. Centralize list IDs, viewport wrappers, and common empty/loading treatments
   when those abstractions do not hide scroll semantics.
3. Document the selection rule above near shared UI guidance.
4. Add behavior-focused regression tests for tail following, resize anchoring,
   front eviction, transcript expansion, and nested scroll isolation.

The gpui-component scrollbar already adapts `ScrollHandle`,
`UniformListScrollHandle`, `ListState`, and `VirtualListScrollHandle`, so Agent
Pane can share scrollbar presentation without sharing the list engine. Terminal
Pane's custom scrollbar additionally covers terminal-mode switching and its
auto-hide interaction, which should remain a separate product-level concern.

Avoid introducing a facade that claims all list engines have the same state
model. Such a facade would either expose every implementation-specific feature
or conceal required behavior behind fragile special cases.

## What Full Unification Would Require

If NiumaTerm deliberately chose to turn `gpui-component::VirtualList` into the
single list engine, it would first need at least:

- bottom and top alignment;
- stateful follow-tail behavior;
- logical item-relative scroll positions;
- incremental splice, including front eviction;
- per-range size invalidation and post-layout measurement feedback;
- absolute and proportional remeasurement anchors;
- configurable overdraw;
- efficient prefix-sum indexing rather than linear viewport scans;
- a selectable width-measurement item or an explicit horizontal content size;
- scroll-state queries and lifecycle callbacks.

At that point the component would substantially duplicate native GPUI
`ListState`, while still needing a specialized fast path equivalent to
`uniform_list`. This is a component redesign and long-term fork maintenance
commitment, not a mechanical replacement project.

## Historical Context

The repository previously investigated this exact choice for Terminal Pane.
Commit `6a3237e` concluded that gpui-component `VirtualList` lacked the sizing,
anchoring, bottom-alignment, and tail-follow semantics needed by the block
list. Commit `c14619a` then moved the split terminal history to GPUI's native
list so measurement, clamping, resize anchoring, and tail following were owned
by `ListState`.

Agent Pane independently made the same choice for its transcript in commit
`b7315a7`: the native variable-height list replaced unvirtualized transcript
rendering and consolidated hand-written bottom-follow behavior into
`FollowMode::Tail`.

The current `VirtualList` source still lacks the capabilities that motivated
those decisions, so the earlier conclusion remains applicable.

## Conclusion

Keep the current division of responsibility. `VirtualList` should remain the
tool for lists with authoritative precomputed sizes, not the universal list
primitive for NiumaTerm.

The project can achieve consistency through a documented selection rule,
shared presentation helpers, and behavior tests. Replacing the native
variable-height and uniform-height list engines with `VirtualList` would create
more custom state management, weaker asymptotic behavior in large fixed-row
lists, and regressions in the interaction semantics that Terminal Pane and
Agent Pane intentionally preserve.
