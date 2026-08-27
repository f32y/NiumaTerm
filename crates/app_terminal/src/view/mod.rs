mod blocks;
mod events;
mod input;
mod mouse;
mod scroll;

#[cfg(test)]
mod tests;

use std::ops::Range;
use std::path::PathBuf;
use std::time::{self, Duration};

use futures::StreamExt;
use gpui::prelude::*;
use gpui::{
    AnyElement, App, AppContext, Bounds, Context, Entity, EntityInputHandler, EventEmitter,
    ExternalPaths, FocusHandle, Focusable, IntoElement, KeyDownEvent, Keystroke, ListOffset,
    Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point,
    ScrollDelta, ScrollWheelEvent, Size, UTF16Selection, Window, actions, div, list, point, px,
    rgb, size,
};
use gpui_component::WindowExt as _;
use gpui_component::notification::Notification;
use nmt_agent_utils::{AgentRoute, agent_process};
use nmt_config::local_state::TabState;
use nmt_config::{CursorShape, active_colors};
use nmt_i18n::i18n;
use nmt_terminal::block_store::BlockStore;
use nmt_terminal::ghostty::{BlockHandle, ScrollbarInfo};
use nmt_terminal::selection::SelectionType;
use tracing::{info, warn};

use crate::block_list::{
    BlockListMeasureKey, BlockListPoint, BlockListState, FrozenPoint, ListReconcile,
    RemeasureScope, block_list_active_top_px, block_list_alignment, block_list_render_metrics,
    block_pad_rows, offset_frozen_chrome, plan_list_reconcile, plan_remeasure,
    shift_selected_item_for_eviction,
};
use crate::dirty::DirtyState;
use crate::frame::{
    TerminalFrame, TerminalFrameCache, theme_default_background, theme_default_foreground,
};
use crate::layout::{
    bottom_anchor_offsets, frame_content_rows, live_frame_text, row_y_offset, terminal_row_at_y,
};
use crate::links::LinkHit;
use crate::scrollbar::{scrollbar_element, scrollbar_offset_for_thumb, scrollbar_opacity};
use crate::session::{HostEvent, InFlightBlock};
use crate::settings::TerminalSettings;
use crate::surface::{
    SurfaceCell, SurfaceCellSide, SurfaceMouseButton, SurfaceMouseEventKind, SurfaceScreenCell,
    TerminalKeyAction as SurfaceKeyAction, TerminalKeyResult as SurfaceKeyResult, TerminalSurface,
};
use crate::terminal_view::{BlockListItem, BlockListView, TerminalView};
use crate::theme::{BLOCK_GUTTER_GAP, BLOCK_GUTTER_WIDTH};
use crate::view::events::terminal_surface_for_tab;
#[cfg(test)]
use crate::view::input::dropped_paths_text;
pub(super) use crate::view::mouse::terminal_cell_at_position;
#[cfg(test)]
use crate::view::mouse::{
    block_gutter_hit, selection_drag_started, selection_type_for_click_count, terminal_scroll_lines,
};
use crate::{block_list, metrics, wake};

actions!(
    terminal,
    [
        /// Send Tab to the PTY (shell completion).
        SendTab,
        /// Send Shift-Tab to the PTY (shell backward completion).
        SendShiftTab,
        /// Copy the selected command block's command line.
        CopyBlockCommand,
        /// Copy the selected command block's output text.
        CopyBlockOutput,
        /// Re-run the selected command block's command.
        RerunBlock,
        /// Scroll the viewport to the previous command block's start.
        PreviousBlock,
        /// Scroll the viewport to the next command block's start.
        NextBlock,
    ]
);

