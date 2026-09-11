# Terminal UI: Presentation Model and GPUI Host Separation

Date: 2026-09-11

Status: Terminal behavior moved into `nmt_terminal`; presentation state and
GPUI hosting remain in `nmt_terminal_ui`. Real-window validation remains
outstanding. Follows
[terminal-core-ui-split.md](terminal-core-ui-split.md), which moved runtime
state into `nmt_terminal` and left "presentation, GPUI integration, and window
resources" in `nmt_terminal_ui`.

## Implementation Notes

The findings and interface sketches below describe the starting point. Batches
0 through 6 first separated responsibilities within the UI crate. The follow-up
migration moves terminal behavior into the existing core crate; completing the
internal module split alone did not complete that separation.

- `nmt_terminal::input` owns the plain key description, key encoding, newline
  and clipboard chords, IME deferral classification, and wheel speed and
  rounding. `view/key.rs` converts GPUI keys, while `view/mouse.rs` converts
  pixel deltas into logical rows. Both normal and injected keys reach the core
  through the controller; the host does not select an encoder or newline rule.
- `nmt_terminal::session::interaction::TerminalInteraction` owns frozen cell
  selection, click-count selection modes, asynchronous word/line expansion,
  copy requests, and completion checks that preserve newer selections.
  `PaneController` retains the pixel drag threshold and hit testing and
  delegates cell operations to that state. Clipboard reads and writes stay in
  the UI; the core returns `PasteRequested` or `PendingCopy` without accessing
  the desktop clipboard. The host acknowledges a copy only after accepting it.
- `TerminalSession::paste_paths` quotes path lists and uses the existing paste
  handling. `TerminalSession::rerun_block` resolves and submits a command by
  block index. Both return actual queue acceptance, including read-only and
  unavailable-session rejection.
- `nmt_terminal::links` owns allowed schemes, URL matching, wrapped-row joining,
  OSC 8 interpretation, and the platform modifier rule. It returns text-row
  segments; the UI maps those segments to underline rectangles.
- `PaneController` owns End-key viewport routing, link hover, pixel selection
  gestures, and scrollbar dragging. Typing-related scrolling, agent events,
  notifications, timers, and repaint scheduling stay in `view`.
- `frame_source/items.rs` reads frozen pages, combines image placements,
  resolves image generations, and assembles frozen and live-history views.
  Elements retain GPUI layout, shaping, painting, and `FrameRecord` delivery.
- `pane_model/settings` owns settings application, cursor-shape requests and
  failure recovery, theme and duration-label updates, metric invalidation,
  and full frame invalidation. The host reads globals, updates list alignment,
  awaits pending requests, and schedules repaints.
- `pane_model/lifecycle.rs` owns measured-cell caching, content size updates,
  session resize requests, frame invalidation and rebuild, wake coalescing,
  and per-frame hit-map reset. Resetting visible records keeps persistent
  gutter selection and the live origin available when the live item is hidden.
- `Viewport` shares its grid row offsets with the element's prepaint state.
  Painting, pointer mapping, and IME placement consume the same layout;
  invalidation retains the displayed coordinates until the next render.
- `block_list/live.rs` computes live content and decoration geometry from
  history height, active rows, padding, command state, and selection.
  `FrameRecord::from_live_view` maps that result into pane coordinates;
  elements only translate the layout into GPUI bounds and shape and paint it.
- The source-level dependency check recursively covers `pane_model`,
  `block_list`, `frame`, `frame_source`, `wake`, `dirty`,
  `layout.rs`, and scrollbar geometry. The documented `SharedString` exception
  remains; this check is a guard against direct host dependencies, not a
  replacement for compiler-enforced crate boundaries. Moved input, link, and
  interaction code now compiles and runs tests in `nmt_terminal`, which has no
  dependency on `nmt_terminal_ui` or GPUI.

`TerminalLaunch`, application-owned launch assembly, remote options beside
`NetPty`, explicit settings/theme values, `Viewport`, `BlockListMirror`,
separate hit-map and gutter-selection state, and `SessionBridge` are in place.
`AgentRoute` stays a type dependency. Per-frame hit records are delivered
through `pane.update`; they no longer own persistent gutter selection.

