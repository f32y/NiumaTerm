mod key;
mod list_state;

#[cfg(test)]
mod tests;

use std::ops::Range;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use gpui::prelude::*;
use gpui::{
    AnyElement, App, AppContext, Bounds, Context, Entity, EntityInputHandler, EventEmitter,
    ExternalPaths, FocusHandle, Focusable, IntoElement, KeyDownEvent, Keystroke, Modifiers,
    ModifiersChangedEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    Point, ScrollDelta, ScrollWheelEvent, Size, UTF16Selection, Window, actions, div, list, point,
    px, rgb, size,
};
use gpui_component::WindowExt as _;
use gpui_component::notification::Notification;
use nmt_agent::AgentRoute;
use nmt_config::active_colors;
use nmt_config::local_state::TabState;
use nmt_terminal::clipboard::{Clipboard, ClipboardType};
use nmt_terminal::input::WheelDelta;
use nmt_terminal::session::interaction::PendingCopy;
use nmt_terminal::session::{
    EngineError, HostEvent, SessionObserver, SurfaceMouseButton, TerminalSession,
    TerminalSessionConfig,
};
use rust_i18n::t;
use tracing::warn;

use crate::terminal_tab::block_list::live::LiveItemState;
use crate::terminal_tab::frame::TerminalFrame;
use crate::terminal_tab::frame_source::TerminalFrameSource;
use crate::terminal_tab::metrics::CellMetrics;
use crate::terminal_tab::pane_model::key_action::{KeyOutcome, TextInput};
use crate::terminal_tab::pane_model::list_mirror::ListPosition;
use crate::terminal_tab::pane_model::mouse::{MouseInput, MouseOutcome};
use crate::terminal_tab::pane_model::scroll::ScrollOutcome;
use crate::terminal_tab::pane_model::viewport::LocalPoint;
use crate::terminal_tab::pane_model::{ClipboardAccess, PaneController, PaneSettings};
use crate::terminal_tab::scrollbar::geometry::SCROLLBAR_AUTO_HIDE_DELAY;
use crate::terminal_tab::scrollbar::scrollbar_element;
use crate::terminal_tab::settings::{TerminalSettings, duration_labels};
use crate::terminal_tab::terminal_view::{BlockListItem, BlockListView, TerminalView};
use crate::terminal_tab::view::key::{modifiers_state, terminal_key};
use crate::terminal_tab::view::list_state::{BlockListState, block_list_alignment};
use crate::terminal_tab::{metrics, wake};

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
    pub(super) model: PaneController,

    /// The terminal leaf's laid-out content rect (window coords, padding
    /// excluded), set from the element's paint. Resize and pointer hit-testing use
    /// it so chrome (tab bar) offsets are honored instead of assuming the window.
    pub(super) content_bounds: Option<Bounds<Pixels>>,

    wake: wake::WakeSignal,
    image_releases_attached: bool,
    pub(super) block_list: BlockListState,
}

pub struct AgentInterrupted;

struct TextCopiedNotification;

struct DesktopClipboard;

impl ClipboardAccess for DesktopClipboard {
    fn read(&mut self) -> Option<String> {
        let text = Clipboard::default().get(ClipboardType::Clipboard);

        (!text.is_empty()).then_some(text)
    }