pub struct TerminalPane {
    pub focus: FocusHandle,
    /// Surface/tab id (same value as this pane's `TabId`); the shell pump uses it
    /// to route host events to the owning tab.
    id: u64,
    profile_name: String,
    agent_route: AgentRoute,
    pub(super) surface: TerminalSurface,
    cursor_shape: CursorShape,
    frame_cache: TerminalFrameCache,
    pub(super) cell_metrics: Option<metrics::CellMetrics>,
    /// The terminal leaf's laid-out content rect (window coords, padding
    /// excluded), set from the element's paint. Resize and pointer hit-testing use
    /// it so chrome (tab bar) offsets are honored instead of assuming the window.
    pub(super) content_bounds: Option<Bounds<Pixels>>,
    /// True while the scrollbar thumb is being dragged (mouse-move then scrolls
    /// to the pointer instead of selecting text).
    pub(super) scrollbar_dragging: bool,
    /// Last user scroll action; the scrollbar stays opaque within
    /// [`gpui_component::scroll::SCROLLBAR_AUTO_HIDE_DELAY`], then fades out
    /// unless it is being dragged.
    pub(super) last_scroll_activity: Option<time::Instant>,
    /// Bumped per scroll action so only the newest hide-timer repaints.
    scroll_activity_gen: u64,
    /// Pointer offset inside the thumb at drag start (track fraction), so
    /// grabbing the thumb doesn't jump it.
    pub(super) scrollbar_grab: f32,
    wake: wake::WakeSignal,
    dirty: DirtyState,
    /// The in-flight command mirrored from the session on drain.
    in_flight: Option<InFlightBlock>,
    /// Whether a trusted prompt input region is open.
    open_prompt: bool,
    pub(super) block_list: BlockListState,
    /// Hit-test data recorded from the last native list prepaint.
    pub(super) frozen_hit: block_list::FrozenHitInfo,
    /// Inputs that affect measured heights of the mutable tail of the native list.
    last_list_measure_key: Option<BlockListMeasureKey>,
    /// The gutter-selected frozen item (block-split): highlighted and
    /// targeted by the copy/re-run/jump actions in list mode.
    pub(super) selected_frozen_item: Option<usize>,
    /// Visible frozen item chrome recorded from native list item bounds.
    pub(super) frozen_chrome: Vec<block_list::FrozenItemChrome>,
    /// Frozen-region selection: (anchor, head), both inclusive cell points.
    frozen_selection: Option<(block_list::FrozenPoint, block_list::FrozenPoint)>,
    /// Visible separator y positions, painted outside GPUI List's content mask.
    pub(super) frozen_separators: Vec<f32>,
    /// Anchor of an in-progress frozen-region drag. The selection itself is
    /// only created on the first mouse-move, so a plain click selects nothing
    /// (matching the engine's empty-selection-dropped-on-up semantics).
    frozen_select_anchor: Option<block_list::FrozenPoint>,
    /// Pixel origin of a text-selection gesture. Ignoring movement within a
    /// quarter-cell radius prevents normal hand jitter from selecting a glyph.
    selection_drag_origin: Option<Point<Pixels>>,
    /// The link under a Ctrl-hover, underlined until the pointer or the
    /// modifier leaves it.
    pub(super) hovered_link: Option<LinkHit>,
    /// Last pointer position, so a Ctrl press/release without movement can
    /// still update the hover underline.
    pub(super) last_mouse_position: Option<Point<Pixels>>,
}

pub struct AgentInterrupted;

impl EventEmitter<AgentInterrupted> for TerminalPane {}

impl TerminalPane {
    /// Launch policy is resolved by the caller: `tab_state` arrives with a
    /// concrete shell (the settings layer fills a blank one from the default
    /// profile) and `profile_name` names the profile it resolved to.
    pub fn spawn(
        cx: &mut impl AppContext,
        surface_id: u64,
        tab_state: TabState,
        profile_name: String,
    ) -> Result<Entity<Self>, String> {
        let (wake, wake_rx) = wake::wake_channel();
        let agent_route = agent_process().allocate_route();
        let environment = agent_process().environment_for(&agent_route);
        let (fixed_bottom_requested, cursor_shape, manage_process_tree) =
            cx.read_global(|settings: &TerminalSettings, _| {
                (
                    settings.fixed_bottom(),
                    settings.cursor_shape,
                    settings.manage_subprocess_job,
                )
            });

        let surface = terminal_surface_for_tab(
            &wake,
            surface_id,
            &tab_state,
            &profile_name,
            cursor_shape,
            environment,
            manage_process_tree,
        )?;

        Ok(cx.new(|cx| {
            Self::from_surface(
                cx,
                surface_id,
                profile_name,
                agent_route,
                wake,
                wake_rx,
                surface,
                fixed_bottom_requested,
                cursor_shape,
            )
        }))
    }