Subsequent session work gave the PTY exclusive ownership of the engine.
Block text and selection expansion now return requests, while row reads use
immutable snapshots and cached pages. The synchronous signatures and lock
examples below are historical rather than current implementation guidance.

Automated validation covers End routing, IME delivery without duplicate text,
bracketed path paste, accepted versus rejected command replay, hover lifecycle,
scrollbar grab offsets, frozen row/image loading and image reuse. GPUI host
tests cover Escape event delivery and typing-scroll settings for key and IME
input, including rejected writes. Existing selection, viewport, frame, list,
and image tests remain in place.

Follow-up tests cover successful, rejected, canceled, and superseded cursor
updates; settings-driven metric and full-frame refresh; repeated content
sizes; retained coordinates while repaint is pending; hit-map reset without
selection loss; and live decoration placement with history and padding.

Core tests now cover key and wheel rules, wrapped URLs and allowed schemes,
word expansion followed by copy, preservation of a newer selection after an
older copy completes, cell-span clipping, quoted path paste, and block replay
with read-only rejection. UI tests retain pixel hit testing, frame and list
layout, and host reactions to the core results.

The real-window checks in the validation plan still need execution. The
Windows automation helper was unavailable during the follow-up implementation;
automated tests do not establish frame-pump or actual list-layout behavior.

## Objective

Put terminal behavior in `nmt_terminal`: input encoding, cell-based selection,
session commands, copy request lifecycle, and interpretation of terminal text.
Keep pixel geometry, frame/list presentation state, graphics resources, and
GPUI hosting in `nmt_terminal_ui`. Hosts convert device events into plain
inputs, translate cell results into visible geometry, and schedule repaints
and timers. Core behavior must be constructible and testable without a
`Window`, an `App`, or a global settings object.

The unit of migration is again a responsibility. A function that happens to
compile without `gpui` is not automatically in the right place; a function
that reads `cx.global::<TerminalSettings>()` in the middle of geometry is in
the wrong place even though it is short.

## Current Structure

Production lines exclude `*tests.rs` and profiling sources.

| Module | Lines | Talks to GPUI | Reads globals | Responsibility today |
| --- | --- | --- | --- | --- |
| `view/` | 2,125 | yes, everywhere | `TerminalSettings` x12 | Pane entity, all input handlers, block-list mirroring, scroll models, block actions, render |
| `terminal_view/` | 878 | yes | `TerminalSettings` via `block_pad_rows` | Leaf elements, item elements, paint of frames and images |
| `block_list/` | 1,360 | `ListState`, paint fns, `ListAlignment` | `TerminalSettings`, theme colors | Block-list geometry, row materialization, selection, chrome, list mirror planning |
| `frame/` | 1,040 | `SharedString`, `RenderImage` through `graphics` | `active_colors()` per frame | Display-line model, extraction from `RenderBuffer`, color resolution, frame images |
| `surface/` | 461 | none directly | `active_colors()` | Session construction with `TabState`, remote session options, clipboard, key actions, pointer row reads, live history lines, frame extraction |
| `links/` | 381 | `Point`, `Bounds`, `Modifiers` | none | URL detection, hover state, and a 190-line pane method that resolves links |
| `graphics/` | 380 | `RenderImage` | none | Image generations, release queue, frozen cache, the session observer |
| `input/` | 246 | `Keystroke`, `Modifiers` | none | Key to PTY-byte and clipboard-chord policy |
| `layout.rs` | 116 | none | none | Content rows, bottom anchoring, row offset mapping |
| `scrollbar/` | 119 | element | none | Thumb geometry, fade curve, the scrollbar element |
| `metrics/`, `settings.rs`, `paint_text.rs`, `wake/`, `dirty/`, `theme.rs` | 430 | mixed | `TerminalSettings` | Cell measurement, settings snapshot, glyph paint, wake coalescing, dirty bit, constants |