    fn write(&mut self, text: String) -> bool {
        Clipboard::default().set(ClipboardType::Clipboard, text)
    }
}

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
            let settings: PaneSettings = cx.global::<TerminalSettings>().into();
            let colors = active_colors();

            this.block_list
                .list
                .set_alignment(block_list_alignment(settings.fixed_bottom));

            if let Some(update) = this
                .model
                .update_settings(settings, &colors, duration_labels())
            {
                cx.spawn(async move |this, cx| {
                    if let Some(failure) = update.failure().await {
                        let _ = this.update(cx, |this, cx| {
                            if this.model.cursor_shape_failed(failure) {
                                cx.notify();
                            }
                        });
                    }
                })
                .detach();
            }

            cx.notify();
        })
        .detach();

        cx.spawn(async move |this, cx| {
            while let Some(wake) = wake_rx.next().await {
                if this
                    .update(cx, |this, cx| match wake {
                        wake::Wake::Content(_) => this.invalidate(cx),

                        wake::Wake::Chrome(_) => {
                            this.model.invalidate();

                            // Background panes cannot clear their dirty bit by rendering, but the
                            // shell observer still needs every chrome wake to refresh tab state.
                            cx.notify();
                        }
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
                settings.into(),
                (&active_colors()).into(),
                duration_labels(),
                Box::new(DesktopClipboard),
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
        self.model
            .cell_metrics_or_measure(|| metrics::measure_cell(window, cx))
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
    pub(super) fn set_content_bounds(
        &mut self,
        bounds: Bounds<Pixels>,
        cell: metrics::CellMetrics,
        cx: &mut Context<Self>,
    ) {
        self.content_bounds = Some(bounds);

        if self.model.resize_content(
            bounds.size.width.as_f32(),
            bounds.size.height.as_f32(),
            cell,
        ) {
            cx.notify();
        }
    }

    fn invalidate(&mut self, cx: &mut Context<Self>) {
        if self.model.invalidate() {
            cx.notify();
        }
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

    pub(super) fn begin_block_list_frame(
        &mut self,
        bounds: Bounds<Pixels>,
        cell: CellMetrics,
        cx: &mut Context<Self>,
    ) {
        self.set_content_bounds(bounds, cell, cx);
        self.model.begin_block_list_frame();
    }

    fn on_copy_block_command(
        &mut self,
        _: &CopyBlockCommand,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(command) = self.model.selected_block_command()
            && self.model.copy_text_to_clipboard(command)
        {
            cx.notify();
        }
    }

    fn on_copy_block_output(
        &mut self,
        _: &CopyBlockOutput,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(copy) = self.model.selected_block_output() {
            self.begin_copy(copy, window, cx);
        }
    }

    fn on_rerun_block(&mut self, _: &RerunBlock, _: &mut Window, cx: &mut Context<Self>) {
        if self.model.write_text_input(TextInput::RerunSelectedBlock) {
            self.invalidate(cx);
        }
    }

    fn on_previous_block(&mut self, _: &PreviousBlock, _: &mut Window, cx: &mut Context<Self>) {
        let outcome = self.model.jump_to_block(-1);

        self.apply_scroll_outcome(outcome, cx);
    }

    fn on_next_block(&mut self, _: &NextBlock, _: &mut Window, cx: &mut Context<Self>) {
        let outcome = self.model.jump_to_block(1);

        self.apply_scroll_outcome(outcome, cx);
    }

    fn render_block_list_content(
        &mut self,
        frame: &TerminalFrame,
        cell: CellMetrics,
        viewport_px: f32,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let offset = self.block_list.list.logical_scroll_top();

        let plan = self.model.prepare_block_list(
            frame,
            cell,
            viewport_px,
            ListPosition {
                item_ix: offset.item_ix,
                offset_px: offset.offset_in_item.as_f32(),
            },
        )?;

        for op in plan.ops {
            self.block_list.apply(op);
        }

        if !self.block_list.scroll_handler_set {
            let pane = cx.entity();

            self.block_list.list.set_scroll_handler(move |_, _, cx| {
                pane.update(cx, |pane, cx| pane.mark_scrollbar_activity(cx));
            });

            self.block_list.scroll_handler_set = true;
        }

        Some(self.block_list_element(
            frame,
            cell,
            plan.cols,
            plan.history_rows,
            plan.live_index,
            cx,
        ))
    }

    fn block_list_element(
        &self,
        frame: &TerminalFrame,
        cell: CellMetrics,
        cols: u32,
        history_rows: u64,
        live_index: usize,
        cx: &Context<Self>,
    ) -> AnyElement {
        let frame_for_items = frame.clone();
        let in_flight_for_items = self.model.in_flight.clone();
        let has_open_prompt_for_items = self.model.open_prompt;
        let selected_frozen_item = self.model.gutter.selected();
        let frozen_selection = self.model.interaction.block_selection();
        let cell_for_items = cell;
        let pane_for_items = cx.entity();
        let store_for_items = self.model.source.session.block_store();

        list(self.block_list.list.clone(), move |ix, _window, _cx| {
            if ix < live_index {
                BlockListItem::Frozen {
                    item_idx: ix,
                    store: store_for_items.clone(),
                    cols,
                    cell: cell_for_items,
                    selection: frozen_selection,
                    selected_item: selected_frozen_item,
                    pane: pane_for_items.clone(),
                }
                .into_any_element()
            } else {
                BlockListItem::Live {
                    frame: frame_for_items.clone(),
                    history_rows,
                    state: LiveItemState {
                        index: live_index,
                        in_flight: in_flight_for_items.clone(),
                        has_open_prompt: has_open_prompt_for_items,
                        selected_item: selected_frozen_item,
                    },
                    cols,
                    cell: cell_for_items,
                    pane: pane_for_items.clone(),
                }
                .into_any_element()
            }
        })
        .size_full()
        .into_any_element()
    }

    pub fn drain_host_events(&mut self) -> Vec<HostEvent> {
        self.model.drain_host_events()
    }

    fn begin_copy(&mut self, copy: PendingCopy, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| match copy.request.await {
            Ok(Ok(text)) => {
                let _ = this.update_in(cx, |this, window, cx| {
                    if this.model.finish_copy(text, copy.completion) {
                        window.push_notification(
                            Notification::new()
                                .message(t!("terminal-text-copied"))
                                .id::<TextCopiedNotification>()
                                .autohide_after(Duration::from_millis(1500))
                                .show_close(false)
                                .w_auto()
                                .px_3()
                                .py_2(),
                            cx,
                        );

                        this.invalidate(cx);

                        cx.notify();
                    }
                });
            }

            result => warn!("terminal copy did not complete: {result:?}"),
        })
        .detach();
    }

    /// UI reaction to input reaching the PTY: optionally snap the view back
    /// to the latest output.
    fn react_to_pty_input(&mut self, cx: &mut Context<Self>) {
        if self.model.settings.scroll_to_bottom_when_typing {
            let outcome = self.model.scroll_to_latest();

            self.apply_scroll_outcome(outcome, cx);
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let interrupts_agent = matches!(event.keystroke.key.as_str(), "escape" | "esc")
            && !event.keystroke.modifiers.modified();

        match self.model.key_down(&terminal_key(&event.keystroke)) {
            KeyOutcome::Ignored => return,

            KeyOutcome::Scrolled(outcome) => {
                self.apply_scroll_outcome(outcome, cx);

                return;
            }

            KeyOutcome::Written => self.react_to_pty_input(cx),

            KeyOutcome::CopyPending(copy) => {
                self.begin_copy(copy, window, cx);

                return;
            }
        }

        if interrupts_agent {
            cx.emit(AgentInterrupted);
        }

        self.invalidate(cx);
    }

    /// Route a keystroke straight to the terminal PTY.
    pub(super) fn feed_terminal_key(&mut self, keystroke: &Keystroke, cx: &mut Context<Self>) {
        match self.model.send_key(&terminal_key(keystroke)) {
            KeyOutcome::Ignored => return,

            KeyOutcome::Scrolled(outcome) => {
                self.apply_scroll_outcome(outcome, cx);

                return;
            }

            KeyOutcome::Written => self.react_to_pty_input(cx),

            KeyOutcome::CopyPending(copy) => {
                cx.spawn(async move |this, cx| {
                    if let Ok(Ok(text)) = copy.request.await {
                        let _ = this.update(cx, |this, cx| {
                            if this.model.finish_copy(text, copy.completion) {
                                this.invalidate(cx);

                                cx.notify();
                            }
                        });
                    }
                })
                .detach();
            }
        }

        self.invalidate(cx);
    }

    /// Tab/Shift-Tab belong to the shell (completion) while the terminal is
    /// focused, but `Root` binds them to focus traversal and key bindings
    /// dispatch before the pane's `on_key_down` listener. These actions are
    /// bound in the deeper `Terminal` context, which wins over `Root`.
    fn on_send_tab(&mut self, _: &SendTab, _: &mut Window, cx: &mut Context<Self>) {
        self.feed_terminal_key(
            &Keystroke {
                modifiers: Modifiers::none(),
                key: "tab".into(),
                key_char: None,
            },
            cx,
        );
    }

    fn on_send_shift_tab(&mut self, _: &SendShiftTab, _: &mut Window, cx: &mut Context<Self>) {
        self.feed_terminal_key(
            &Keystroke {
                modifiers: Modifiers::shift(),
                key: "tab".into(),
                key_char: None,
            },
            cx,
        );
    }

    fn on_file_drop(&mut self, paths: &ExternalPaths, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);

        if self
            .model
            .write_text_input(TextInput::DropPaths(paths.paths()))
        {
            self.invalidate(cx);
        }
    }

    fn on_modifiers_changed(
        &mut self,
        event: &ModifiersChangedEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .model
            .hover_modifiers_changed(modifiers_state(event.modifiers))
        {
            cx.notify();
        }
    }

    pub(super) fn local_position(&self, position: Point<Pixels>) -> LocalPoint {
        let origin = self.content_origin();

        LocalPoint {
            x: (position.x - origin.x).as_f32(),
            y: (position.y - origin.y).as_f32(),
        }
    }

    fn mouse_input(
        &self,
        position: Point<Pixels>,
        button: Option<MouseButton>,
        modifiers: Modifiers,
        click_count: usize,
    ) -> MouseInput {
        MouseInput {
            position: self.local_position(position),
            button: button.and_then(|button| match button {
                MouseButton::Left => Some(SurfaceMouseButton::Left),
                MouseButton::Middle => Some(SurfaceMouseButton::Middle),
                MouseButton::Right => Some(SurfaceMouseButton::Right),
                MouseButton::Navigate(_) => None,
            }),
            modifiers: modifiers_state(modifiers),
            click_count,
        }
    }

    fn apply_mouse_outcome(&mut self, outcome: MouseOutcome, cx: &mut Context<Self>) {
        match outcome {
            MouseOutcome::Ignored => {}
            MouseOutcome::OpenUrl(url) => cx.open_url(&url),
            MouseOutcome::SelectionChanged | MouseOutcome::HoverChanged => cx.notify(),

            MouseOutcome::FrozenSelectionStarted => {
                self.invalidate(cx);

                cx.notify();
            }

            MouseOutcome::EngineHandled => self.invalidate(cx),

            MouseOutcome::Scrolled(outcome) => {
                self.apply_scroll_outcome(outcome, cx);
            }
        }
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus, cx);
        self.cell_metrics(window, cx);

        let input = self.mouse_input(
            event.position,
            Some(event.button),
            event.modifiers,
            event.click_count,
        );

        let outcome = self.model.mouse_down(input);

        self.apply_mouse_outcome(outcome, cx);
    }

    fn on_mouse_up(&mut self, event: &MouseUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.cell_metrics(window, cx);

        let input = self.mouse_input(event.position, Some(event.button), event.modifiers, 1);
        let release = self.model.mouse_up(input);

        if release.scrollbar_released {
            self.mark_scrollbar_activity(cx);
        }

        self.apply_mouse_outcome(release.outcome, cx);
    }

    pub(super) fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cell_metrics(window, cx);

        let input = self.mouse_input(event.position, event.pressed_button, event.modifiers, 1);
        let outcome = self.model.mouse_move(input);

        self.apply_mouse_outcome(outcome, cx);
    }

    fn on_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cell = self.cell_metrics(window, cx);

        let delta = match event.delta {
            ScrollDelta::Lines(point) => WheelDelta::Steps(point.y),

            ScrollDelta::Pixels(point) => {
                WheelDelta::Rows(point.y.as_f32() / cell.height_px.max(1.0))
            }
        };

        let outcome = self.model.scroll_wheel(
            self.local_position(event.position),
            delta,
            modifiers_state(event.modifiers),
        );

        if outcome.hover_changed {
            cx.notify();
        }

        if outcome.handled {
            self.mark_scrollbar_activity(cx);
            self.invalidate(cx);
        }
    }

    pub(super) fn mark_scrollbar_activity(&mut self, cx: &mut Context<Self>) {
        let generation = self.model.scrollbar.mark_activity();

        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(SCROLLBAR_AUTO_HIDE_DELAY)
                .await;

            let _ = this.update(cx, |this, cx| {
                if this.model.scrollbar.should_fade(generation) {
                    cx.notify();
                }
            });
        })
        .detach();

        cx.notify();
    }

    pub(super) fn apply_scroll_outcome(
        &mut self,
        outcome: ScrollOutcome,
        cx: &mut Context<Self>,
    ) -> bool {
        match outcome {
            ScrollOutcome::Ignored => return false,
            ScrollOutcome::GridRequested => self.invalidate(cx),

            ScrollOutcome::List(op) => {
                self.block_list.apply(op);

                cx.notify();
            }
        }

        self.mark_scrollbar_activity(cx);

        true
    }
}

