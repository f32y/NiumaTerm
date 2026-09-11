mod blocks;
mod events;
mod input;
pub(crate) mod links;
mod list_state;
mod mouse;
mod scroll;
#[cfg(test)]
mod tests;

use std::sync::Arc;

use futures::StreamExt;
use gpui::prelude::*;
use gpui::{
    App, AppContext, Bounds, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement,
    MouseButton, Pixels, Point, Size, Window, actions, div, px, rgb,
};
use nmt_agent::AgentRoute;
use nmt_config::active_colors;
use nmt_config::local_state::TabState;
use nmt_terminal::session::{EngineError, SessionObserver, TerminalSession, TerminalSessionConfig};

use crate::frame_source::TerminalFrameSource;
use crate::pane_model::{FrameTheme, PaneController, PaneSettings};
use crate::scrollbar::scrollbar_element;
use crate::settings::{TerminalSettings, duration_labels};
use crate::terminal_view::{BlockListView, TerminalView};
use crate::view::list_state::{BlockListState, block_list_alignment};
use crate::{metrics, wake};

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

pub struct TerminalLaunch {
    pub config: TerminalSessionConfig,
    pub restorable: TabState,
    pub profile_name: String,
    pub agent_route: AgentRoute,
}

struct PaneIdentity {
    /// Surface/tab id (same value as this pane's `TabId`); the shell pump uses it
    /// to route host events to the owning tab.
    id: u64,
    profile_name: String,
    restorable: TabState,
    agent_route: AgentRoute,
}

pub struct TerminalPane {
    pub focus: FocusHandle,
    identity: PaneIdentity,
    pub(crate) model: PaneController,
    /// The terminal leaf's laid-out content rect (window coords, padding
    /// excluded), set from the element's paint. Resize and pointer hit-testing use
    /// it so chrome (tab bar) offsets are honored instead of assuming the window.
    pub(super) content_bounds: Option<Bounds<Pixels>>,
    wake: wake::WakeSignal,
    image_releases_attached: bool,
    pub(super) block_list: BlockListState,
}

pub struct AgentInterrupted;

impl EventEmitter<AgentInterrupted> for TerminalPane {}

impl TerminalPane {
    pub fn spawn(
        cx: &mut impl AppContext,
        surface_id: u64,
        launch: TerminalLaunch,
    ) -> Result<Entity<Self>, String> {
        let (wake, wake_rx) = wake::wake_channel();
        let source = TerminalFrameSource::for_gpui(
            wake.clone(),
            surface_id,
            launch.config,
            active_colors(),
        )?;
        let identity = PaneIdentity {
            id: surface_id,
            profile_name: launch.profile_name,
            restorable: launch.restorable,
            agent_route: launch.agent_route,
        };
        Ok(cx.new(|cx| Self::from_source(cx, identity, wake, wake_rx, source)))
    }

    /// Install the observer before starting the session so its first output,
    /// images and wake notifications reach the pane being constructed.
    pub fn attach(
        cx: &mut impl AppContext,
        surface_id: u64,
        profile_name: String,
        agent_route: AgentRoute,
        connect: impl FnOnce(Arc<dyn SessionObserver>) -> Result<TerminalSession, EngineError>,
    ) -> Result<Entity<Self>, String> {
        let (wake, wake_rx) = wake::wake_channel();
        let source = TerminalFrameSource::attach(wake.clone(), surface_id, connect)?;
        let identity = PaneIdentity {
            id: surface_id,
            profile_name,
            restorable: TabState::default(),
            agent_route,
        };
        Ok(cx.new(|cx| Self::from_source(cx, identity, wake, wake_rx, source)))
    }