Roughly a third of the production code is a presentation model that needs no
window, a third is GPUI hosting, and the remaining third is interaction policy
that is currently written directly on the GPUI entity.

## Findings

### F1. The pane entity is the interaction state machine

`TerminalPane` carries nineteen fields: the session, the frame cache, cell
metrics, content bounds, scrollbar activity, the wake handle, the dirty bit,
the image-release flag, the in-flight command mirror, the open-prompt mirror,
the list mirror, the frozen hit map plus gutter selection, the frozen drag,
and the link hover. Every handler in `view/{input,mouse,scroll,blocks}.rs`
interleaves a policy decision with `cx.notify()` or `self.invalidate(cx)`;
there are thirty such sites across the six view files.

The consequence is that the rules that matter to users can only be exercised
through the GPUI test harness. `view/tests.rs` covers eight pure helpers
(click-count mapping, drag threshold, wheel lines, path quoting) and nothing
about, for example, "a left press in the frozen region starts a frozen
selection unless the program has enabled mouse reporting", which lives at
[`view/mouse.rs`](../../crates/terminal_ui/src/view/mouse.rs) inside
`on_mouse_down`.

### F2. The store-then-engine lock rule is enforced by comments at five UI sites

The PTY worker acquires the engine lock and then the block-store lock. Any UI
read that needs both must therefore take the store first, release it, and then
acquire the engine. That rule is currently re-implemented, each time with an
explanatory comment, at:

- `view/blocks.rs` `selected_frozen_output`, `expanded_frozen_selection`,
  `frozen_selection_to_text`
- `links/mod.rs` `link_at_position` (frozen branch)
- `terminal_view/item.rs` `prepaint` (frozen branch)

The operations behind those sites are session questions with block-store
coordinates: "the command of item N", "the text of item N", "the text between
two block points", "the semantic or line expansion of a click in a block", and
"the padded text and OSC 8 spans of one row". None of them needs pixels or a
window. `nmt_terminal` already owns `frozen_selection_range`, `acquire_block`,
and `with_screen_reader`; the missing piece is the composite operations.

### F3. Elements write into the entity during prepaint

`TerminalView::prepaint` calls `pane.update(.., set_content_bounds)`;
`BlockListView::prepaint` calls `begin_block_list_frame`;
`BlockListItem::prepaint` calls `record_frozen_view`, `record_frozen_chrome`,
and `set_active_top`, and also reads `pane.read(cx).surface.session` to acquire
engine blocks and to resolve frozen images
([`terminal_view/item.rs`](../../crates/terminal_ui/src/terminal_view/item.rs)).

Two different kinds of state are mixed inside `FrozenGutterSelection`: the
per-frame record of what was painted (row positions, chrome rectangles,
separator positions, active top), which is cleared and rebuilt every frame,
and the persistent gutter selection index that the copy, re-run, and jump
actions target. The per-frame record is a hit map; the selection is user
state. They should be separate values with separate lifetimes.

The frozen-image cache lookup at `item.rs` lines 176 to 200 (get from the
frozen cache, else read pixels from the acquired block and insert) is cache
policy that belongs on the image store, exposed as one get-or-load call.

### F4. Two viewport models branch in every handler

Classic grid mode scrolls the engine viewport; block-list mode scrolls a GPUI
list while the engine stays pinned. `block_list_mode` is consulted at thirteen
sites: scroll-to-latest, thumb scrolling, wheel handling, mouse mapping, row
offsets, IME cursor bounds, and link resolution each carry both branches.
`block_list_mode(&self, _cx: &App)` has an unused context parameter left over
from when it read a setting.

The two models answer the same questions: is the view scrolled away from the
bottom, what scroll position corresponds to a thumb fraction, where does the
live grid start, and how does a pointer position map to a cell. One value that
answers those questions for the current mode removes the branch from the
handlers.

### F5. Presentation code reads process globals mid-computation

- `BackgroundColors::new` calls `active_colors()` on every frame extraction
  ([`frame/colors.rs`](../../crates/terminal_ui/src/frame/colors.rs)).