    /// Spawn a pane backed by an already-attached remote session. Mirrors
    /// [`Self::spawn`] but skips local-only concerns (shell profile, working
    /// dir); the remote host owns the process.
    #[cfg(windows)]
    pub fn spawn_remote(
        cx: &mut impl AppContext,
        surface_id: u64,
        remote: nmt_remote_net::RemoteSession,
    ) -> Result<Entity<Self>, String> {
        let (wake, wake_rx) = wake::wake_channel();
        let agent_route = agent_process().allocate_route();
        let (fixed_bottom_requested, cursor_shape) =
            cx.read_global(|settings: &TerminalSettings, _| {
                (settings.fixed_bottom(), settings.cursor_shape)
            });

        let surface = TerminalSurface::for_gpui_remote(wake.clone(), surface_id, remote)?;

        Ok(cx.new(|cx| {
            Self::from_surface(
                cx,
                surface_id,
                i18n("terminal-remote-profile-name").to_string(),
                agent_route,
                wake,
                wake_rx,
                surface,
                fixed_bottom_requested,
                cursor_shape,
            )
        }))
    }

    #[allow(clippy::too_many_arguments)]
    fn from_surface(
        cx: &mut Context<Self>,
        surface_id: u64,
        profile_name: String,
        agent_route: AgentRoute,
        wake: wake::WakeSignal,
        mut wake_rx: wake::WakeReceiver,
        surface: TerminalSurface,
        fixed_bottom_requested: bool,
        cursor_shape: CursorShape,
    ) -> Self {
        // Apply terminal presentation settings to existing panes and invalidate
        // measurements that depend on font metrics.
        cx.observe_global::<TerminalSettings>(|this, cx| {
            let settings = cx.global::<TerminalSettings>();
            let fixed_bottom = settings.fixed_bottom();
            let cursor_shape = settings.cursor_shape;

            this.block_list
                .list
                .set_alignment(block_list_alignment(fixed_bottom));

            this.surface.set_theme_colors(&active_colors());

            if cursor_shape != this.cursor_shape && this.surface.set_cursor_shape(cursor_shape) {
                this.cursor_shape = cursor_shape;
            }

            this.cell_metrics = None;

            this.frame_cache.invalidate_full();

            cx.notify();
        })
        .detach();

        cx.spawn(async move |this, cx| {
            while let Some(wake) = wake_rx.next().await {
                if this
                    .update(cx, |this, cx| match wake {
                        wake::Wake::Content(_) => this.invalidate(cx),
                        wake::Wake::Chrome(_) => this.invalidate_chrome(cx),
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        Self {
            focus: cx.focus_handle(),
            id: surface_id,
            profile_name,
            agent_route,
            surface,
            cursor_shape,
            frame_cache: TerminalFrameCache::default(),
            cell_metrics: None,
            content_bounds: None,
            scrollbar_dragging: false,
            last_scroll_activity: None,
            scroll_activity_gen: 0,
            scrollbar_grab: 0.0,
            wake,
            dirty: DirtyState::default(),
            in_flight: None,
            open_prompt: false,
            block_list: BlockListState::new(block_list_alignment(fixed_bottom_requested)),
            frozen_hit: Default::default(),
            last_list_measure_key: None,
            selected_frozen_item: None,
            frozen_chrome: Vec::new(),
            frozen_selection: None,
            frozen_separators: Vec::new(),
            frozen_select_anchor: None,
            selection_drag_origin: None,
            hovered_link: None,
            last_mouse_position: None,
        }
    }

    pub fn agent_route(&self) -> &AgentRoute {
        &self.agent_route
    }

    pub fn profile_name(&self) -> &str {
        &self.profile_name
    }

    fn cell_metrics(&mut self, window: &mut Window, cx: &App) -> metrics::CellMetrics {
        *self
            .cell_metrics
            .get_or_insert_with(|| metrics::measure_cell(window, cx))
    }

    /// Top-left of the terminal content in window coords (falls back to origin
    /// before the first paint).
    pub(super) fn content_origin(&self) -> Point<Pixels> {
        self.content_bounds
            .map(|bounds| bounds.origin)
            .unwrap_or_default()
    }

    /// Store the terminal leaf's laid-out content rect and resize the surface to
    /// it. Called from the element's paint, where actual bounds are known.
    pub(crate) fn set_content_bounds(
        &mut self,
        bounds: Bounds<Pixels>,
        cell: metrics::CellMetrics,
        cx: &mut Context<Self>,
    ) {
        self.content_bounds = Some(bounds);

        if self.surface.resize_for_content(
            bounds.size.width.as_f32(),
            bounds.size.height.as_f32(),
            cell,
        ) {
            self.frame_cache.invalidate();
            cx.notify();
        }
    }

    fn refresh_frame(&mut self) {
        let previous = self.frame_cache.reusable_frame();

        self.frame_cache
            .rebuild(self.surface.frame(previous.as_ref()));
    }

    fn invalidate(&mut self, cx: &mut Context<Self>) {
        self.frame_cache.invalidate();

        if self.dirty.mark() {
            cx.notify();
        }
    }

    fn invalidate_chrome(&mut self, cx: &mut Context<Self>) {
        self.frame_cache.invalidate();

        self.dirty.mark();

        // Background panes cannot clear their dirty bit by rendering, but the
        // shell observer still needs every chrome wake to refresh tab state.
        cx.notify();
    }

    /// The current frame's row offsets for pointer/IME mapping.
    pub(super) fn current_row_offsets(&self, cx: &App) -> Vec<f32> {
        if self.block_list_mode(cx) {
            return Vec::new();
        }

        let (Some(frame), Some(cell)) = (self.frame_cache.current(), self.cell_metrics) else {
            return Vec::new();
        };

        let fixed_bottom = cx.global::<TerminalSettings>().fixed_bottom();

        bottom_anchor_offsets(&frame, cell.height_px, fixed_bottom)
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn terminal_title(&self) -> String {
        self.surface.title()
    }

    /// The pane's last laid-out content size (`None` before the first paint).
    /// Split creation uses it to check the focused pane can yield the minimum
    /// panel size.
    pub fn content_size(&self) -> Option<Size<Pixels>> {
        self.content_bounds.map(|bounds| bounds.size)
    }

    /// Number of child processes in the shell's Job Object (requires the
    /// job-management setting; 0 otherwise).
    pub fn child_process_count(&self) -> usize {
        self.surface.child_process_count()
    }

    /// Whether a command is currently executing in this pane. Mirrors the
    /// session's in-flight block, so it only reports for shells whose OSC 133
    /// marks are trusted; an unintegrated shell always reads as idle.
    pub fn command_running(&self) -> bool {
        self.in_flight.is_some()
    }

    pub fn tab_state(&self) -> TabState {
        self.surface.tab_state()
    }
}

impl Focusable for TerminalPane {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl TerminalPane {}

impl Render for TerminalPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.dirty.begin_frame();

        self.wake.mark_delivered(self.id);

        // Host events are drained by the shell pump (observer), and the surface
        // is resized from the leaf's actual bounds in paint — neither happens
        // here, so background tabs and chrome offsets are handled correctly.
        let cell = self.cell_metrics(window, cx);

        if self.frame_cache.needs_rebuild() {
            self.refresh_frame();
        }

        let frame = self.frame_cache.current().unwrap_or_default();

        // Release the atlas tiles of Kitty images whose final reference dropped
        // (replaced, removed, evicted, or last frozen owner gone). Gated
        // on the lock-free live-image check so a graphics-free session never touches
        // the store here.
        if self.surface.has_live_images() {
            for image in self.surface.drain_released_images() {
                let _ = window.drop_image(image);
            }
        }

        let settings = cx.global::<TerminalSettings>();
        let fixed_bottom = settings.fixed_bottom();
        let show_block_chrome = settings.command_blocks;
        self.block_list
            .list
            .set_smooth_wheel_enabled(settings.smooth_wheel);

        // Block-split list: native GPUI list owns visibility, clamp, resize
        // anchoring, and tail following.
        let viewport_px = self
            .content_bounds
            .map(|b| b.size.height.as_f32())
            .unwrap_or(0.0);

        let block_list_element = self.render_block_list_content(&frame, cell, viewport_px, cx);

        // Auto-hide: the scrollbar stays solid briefly, then fades out.
        let scrollbar_opacity = scrollbar_opacity(
            self.scrollbar_dragging,
            self.last_scroll_activity.map(|at| at.elapsed()),
        );

        if scrollbar_opacity.is_some_and(|opacity| opacity < 1.0) {
            window.request_animation_frame();
        }

        let scrollbar_info = if block_list_element.is_some() {
            ScrollbarInfo {
                total: (self.block_list.scrollbar.1 + viewport_px).max(0.0) as u64,
                offset: self.block_list.scrollbar.0.max(0.0) as u64,
                len: viewport_px.max(0.0) as u64,
            }
        } else {
            frame.scrollbar()
        };

        // Keep the transparent track hit-testable so hovering the scrollbar
        // region can reveal it after the activity fade has completed.
        let scrollbar = scrollbar_element(scrollbar_info, scrollbar_opacity.unwrap_or(0.0), cx);

        div()
            // Stateful id: hover-end tracking (the link-underline clear
            // below) needs element state.
            .id(("terminal-pane", self.id as usize))
            .size_full()
            .relative()
            // This is the terminal region's single full-bleed background;
            // cells with explicit background colors stay opaque on top.
            .bg(rgb(theme_default_background().rgb_u32())
                .opacity(cx.global::<TerminalSettings>().background_opacity))
            // The shell frames each pane as a 1px-bordered rounded card; the
            // fill is rounded to the card's inner radius so its corners don't
            // paint square over the frame. The cell padding below keeps glyphs
            // clear of the rounded corners.
            .rounded(cx.global::<TerminalSettings>().corner_radius - px(1.))
            .text_color(rgb(theme_default_foreground().rgb_u32()))
            .font(cx.global::<TerminalSettings>().font())
            .text_size(px(metrics::font_size_px(cx)))
            .line_height(px(cell.height_px))
            .p(px(metrics::PADDING_PX))
            .overflow_hidden()
            .track_focus(&self.focus)
            .key_context("Terminal")
            .on_action(cx.listener(Self::on_send_tab))
            .on_action(cx.listener(Self::on_send_shift_tab))
            .on_action(cx.listener(Self::on_copy_block_command))
            .on_action(cx.listener(Self::on_copy_block_output))
            .on_action(cx.listener(Self::on_rerun_block))
            .on_action(cx.listener(Self::on_previous_block))
            .on_action(cx.listener(Self::on_next_block))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_modifiers_changed(cx.listener(Self::on_modifiers_changed))
            .on_drop(cx.listener(Self::on_file_drop))
            // Moving off the pane produces no further mouse-move events here,
            // so hover end is what clears a still-Ctrl-held underline.
            .on_hover(cx.listener(|this, hovered: &bool, _window, cx| {
                if !hovered {
                    this.last_mouse_position = None;
                    if this.hovered_link.take().is_some() {
                        cx.notify();
                    }
                }
            }))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Middle, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up(MouseButton::Middle, cx.listener(Self::on_mouse_up))
            .on_mouse_up(MouseButton::Right, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            .child(if let Some(list_element) = block_list_element {
                BlockListView {
                    cell,
                    focus: self.focus.clone(),
                    pane: cx.entity(),
                    list: list_element,
                    show_chrome: show_block_chrome,
                }
                .into_any_element()
            } else {
                TerminalView::new(frame, cell, self.focus.clone(), cx.entity(), fixed_bottom)
                    .into_any_element()
            })
            .children(scrollbar)
            // Ctrl-hover link underline. Rects are content-origin-relative;
            // absolute children position from the padding box, so shift by
            // the content padding.
            .when_some(self.hovered_link.as_ref(), |this, link| {
                this.cursor_pointer()
                    .children(link.rects.iter().map(|rect| {
                        div()
                            .absolute()
                            .left(rect.origin.x + px(metrics::PADDING_PX))
                            .top(rect.origin.y + px(metrics::PADDING_PX))
                            .w(rect.size.width)
                            .h(rect.size.height)
                            .bg(rgb(theme_default_foreground().rgb_u32()))
                    }))
            })
    }
}