/// Commit-only IME: composition and candidate placement stay with the OS; the
/// pane receives only the committed string. Inline preedit stays in the IME-owned UI,
/// marked-text methods are inert. `bounds_for_range` reports the terminal cursor
/// cell so the OS positions the candidate window correctly.
impl EntityInputHandler for TerminalPane {
    fn replace_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.model.write_text_input(TextInput::Commit(text)) {
            self.react_to_pty_input(cx);
            self.invalidate(cx);
        }
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let cursor = self.model.frame_cache.current()?.cursor()?;
        let cell = self.model.cell_metrics?;

        // `element_bounds` is the terminal leaf's content rect (padding already
        // excluded), so the cursor cell offsets from its origin directly — plus
        // the inter-block gap offset for the cursor's row.
        let cursor_y = self.model.viewport.cursor_y(cursor.row, cell.height_px);

        Some(Bounds::new(
            point(
                element_bounds.left() + px(cursor.col as f32 * cell.width_px),
                element_bounds.top() + px(cursor_y),
            ),
            size(px(cell.width_px), px(cell.height_px)),
        ))
    }

    // No editable document and no preedit: text and marked-text methods are inert.
    fn text_for_range(
        &mut self,
        _range: Range<usize>,
        _adjusted: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        // GPUI's Windows IME path queries bounds only after obtaining a
        // selection; an empty virtual caret keeps commit-only input eligible.
        Some(UTF16Selection {
            range: 0..0,
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        None
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {}

    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        _new_text: &str,
        _new_selected: Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
    }

    fn character_index_for_point(
        &mut self,
        _point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
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

        self.wake.mark_delivered(self.identity.id);

        // Host events are drained by the shell pump (observer), and the surface
        // is resized from the leaf's actual bounds in paint — neither happens
        // here, so background tabs and chrome offsets are handled correctly.
        let cell = self.cell_metrics(window, cx);

        let frame = self.model.begin_frame();
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
            .bg(rgb(self.model.theme.background.into())
                .opacity(cx.global::<TerminalSettings>().background_opacity))
            // The shell frames each pane as a 1px-bordered rounded card; the
            // fill is rounded to the card's inner radius so its corners don't
            // paint square over the frame. The cell padding below keeps glyphs
            // clear of the rounded corners.
            .rounded(cx.global::<TerminalSettings>().corner_radius - px(1.))
            .text_color(rgb(self.model.theme.foreground.into()))
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
                if !hovered && this.model.pointer_left() {
                    cx.notify();
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
                TerminalView::new(frame, cell, self.focus.clone(), cx.entity()).into_any_element()
            })
            .children(scrollbar)
            // Ctrl-hover link underline. Rects are content-origin-relative;
            // absolute children position from the padding box, so shift by
            // the content padding.
            .when_some(self.model.hovered_link(), |this, link| {
                this.cursor_pointer()
                    .children(link.rects.iter().map(|rect| {
                        div()
                            .absolute()
                            .left(px(rect.origin.x + metrics::PADDING_PX))
                            .top(px(rect.origin.y + metrics::PADDING_PX))
                            .w(px(rect.width))
                            .h(px(rect.height))
                            .bg(rgb(self.model.theme.foreground.into()))
                    }))
            })
    }
}