- `theme_default_foreground()` is read per row build in `block_list/rows.rs`
  and `surface/reads.rs`; `theme_selection_background()` per paint in
  `block_list/paint.rs`.
- `block_pad_rows(cx)` in `block_list/geometry.rs` reads `TerminalSettings`
  from inside geometry; every caller must thread `cx` to reach it.
- Eighteen `cx.global::<TerminalSettings>()` reads are spread across `view/`,
  `metrics/`, and `block_list/`.

`TerminalSettings` is already a snapshot the application installs, which was
the right first step. The remaining problem is that the snapshot is consulted
at the leaves instead of once at the pane boundary. Geometry and extraction
should receive plain values (pad rows, fixed bottom, theme colors) as
arguments so they can run in tests with no global installed.

### F6. Application assembly still lives in the terminal crate

- `TerminalPane::spawn` allocates an agent route and reads the agent
  environment through `agent_process()`
  ([`view/mod.rs`](../../crates/terminal_ui/src/view/mod.rs) lines 136 and
  177). This is the only reason `nmt_terminal_ui` depends on `nmt_agent`.
- `TabState` (persisted tab layout) is constructed and consumed in
  `surface/mod.rs` and `view/events.rs`.
- `terminal_surface_for_tab` retries a restored tab without its saved cwd,
  while `crates/app/src/ui/persistence.rs` lines 531 to 545 already implement
  a three-level fallback (workspace cwd, default profile, built-in shell).
  Two retry policies are stacked; the app-level one is the one users can
  reason about.
- `TerminalSurface::for_gpui_remote` hard-codes the remote client's
  `SessionOptions` (scrollback, engine blocks off, terminal responses on).
  The remote client's terminal-response setting is a protocol decision that
  belongs next to `NetPty` in `nmt_remote_net`, where the host-side options
  already live.
- The remote profile name is localized inside the pane constructor.

### F7. `TerminalSurface` has no single responsibility

After the core split, `surface/` still holds: launch state, image caches,
grid size, frame extraction, clipboard writes, key-action application,
pointer row reads, and live-history line building. Once F2 moves the reads to
the core and F6 moves launch assembly to the application, what remains is
frame extraction plus resize plus image-cache access. That is a cohesive
"frame source" and should be named as one.

Key-action application (`apply_key_action`, `copy_selection`, `paste`) is
interaction policy with no GPUI dependency. It belongs with the other
interaction rules (F1), not with frame extraction. The previous split decided
that desktop clipboard access stays outside the core; this proposal keeps that
decision and only moves the code within the UI crate.

### F8. `link_at_position` mixes four concerns

The 190-line method in `links/mod.rs` resolves the pointer to a row source
(pixel geometry), reads rows through the surface (engine access), joins
soft-wrapped neighbours and matches a URL (pure text logic), and builds
underline rectangles (pixel geometry). The URL logic is already pure
(`url_at_col`, `open_allowed`) but the join-and-map step that decides which
row segments carry the URL is not separable today. The pure part should
return the URL plus row-relative segments; the geometry part should map
segments to rectangles through the viewport model of F4.

### F9. `render_block_list_content` mirrors, measures, and renders in one method

The 147-line method in `view/blocks.rs` computes store metrics, decides how to
patch the GPUI `ListState` (using the already-pure `plan_list_reconcile` and
`plan_remeasure`), installs the scroll handler, updates the scrollbar mirror
and active top, and then constructs the `list()` element with a closure that
captures nine values. The planning half already returns plans; the application
half should be a short loop over those plans, and the element construction
should be a separate function.

### F10. Glob imports hide the dependency structure

`use crate::view::*;` (five files), `use crate::block_list::*;` (seven files),
and `use crate::terminal_view::*;` (two files) mean that `view/blocks.rs`,
`view/mouse.rs`, and `view/input.rs` show zero GPUI import lines while using
`AnyElement`, `ListOffset`, `Window`, `Context`, and `px`. Any layering rule,
whether enforced by review or by a test, needs explicit imports first. This
is mechanical preparation.

### F11. The session observer is named after one of its four duties