    fn from_source(
        cx: &mut Context<Self>,
        identity: PaneIdentity,
        wake: wake::WakeSignal,
        mut wake_rx: wake::WakeReceiver,
        surface: TerminalFrameSource,
    ) -> Self {
        // Apply terminal presentation settings to existing panes and invalidate
        // measurements that depend on font metrics.
        cx.observe_global::<TerminalSettings>(|this, cx| {
            let settings = PaneSettings::from(cx.global::<TerminalSettings>());
            let colors = active_colors();
            this.block_list
                .list
                .set_alignment(block_list_alignment(settings.fixed_bottom));

            this.model.source.session.set_theme_colors(&colors);

            if settings.cursor_shape != this.model.settings.cursor_shape {
                let previous = this.model.settings.cursor_shape;
                let requested = settings.cursor_shape;
                let request = this.model.source.session.set_cursor_shape(requested);
                cx.spawn(async move |this, cx| {
                    if !matches!(request.await, Ok(Ok(()))) {
                        let _ = this.update(cx, |this, cx| {
                            if this.model.settings.cursor_shape == requested {
                                this.model.settings.cursor_shape = previous;
                                this.invalidate(cx);
                                cx.notify();
                            }
                        });
                    }
                })
                .detach();
            }

            this.model.settings = settings;
            this.model.theme = FrameTheme::from(&colors);
            this.model.duration_labels = duration_labels();
            this.model.cell_metrics = None;

            this.model.frame_cache.invalidate_full();

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

        let settings = cx.global::<TerminalSettings>();
        let fixed_bottom_requested = settings.fixed_bottom();

        Self {
            focus: cx.focus_handle(),
            identity,
            model: PaneController::new(
                surface,
                PaneSettings::from(settings),
                FrameTheme::from(&active_colors()),
                duration_labels(),
            ),
            content_bounds: None,
            wake,
            image_releases_attached: false,
            block_list: BlockListState::new(block_list_alignment(fixed_bottom_requested)),
        }
    }

    pub fn agent_route(&self) -> &AgentRoute {
        &self.identity.agent_route
    }

    pub fn profile_name(&self) -> &str {
        &self.identity.profile_name
    }

    fn cell_metrics(&mut self, window: &mut Window, cx: &App) -> metrics::CellMetrics {
        *self
            .model
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
        self.model.content_size = (bounds.size.width.as_f32(), bounds.size.height.as_f32());
        self.model.cell_metrics = Some(cell);
        self.model.update_viewport();

        if self.model.source.resize_for_content(
            bounds.size.width.as_f32(),
            bounds.size.height.as_f32(),
            cell,
        ) {
            self.model.frame_cache.invalidate();
            cx.notify();
        }
    }

    fn invalidate(&mut self, cx: &mut Context<Self>) {
        self.model.frame_cache.invalidate();

        if self.model.dirty.mark() {
            cx.notify();
        }
    }

    fn invalidate_chrome(&mut self, cx: &mut Context<Self>) {
        self.model.frame_cache.invalidate();

        self.model.dirty.mark();

        // Background panes cannot clear their dirty bit by rendering, but the
        // shell observer still needs every chrome wake to refresh tab state.
        cx.notify();
    }

    pub fn id(&self) -> u64 {
        self.identity.id
    }

    pub fn terminal_title(&self) -> String {
        self.model.source.session.title()
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
        self.model.source.session.child_process_count()
    }

    /// Whether a command is currently executing in this pane. Mirrors the
    /// session's in-flight block, so it only reports for shells whose OSC 133
    /// marks are trusted; an unintegrated shell always reads as idle.
    pub fn command_running(&self) -> bool {
        self.model.in_flight.is_some()
    }

    pub fn tab_state(&self) -> TabState {
        let mut state = self.identity.restorable.clone();
        if let Some(cwd) = self.model.source.session.current_directory() {
            state.cwd = Some(cwd);
        }
        state
    }
}

impl Focusable for TerminalPane {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TerminalPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.image_releases_attached {
            let queue = self.model.source.images.generations.lock().release_queue();
            if let Some(mut releases) = queue.lock().attach() {
                let handle = window.window_handle();
                // The task owns no pane or generation references. It drains through
                // the original window until the final generation releases its sender.
                cx.spawn(async move |_, cx| {
                    while let Some(image) = releases.next().await {
                        if handle
                            .update(cx, |_, window, _| {
                                let _ = window.drop_image(image);
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                })
                .detach();
            }
            self.image_releases_attached = true;
        }

        self.model.dirty.begin_frame();

        self.wake.mark_delivered(self.identity.id);

        // Host events are drained by the shell pump (observer), and the surface
        // is resized from the leaf's actual bounds in paint — neither happens
        // here, so background tabs and chrome offsets are handled correctly.
        let cell = self.cell_metrics(window, cx);

        if self.model.frame_cache.needs_rebuild() {
            self.model.refresh_frame();
        }

        let frame = self.model.frame_cache.current().unwrap_or_default();

        let fixed_bottom = self.model.settings.fixed_bottom;
        let show_block_chrome = self.model.settings.show_block_chrome;

        self.block_list
            .list
            .set_smooth_wheel_enabled(self.model.settings.smooth_wheel);

        // Block-split list: native GPUI list owns visibility, clamp, resize
        // anchoring, and tail following.
        let viewport_px = self
            .content_bounds
            .map(|b| b.size.height.as_f32())
            .unwrap_or(0.0);

        let block_list_element = self.render_block_list_content(&frame, cell, viewport_px, cx);

        // Auto-hide: the scrollbar stays solid briefly, then fades out.
        let scrollbar_opacity = self.model.scrollbar.opacity();

        if scrollbar_opacity.is_some_and(|opacity| opacity < 1.0) {
            window.request_animation_frame();
        }

        let scrollbar_info = self.model.viewport.scrollbar_info();

        // Keep the transparent track hit-testable so hovering the scrollbar
        // region can reveal it after the activity fade has completed.
        let scrollbar = scrollbar_element(scrollbar_info, scrollbar_opacity.unwrap_or(0.0), cx);

        div()
            // Stateful id: hover-end tracking (the link-underline clear
            // below) needs element state.
            .id(("terminal-pane", self.identity.id as usize))
            .size_full()
            .relative()
            // This is the terminal region's single full-bleed background;
            // cells with explicit background colors stay opaque on top.
            .bg(rgb(self.model.theme.background.rgb_u32())
                .opacity(cx.global::<TerminalSettings>().background_opacity))
            // The shell frames each pane as a 1px-bordered rounded card; the
            // fill is rounded to the card's inner radius so its corners don't
            // paint square over the frame. The cell padding below keeps glyphs
            // clear of the rounded corners.
            .rounded(cx.global::<TerminalSettings>().corner_radius - px(1.))
            .text_color(rgb(self.model.theme.foreground.rgb_u32()))
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
                    this.model.links.forget_position();

                    if this.model.links.clear() {
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
            .when_some(self.model.links.current(), |this, link| {
                this.cursor_pointer()
                    .children(link.rects.iter().map(|rect| {
                        div()
                            .absolute()
                            .left(px(rect.origin.x + metrics::PADDING_PX))
                            .top(px(rect.origin.y + metrics::PADDING_PX))
                            .w(px(rect.width))
                            .h(px(rect.height))
                            .bg(rgb(self.model.theme.foreground.rgb_u32()))
                    }))
            })
    }
}