`graphics::session::SessionImages` implements `SessionObserver::graphics`,
`blocks`, `clipboard`, and `changed`. It is the UI crate's adapter for the
core session: image installation, frozen-cache pruning, OSC 52 clipboard
writes, and wake delivery. Naming it as an image store hides the clipboard and
wake duties from readers of `surface/mod.rs`.

### F12. Smaller mixes

- `ScrollbarActivity::mark_activity(cx)` spawns the auto-hide timer from
  inside the state struct (`view/scroll.rs`). The state should record the
  activity and return the generation; the pane schedules the timer.
- `block_list/chrome.rs` holds both `item_header`/`format_duration` (pure)
  and `paint_frozen_chrome`/`paint_frozen_separators` (GPUI).
  `block_list/reconcile.rs` holds both pure planning and `BlockListState`,
  which owns a `ListState`. `block_list/geometry.rs` returns
  `gpui::ListAlignment`.
- `input/mod.rs` is keyed on `gpui::Keystroke`. The policy is already pure
  and tested; a plain key description would let it compile without GPUI, but
  this is the lowest-value change in the list.

## Target Ownership

Three tiers inside `nmt_terminal_ui`, plus two moves out of it.

```text
nmt_terminal (additions)
  input/               plain keys, terminal encoding, IME classification,
                        wheel speed and rounding
  links/               allowed URLs, wrapped-row resolution, row segments
  session/interaction/ frozen cell selection, pending expansion, copy
                        requests and completion, key command dispatch
  session/input.rs     path paste and block replay alongside text input
  session/blocks.rs     BlockPoint, block_command, block_text,
                        frozen_selection_text, expand_frozen_selection
  session/rows.rs       RowText (padded text, wrapped flag, OSC 8 spans),
                        screen_row_text, block_row_text

nmt_remote_net
  net_pty               remote client SessionOptions and a constructor that
                        returns TerminalSession

crates/app
  terminal launch       TabState -> TerminalLaunch, cwd retry, agent route
                        allocation, remote profile name

nmt_terminal_ui
  presentation model (no gpui, no globals)
    frame/              unchanged shape; colors and theme passed in
    block_list/         geometry, rows, selection, images, chrome labels,
                        list-mirror planning (ListOp values, no ListState)
    layout.rs, scrollbar/geometry.rs, wake/, dirty/
    pane_model/         PaneSettings, FrameTheme, Viewport, FrozenHitMap,
                        GutterSelection, pixel drag origin, LinkHover,
                        ScrollbarActivity (no timer), TerminalFrameCache,
                        PaneController and its outcome enums
    frame_source/       what remains of TerminalSurface: session + images +
                        grid size; frame(), resize_for_content(),
                        live_history_lines(), frozen image get-or-load
    session_bridge/     the SessionObserver implementation (images, frozen
                        pruning, clipboard, wake)

  gpui host
    view/               TerminalPane: entity fields, listeners that convert
                        GPUI events to plain inputs, outcome application,
                        ListState ownership and ListOp application, timers,
                        notifications, render
    terminal_view/      elements; prepaint returns records instead of
                        mutating the pane where possible
    paint/              paint_text.rs, block_list paint, image paint,
                        scrollbar element
    metrics.rs          measure_cell (GPUI) beside CellMetrics (plain)
    settings.rs         TerminalSettings global and PaneSettings::from
```

### Disposition table

| Existing code | Disposition |
| --- | --- |
| `surface/reads.rs` pointer rows | Core `RowText` reads |
| `view/blocks.rs` block text, command, selection expansion, frozen text | Core block operations on `BlockPoint` |
| `block_list::FrozenPoint`, `frozen_selection_pieces` | Core `BlockPoint`; pieces become an internal step of `frozen_selection_text` |
| `surface/mod.rs` `TabState`, `restorable_tab_state`, `tab_state_with_cwd` | Application launch module; pane stores the restorable state it is given |
| `view/events.rs` `terminal_surface_for_tab` | Delete; the app's existing fallback chain covers it |
| `view/mod.rs` agent route allocation | Application; pane receives the route and environment |
| `surface/mod.rs` `for_gpui_remote` options | `nmt_remote_net::net_pty` |
| `surface/input.rs` key actions and clipboard | Core input and interaction; clipboard access in UI |
| `surface/mod.rs` `frame`, `resize_for_content`, image cache access | `frame_source` |
| `graphics/session.rs` | `session_bridge`, renamed to say what it is |
| `frame/colors.rs` `active_colors()` per frame | `FrameTheme` argument built once per settings change |
| `block_list/geometry.rs` `block_pad_rows(cx)` | Plain `pad_rows: f32` argument from `PaneSettings` |
| `block_list/geometry.rs` `block_list_alignment` | View |
| `block_list/reconcile.rs` `BlockListState` | View (`ListState` owner); planning stays |
| `block_list/chrome.rs` paint functions | `paint/` |
| `view/mouse.rs` selection rules and drag geometry | Core cell-selection state; pixel geometry in `pane_model` |
| `view/scroll.rs` `ScrollbarActivity` | `pane_model`, timer scheduling moves to view |
| `links/mod.rs` URL logic and `LinkHover` | URL logic in core `links`; hover state in `pane_model` |
| `links/mod.rs` `link_at_position` | Core text resolution; pointer and rectangle mapping via `Viewport` |
| `view/blocks.rs` `render_block_list_content` | Planning in `pane_model` returning `ListOp`s; application and element construction in view |
| `terminal_view/item.rs` frozen image resolution | `frame_source` get-or-load |
| `view/*` handlers | `PaneController` maps geometry and delegates terminal behavior to core; view applies outcomes |

## Interface Sketches

These are shapes to check against call sites during each batch, not final
signatures.

### Core additions

```rust
// nmt_terminal::session
pub struct BlockPoint { pub item: usize, pub line: usize, pub col: u32 }

pub struct RowText {
    pub text: String,                     // padded to grid width
    pub wrapped: bool,
    pub hyperlinks: Vec<(u16, u16, String)>,
}

impl TerminalSession {
    pub fn block_command(&self, item: usize) -> Option<String>;
    pub fn block_text(&self, item: usize) -> Option<String>;
    pub fn frozen_selection_text(&self, a: BlockPoint, b: BlockPoint) -> String;
    pub fn expand_frozen_selection(&self, at: BlockPoint, kind: SelectionType)
        -> Option<(BlockPoint, BlockPoint)>;
    pub fn screen_row_text(&self, row: u32) -> Option<RowText>;
    pub fn block_row_text(&self, item: usize, row: usize) -> Option<RowText>;
}
```

Each of these takes the store lock, copies the handle, releases the store,
then acquires the engine. The rule is written once, in the crate that also
owns the worker that defines it.

### Launch value

```rust
// nmt_terminal_ui::view
pub struct TerminalLaunch {
    pub config: TerminalSessionConfig,   // already includes env overrides
    pub restorable: TabState,            // what tab_state() reports back
    pub profile_name: String,
    pub agent_route: AgentRoute,         // allocated by the caller
}

impl TerminalPane {
    pub fn spawn(cx, surface_id, launch: TerminalLaunch) -> Result<Entity<Self>, String>;
    pub fn spawn_remote(cx, surface_id, session: TerminalSession, profile_name: String, agent_route: AgentRoute) -> Entity<Self>;
}
```

`AgentRoute` remains a type dependency on `nmt_agent` unless the application
keeps its own pane-to-route map. The type-only dependency is acceptable for
the first pass; removing it is an application change and is listed under
decisions.

### Settings and theme snapshot

```rust
#[derive(Clone, Copy)]
pub(crate) struct PaneSettings {
    pub fixed_bottom: bool,
    pub pad_rows: f32,          // ITEM_PAD_ROWS or 0.0
    pub show_block_chrome: bool,
    pub smooth_wheel: bool,
    pub scroll_to_bottom_when_typing: bool,
    pub newline_shortcut: NewlineShortcut,
    pub cursor_shape: CursorShape,
}

#[derive(Clone, Copy)]
pub(crate) struct FrameTheme {
    pub foreground: TerminalColor,
    pub background: TerminalColor,
    pub selection_background: TerminalColor,
    pub palette: nmt_config::colors::List,
}
```

The pane rebuilds both inside its existing `observe_global::<TerminalSettings>`
callback and stores them; handlers and the frame source receive them as
arguments. `BackgroundColors::new(term_colors, &theme)` replaces the global
read.

### Viewport model

```rust
pub(crate) enum Viewport {
    Grid { scrollbar: ScrollbarInfo, row_offsets: Vec<f32> },
    BlockList { scroll_px: f32, max_scroll_px: f32, active_top: f32, viewport_px: f32 },
}

impl Viewport {
    fn is_scrolled(&self) -> bool;
    fn scrollbar_info(&self) -> ScrollbarInfo;
    fn live_origin_y(&self) -> f32;               // 0 or active_top
    fn cell_at(&self, local: (f32, f32), cell: CellMetrics) -> (SurfaceCell, SurfaceCellSide);
    fn cursor_y(&self, row: u16, cell_h: f32) -> f32;
}
```

The controller builds one `Viewport` per frame from the frame cache and the
list metrics; the thirteen `block_list_mode` branches collapse into the two
constructors.

### List mirror planning

```rust
pub(crate) enum ListOp {
    Reset(usize),
    Splice(Range<usize>, usize),
    RemeasureAll,
    Remeasure(Range<usize>),
    ScrollTo { item_ix: usize, offset_px: f32 },
    ScrollToEnd,
}

pub(crate) struct BlockListMirror { item_count: usize, evicted_items: u64, last_measure_key: Option<..> }

impl BlockListMirror {
    fn sync(&mut self, metrics: &BlockListRenderMetrics, layout: (u32, f32, f32), live_rows: usize) -> Vec<ListOp>;
    fn scroll_to_px(&self, store: &BlockStore, frame: &TerminalFrame, ..., target: f32) -> ListOp;
}
```

The view applies `ListOp`s to its `ListState` in one `match`.

### Controller outcomes

Each handler returns a small enum that names what happened; the view decides
the reaction. Examples:

```rust
pub(crate) enum KeyOutcome {
    Ignored,
    Written,                 // react: maybe scroll to latest, invalidate
    Copied,                  // react: notification, invalidate
    ScrolledToLatest,        // react: notify
    FrozenCopied,            // react: notification, clear drag, notify
}

pub(crate) enum MouseDownOutcome {
    Ignored,
    OpenUrl(String),
    FrozenSelectionStarted,  // react: invalidate + notify
    FrozenSelectionCleared,  // react: notify
    EngineHandled,           // react: invalidate
}
```

"Interrupts the agent" is a property of the keystroke (plain Escape) that the
view can compute itself before calling the controller; it does not need to be
an outcome.

## Migration Plan

| Batch | Change | Validation focus |
| --- | --- | --- |
| 0 | Replace glob imports with explicit ones; move paint functions out of `block_list/chrome.rs`; move `BlockListState` to view | Compiles; no behavior change |
| 1 | Core `BlockPoint`, `RowText`, and the six session operations; delete the five UI lock-order sites | Copy block output and command, frozen drag copy, double and triple click in a frozen block, Ctrl+click on a URL that wraps across rows in a frozen block |
| 2 | `TerminalLaunch`; app builds it and allocates the agent route; delete `terminal_surface_for_tab`; remote options move to `nmt_remote_net`; pane stores restorable state | Restore a workspace whose saved cwd no longer exists; open a remote tab; agent notification routing; `tab_state()` still reports the last cwd |
| 3 | `PaneSettings` and `FrameTheme` built in the settings observer; remove `block_pad_rows(cx)`, per-frame `active_colors()`, per-row theme reads | Switch theme while output scrolls; toggle Command Blocks and fixed-bottom on a pane with history; frame extraction unit tests run with no global installed |
| 4 | `Viewport`, `BlockListMirror` with `ListOp`s, `FrozenHitMap` separated from `GutterSelection`, frozen image get-or-load on the frame source | Thumb drag in both modes, End key in both modes, IME candidate placement in both modes, gutter selection after eviction |
| 5 | `PaneController` with outcome enums; move `FrozenSelectionDrag`, `LinkHover`, `ScrollbarActivity`, `TerminalFrameCache`, `DirtyState`, in-flight mirror, key actions into it; `TerminalPane` keeps entity fields, listeners, `ListState`, timers | Table-driven controller tests: mouse reporting on/off, drag threshold, link click precedence over selection, Escape interrupt, scroll-to-bottom-when-typing |
| 6 | Rename `TerminalSurface` to the frame source; rename the observer; optional plain key description for `input/` | No behavior change |

Each batch keeps the application running. Batches 1 and 2 are independent of
each other and of 3; batch 4 depends on 3 (it needs `pad_rows` as a value);
batch 5 depends on 4.

A layering check can be added after batch 3 as a unit test that walks the
`presentation model` module paths and fails on a `gpui` import. It is cheap,
runs with `cargo test`, and gives the same guarantee a crate split would give
without forcing `TerminalLine` and `ImageGeneration` to become generic over
the string and image types.

## Validation Plan

Automated, per batch:

- Batch 1: core tests for each new operation, including the "store released
  before engine" property (a test that holds the store lock on another thread
  while calling `block_text` must not deadlock; today that would deadlock if
  the order were wrong).
- Batch 3: `frame/tests.rs` and `block_list/tests.rs` run with no
  `TerminalSettings` global and no active colors installed; theme values come
  from the test.
- Batch 4: `Viewport` unit tests for pointer mapping and thumb math in both
  modes; `BlockListMirror::sync` tests replace the current
  `plan_list_reconcile`/`plan_remeasure` tests and add `ScrollTo` planning.
- Batch 5: controller tests as listed in the migration table; the existing
  `view/tests.rs` helper tests move with their helpers.

Manual, with `--testing`, because the GPUI harness models neither the frame
pump nor real list layout:

- Quiet window after each batch: no repaint loop with cursor blink disabled.
- Scrollbar fade after a wheel scroll, in both modes.
- Frozen selection dragged past the live boundary clamps to the last frozen
  row.
- A restored tab whose cwd is gone falls back once, through the app policy.

Performance: the frame-extraction profile from the previous split should be
re-run after batch 3, since `BackgroundColors` construction changes from a
global read to an argument. No change is expected; the measurement is the
evidence.

## Non-goals

- No new crate in this pass. Terminal behavior uses the existing
  `nmt_terminal` crate. `TerminalLine` holds a `SharedString` for
  `shape_line_by_hash`, and `FrameImage` holds a GPUI `RenderImage`; those
  rendering values stay in `nmt_terminal_ui` without generic replacements.
- Block-list geometry, display ordering, and shaping caches stay in the UI
  crate, as the previous split decided.
- No event bus. Outcome enums are per handler and consumed immediately.
- No change to what the core session exposes for the remote host or the
  session hub.

## Decisions Requiring Implementation-Level Review

- Whether `AgentRoute` stays a type dependency of `nmt_terminal_ui` or the
  application keeps a pane-to-route map. The former is a one-line `Cargo`
  entry; the latter touches `agent_notifications.rs`, `close.rs`, `pump.rs`,
  and `shell/mod.rs`.
- How elements hand their per-frame records to the controller. Keeping one
  `pane.update` entry point that records a `FrameRecord` value is the smallest
  change; a shared per-frame cell handed to the item elements avoids the
  entity write but adds a lifetime to reason about. Decide in batch 4 against
  the `list()` closure that constructs the items.
- Whether `frozen_selection_text` joins pieces with `\n` in the core or
  returns pieces. Returning the joined string matches every current caller.
- Whether `RowText` padding to the grid width belongs in the core read or in
  the URL matcher. The matcher relies on equal-width segments for its
  row-to-rectangle mapping; padding in the core keeps the matcher pure.
- Whether the remote profile name is passed in by the application or looked
  up through `nmt_i18n` in the pane. Passing it in removes the pane's last
  reason to know that a session is remote.
