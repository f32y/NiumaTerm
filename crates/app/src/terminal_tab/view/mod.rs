mod key;
mod list_state;

#[cfg(test)]
mod tests;

use std::io;
use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures::StreamExt;
use gpui::prelude::*;
use gpui::{
    AnyElement, App, AppContext, Bounds, ClickEvent, Context, Entity, EntityInputHandler,
    EventEmitter, ExternalPaths, FocusHandle, Focusable, IntoElement, KeyDownEvent, KeyUpEvent,
    Keystroke, Modifiers, ModifiersChangedEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Point, Role, ScrollDelta, ScrollWheelEvent, SharedString, Size,
    UTF16Selection, Window, actions, div, list, point, px, rgb, size,
};
use nmt_agent::AgentRoute;
use nmt_config::local_state::TabState;
use nmt_config::{CursorShape, active_colors};
use nmt_remote::NetworkPty;
use nmt_remote::connection::{RemoteHost, Status};
use nmt_remote::local_view::LocalView;
use nmt_remote_core::rpc::EndReason;
use nmt_terminal::clipboard::{Clipboard, ClipboardType};
use nmt_terminal::event::MsgSender;
use nmt_terminal::input::{KeyPhase, WheelDelta};
use nmt_terminal::session::interaction::{CopyCompletion, PendingCopy};
use nmt_terminal::session::{HostEvent, SessionChange, SurfaceMouseButton, TerminalSessionConfig};
use rust_i18n::t;
use tracing::warn;

use crate::copy_toast::show_text_copied;
use crate::find::OpenFind;
use crate::remote_control::{CloseTab, ControlSheet, HostControl};
use crate::terminal_tab::block_list::live::LiveItemState;
use crate::terminal_tab::frame::TerminalFrame;
use crate::terminal_tab::frame_source::TerminalFrameSource;
use crate::terminal_tab::metrics::CellMetrics;
use crate::terminal_tab::pane_model::list_mirror::ListPosition;
use crate::terminal_tab::pane_model::viewport::LocalPoint;
use crate::terminal_tab::pane_model::{
    ClipboardAccess, KeyOutcome, MouseInput, MouseOutcome, PaneController, ScrollOutcome, TextInput,
};
use crate::terminal_tab::scrollbar::geometry::SCROLLBAR_AUTO_HIDE_DELAY;
use crate::terminal_tab::scrollbar::scrollbar_element;
use crate::terminal_tab::search::{self, SearchState};
use crate::terminal_tab::settings::{TerminalSettings, duration_labels};
use crate::terminal_tab::terminal_view::{BlockListItem, BlockListView, TerminalView};
use crate::terminal_tab::view::key::{is_window_system_key, modifiers_state, terminal_key};
use crate::terminal_tab::view::list_state::{BlockListState, block_list_alignment};
use crate::terminal_tab::{metrics, wake};

actions!(
    terminal,
    [
        /// Send Tab to the PTY (shell completion).
        SendTab,
        /// Send Shift-Tab to the PTY (shell backward completion).
        SendShiftTab,
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

    /// The PTY runs on another computer. Such a tab is restored by
    /// reattaching to its session, not by relaunching a shell here.
    remote: Option<RemoteTab>,

    /// The session id of a terminal a paired device started on this
    /// computer, when the PTY is a view of it. Paired devices already see
    /// that terminal, so the pane is not offered to them a second time.
    remote_created: Option<String>,
}

/// A terminal session on another computer, shown in this pane.
pub struct RemoteTab {
    pub host: Arc<RemoteHost>,
    pub session: String,

    /// Closing the tab ends the session: this client started it, and nothing
    /// else would show it afterwards. A host tab, or another client's
    /// session, only loses this view.
    pub ends_with_tab: bool,
}

/// A host tab offered to paired devices. Remote views may resize the shared
/// PTY; the tab takes the size back when its user types, and reports its
/// own resizes so remote views can do the same.
pub struct HostShare {
    pub claimed_remotely: Arc<AtomicBool>,
    pub on_size: Box<dyn Fn(u16, u16)>,
}

pub struct TerminalPane {
    pub(super) search: SearchState,
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
    host_share: Option<HostShare>,

    /// Who controls this host tab from another computer, and taking it
    /// back. While anyone does, a sheet covers the pane and refuses input;
    /// the output underneath keeps drawing so the host can watch.
    host_control: Option<HostControl>,

    close_tab: Option<CloseTab>,

    /// Holds the keyboard while a control sheet covers the pane.
    sheet_focus: FocusHandle,

    /// Whether the last frame showed a control sheet. Input is refused
    /// while it did: the session belongs to the other side.
    sheet_shown: bool,
}

pub struct AgentInterrupted;

struct DesktopClipboard;

impl ClipboardAccess for DesktopClipboard {
    fn read(&mut self) -> Option<String> {
        let text = Clipboard::default().get(ClipboardType::Clipboard);

        (!text.is_empty()).then_some(text)
    }

    fn write(&mut self, kind: ClipboardType, text: String) -> bool {
        Clipboard::default().set(kind, text)
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

        // ConPTY dimensions must fit its signed coordinates. Missing or invalid
        // saved grids use the initial default until layout supplies a size.
        let (cols, rows) = launch
            .restorable
            .grid_size
            .filter(|&(cols, rows)| {
                (1..=i16::MAX as u16).contains(&cols) && (1..=i16::MAX as u16).contains(&rows)
            })
            .unwrap_or((metrics::COLS, metrics::ROWS));

        let source = TerminalFrameSource::for_gpui(
            wake.clone(),
            surface_id,
            TerminalSessionConfig {
                cols,
                rows,
                ..launch.config
            },
            active_colors(),
        )?;

        let identity = PaneIdentity {
            id: surface_id,
            profile_name: launch.profile_name,
            restorable: launch.restorable,
            agent_route: launch.agent_route,
            remote: None,
            remote_created: None,
        };

        Ok(cx.new(|cx| Self::from_source(cx, identity, wake, wake_rx, source)))
    }

    /// A pane showing a terminal that runs on another computer. Layout
    /// resizes the host PTY to the pane.
    pub fn spawn_remote(
        cx: &mut impl AppContext,
        surface_id: u64,
        pty: NetworkPty,
        ends_with_tab: bool,
        agent_route: AgentRoute,
        cursor_shape: CursorShape,
    ) -> Result<Entity<Self>, String> {
        let (wake, wake_rx) = wake::wake_channel();
        let host = Arc::clone(pty.host());
        let session = pty.session().to_owned();
        let title = host.name();

        let mut status = host.status();
        let mut ended = host.ended_changes();

        let source = TerminalFrameSource::remote(
            wake.clone(),
            surface_id,
            pty,
            (metrics::COLS, metrics::ROWS),
            cursor_shape,
            active_colors(),
        )?;

        let identity = PaneIdentity {
            id: surface_id,
            profile_name: title,
            restorable: TabState::default(),
            agent_route,
            remote: Some(RemoteTab {
                host,
                session,
                ends_with_tab,
            }),
            remote_created: None,
        };

        Ok(cx.new(|cx| {
            // The connection banner follows the link, whose changes come
            // from the network runtime, not from a frame.
            cx.spawn(async move |this, cx| {
                while status.changed().await.is_ok() {
                    if this.update(cx, |_, cx| cx.notify()).is_err() {
                        break;
                    }
                }
            })
            .detach();

            // The host ending or taking back this view changes what covers
            // the pane.
            cx.spawn(async move |this, cx| {
                while ended.changed().await.is_ok() {
                    if this.update(cx, |_, cx| cx.notify()).is_err() {
                        break;
                    }
                }
            })
            .detach();

            Self::from_source(cx, identity, wake, wake_rx, source)
        }))
    }

    /// A pane showing a terminal a paired device started on this computer.
    /// Closing it leaves the terminal running for that device.
    pub fn spawn_local_view(
        cx: &mut impl AppContext,
        surface_id: u64,
        pty: LocalView,
        title: String,
        agent_route: AgentRoute,
        cursor_shape: CursorShape,
    ) -> Result<Entity<Self>, String> {
        let (wake, wake_rx) = wake::wake_channel();
        let session = pty.session().to_owned();

        let source = TerminalFrameSource::remote(
            wake.clone(),
            surface_id,
            pty,
            (metrics::COLS, metrics::ROWS),
            cursor_shape,
            active_colors(),
        )?;

        let identity = PaneIdentity {
            id: surface_id,
            profile_name: title,
            restorable: TabState::default(),
            agent_route,
            remote: None,
            remote_created: Some(session),
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
        cx.observe_global::<TerminalSettings>(Self::on_terminal_settings_changed)
            .detach();

        cx.spawn(async move |this, cx| {
            while let Some(wake) = wake_rx.next().await {
                if this.update(cx, |this, cx| this.on_wake(wake, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();

        let settings = cx.global::<TerminalSettings>();
        let fixed_bottom_requested = settings.fixed_bottom();

        Self {
            focus: cx.focus_handle(),
            search: SearchState::default(),
            identity,
            model: PaneController::new(
                surface,
                settings.clone(),
                (&active_colors()).into(),
                duration_labels(),
                Box::new(DesktopClipboard),
            ),
            content_bounds: None,
            wake,
            image_releases_attached: false,
            block_list: BlockListState::new(block_list_alignment(fixed_bottom_requested)),
            host_share: None,
            host_control: None,
            close_tab: None,
            sheet_focus: cx.focus_handle(),
            sheet_shown: false,
        }
    }

    fn on_terminal_settings_changed(&mut self, cx: &mut Context<Self>) {
        let settings = cx.global::<TerminalSettings>().clone();
        let colors = active_colors();

        self.block_list
            .list
            .set_alignment(block_list_alignment(settings.fixed_bottom()));

        if let Some(update) = self
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
    }

    fn on_wake(&mut self, change: SessionChange, cx: &mut Context<Self>) {
        match change {
            SessionChange::Content => self.invalidate(cx),
            SessionChange::HostEvents => {
                self.model.invalidate();

                // Background panes cannot clear their dirty bit by rendering, but the
                // shell observer still needs every chrome wake to refresh tab state.
                cx.notify();
            }
        }
    }

    pub fn agent_route(&self) -> &AgentRoute {
        &self.identity.agent_route
    }

    pub fn profile_name(&self) -> &str {
        &self.identity.profile_name
    }

    pub fn is_remote(&self) -> bool {
        self.identity.remote.is_some()
    }

    /// The remote-created terminal this pane views, if any.
    pub fn remote_created_session(&self) -> Option<&str> {
        self.identity.remote_created.as_deref()
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
            if let Some(share) = &self.host_share {
                let (cols, rows) = self.model.source.grid_size();

                (share.on_size)(cols, rows);
            }

            cx.notify();
        }
    }

    /// Offer this tab's terminal to paired devices.
    pub fn share_with_host(&mut self, share: HostShare) {
        self.host_share = Some(share);
    }

    /// The handle that drives this pane's PTY loop from other threads.
    pub fn session_messenger(&self) -> MsgSender {
        self.model.source.session.messenger()
    }

    pub fn grid_size(&self) -> (u16, u16) {
        self.model.source.grid_size()
    }

    pub fn remote_tab(&self) -> Option<&RemoteTab> {
        self.identity.remote.as_ref()
    }

    /// The tab is closing: end its remote session if this client owns it.
    /// A session the host ended or took back is no longer this client's.
    /// Leave the session this pane follows running on its host when the
    /// pane closes, as when its user disconnects from it instead of ending it.
    pub fn keep_remote_session(&mut self) {
        if let Some(remote) = &mut self.identity.remote {
            remote.ends_with_tab = false;
        }
    }

    pub fn end_remote_session(&self) {
        if let Some(remote) = &self.identity.remote
            && remote.ends_with_tab
            && remote.host.ended(&remote.session).is_none()
        {
            remote.host.terminate(&remote.session);
        }
    }

    /// Follow who controls this shared host tab from another computer.
    pub fn control_from_host(&mut self, control: HostControl, cx: &mut Context<Self>) {
        let mut changes = control.changes.clone();

        cx.spawn(async move |this, cx| {
            while changes.changed().await.is_ok() {
                if this.update(cx, |this, cx| this.invalidate(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();

        self.host_control = Some(control);
    }

    /// How the sheets' close buttons close this pane's tab.
    pub fn close_tab_with(&mut self, close: CloseTab) {
        self.close_tab = Some(close);
    }

    /// Take the session back from the devices controlling it, and the PTY
    /// size with it.
    fn take_back(&mut self, cx: &mut Context<Self>) {
        if let Some(control) = &self.host_control {
            (control.take_back)();
        }

        if let Some(share) = &self.host_share {
            share.claimed_remotely.store(true, Ordering::Relaxed);
        }

        self.reclaim_shared_size();

        self.invalidate(cx);
    }

    /// The sheet over the pane, if one belongs there: another computer
    /// controls this host tab, or the host ended this view of its session.
    fn control_sheet(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let close = self.close_tab.clone();

        let close_tab = move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
            if let Some(close) = &close {
                close(window, cx);
            }
        };

        let controllers = self
            .host_control
            .as_ref()
            .map(|control| (control.controllers)())
            .unwrap_or_default();

        let sheet = if !controllers.is_empty() {
            Some(
                ControlSheet::new(
                    t!("remote-controlled-by", devices = controllers.join(", ")).into_owned(),
                    self.sheet_focus.clone(),
                )
                .button(
                    t!("remote-take-back"),
                    true,
                    cx.listener(|this, _, _, cx| this.take_back(cx)),
                )
                .button(t!("remote-end-session"), false, close_tab),
            )
        } else if let Some(remote) = &self.identity.remote
            && let Some(reason) = remote.host.ended(&remote.session)
        {
            let name = remote.host.name();

            match reason {
                EndReason::Closed => Some(
                    ControlSheet::new(
                        t!("remote-session-closed", name = name).into_owned(),
                        self.sheet_focus.clone(),
                    )
                    .button(t!("remote-close-tab"), true, close_tab),
                ),
                EndReason::TakenBack | EndReason::Unknown => {
                    let host = Arc::clone(&remote.host);
                    let session = remote.session.clone();

                    Some(
                        ControlSheet::new(
                            t!("remote-taken-back", name = name).into_owned(),
                            self.sheet_focus.clone(),
                        )
                        .button(t!("remote-reconnect"), true, move |_, _, _| {
                            host.reconnect(&session)
                        })
                        .button(t!("remote-end-session"), false, close_tab),
                    )
                }
            }
        } else {
            None
        };

        // The sheet takes the keyboard from the terminal while it is up and
        // gives it back when it goes, if the terminal had it.
        match (sheet.is_some(), self.sheet_shown) {
            (true, false) if self.focus.contains_focused(window, cx) => {
                window.focus(&self.sheet_focus, cx)
            }
            (false, true) if self.sheet_focus.is_focused(window) => window.focus(&self.focus, cx),
            _ => {}
        }

        self.sheet_shown = sheet.is_some();

        sheet.map(|sheet| sheet.render(cx))
    }

    /// A remote view resized this shared terminal; typing here takes the
    /// size back so the person at the host sees the terminal fit.
    fn reclaim_shared_size(&mut self) {
        let Some(share) = &self.host_share else {
            return;
        };

        if share.claimed_remotely.swap(false, Ordering::Relaxed)
            && self.model.source.reassert_size()
        {
            let (cols, rows) = self.model.source.grid_size();

            (share.on_size)(cols, rows);
        }
    }

    /// What the remote tab's banner says, or `None` while connected.
    fn remote_banner(&self) -> Option<SharedString> {
        let remote = self.identity.remote.as_ref()?;
        let name = remote.host.name();

        let text = match *remote.host.status().borrow() {
            Status::Connected => return None,
            Status::Idle | Status::Connecting => t!("remote-banner-connecting", name = name),
            Status::Reconnecting => t!("remote-banner-reconnecting", name = name),
            Status::Refused => t!("remote-banner-refused", name = name),
            Status::Unreachable => t!("remote-banner-unreachable", name = name),
        };

        Some(text.into_owned().into())
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
    pub fn child_process_count(&self) -> io::Result<usize> {
        self.model.source.session.child_process_count()
    }

    /// Whether a command is currently executing in this pane. Mirrors the
    /// session's in-flight block, so it only reports for shells whose OSC 133
    /// marks are trusted; an unintegrated shell always reads as idle.
    pub fn command_running(&self) -> bool {
        self.model.in_flight.is_some()
    }

    pub fn tab_state(&self) -> TabState {
        if let Some(remote) = &self.identity.remote {
            return TabState {
                remote_host: Some(remote.host.id().as_str().to_owned()),
                remote_session: Some(remote.session.clone()),
                grid_size: Some(self.model.source.grid_size()),
                ..TabState::default()
            };
        }

        let mut state = self.identity.restorable.clone();

        state.grid_size = Some(self.model.source.grid_size());

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
            // The list state lives in this pane, so a strong handle here would
            // keep the pane, and with it the session and its shell, alive after
            // its tab closes.
            let pane = cx.entity().downgrade();

            self.block_list.list.set_scroll_handler(move |_, _, cx| {
                let _ = pane.update(cx, |pane, cx| pane.mark_scrollbar_activity(cx));
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
                    pane: pane_for_items.clone(),
                }
                .into_any_element()
            } else {
                BlockListItem::Live {
                    frame: frame_for_items.clone(),
                    history_rows,
                    state: LiveItemState {
                        in_flight: in_flight_for_items.clone(),
                        has_open_prompt: has_open_prompt_for_items,
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
                    this.on_copy_finished(text, copy.completion, window, cx)
                });
            }
            result => warn!("terminal copy did not complete: {result:?}"),
        })
        .detach();
    }

    fn on_copy_finished(
        &mut self,
        text: String,
        completion: CopyCompletion,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.model.finish_copy(text, completion) {
            show_text_copied(window, cx);

            self.invalidate(cx);

            cx.notify();
        }
    }

    fn attach_image_releases(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    /// UI reaction to input reaching the PTY: optionally snap the view back
    /// to the latest output.
    fn react_to_pty_input(&mut self, cx: &mut Context<Self>) {
        if self.model.settings.scroll_to_bottom_when_typing {
            let outcome = self.model.scroll_to_latest();

            self.apply_scroll_outcome(outcome, cx);
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.prefer_character_input
            || !self.focus.is_focused(window)
            || self.sheet_shown
            || is_window_system_key(&event.keystroke)
        {
            return;
        }

        self.reclaim_shared_size();

        let interrupts_agent = matches!(event.keystroke.key.as_str(), "escape" | "esc")
            && !event.keystroke.modifiers.modified();

        let mut key = terminal_key(&event.keystroke);

        if event.is_held {
            key.phase = KeyPhase::Repeat;
        }

        let outcome = self.model.key_down(&key);

        if !matches!(outcome, KeyOutcome::Ignored) {
            // Stop native character delivery after the key path has accepted
            // input, otherwise printable keys are sent again by WM_CHAR.
            cx.stop_propagation();
        }

        if self.apply_key_outcome(outcome, window, cx) && interrupts_agent {
            cx.emit(AgentInterrupted);
        }
    }

    fn on_key_up(&mut self, event: &KeyUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus.is_focused(window)
            || self.sheet_shown
            || is_window_system_key(&event.keystroke)
        {
            return;
        }

        let mut key = terminal_key(&event.keystroke);

        key.phase = KeyPhase::Release;

        if matches!(self.model.send_key(&key), KeyOutcome::Written) {
            cx.stop_propagation();
        }
    }

    fn apply_key_outcome(
        &mut self,
        outcome: KeyOutcome,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        match outcome {
            KeyOutcome::Ignored => false,
            KeyOutcome::Scrolled(outcome) => {
                self.apply_scroll_outcome(outcome, cx);

                false
            }
            KeyOutcome::Written => {
                self.react_to_pty_input(cx);

                self.invalidate(cx);

                true
            }
            KeyOutcome::CopyPending(copy) => {
                self.begin_copy(copy, window, cx);

                false
            }
        }
    }

    /// Route a keystroke straight to the terminal PTY.
    fn feed_terminal_key(
        &mut self,
        keystroke: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let outcome = self.model.send_key(&terminal_key(keystroke));

        self.apply_key_outcome(outcome, window, cx);
    }

    /// Tab/Shift-Tab belong to the shell (completion) while the terminal is
    /// focused, but `Root` binds them to focus traversal and key bindings
    /// dispatch before the pane's `on_key_down` listener. These actions are
    /// bound in the deeper `Terminal` context, which wins over `Root`.
    fn on_send_tab(&mut self, _: &SendTab, window: &mut Window, cx: &mut Context<Self>) {
        if self.sheet_shown || !self.focus.is_focused(window) {
            cx.propagate();

            return;
        }

        self.feed_terminal_key(
            &Keystroke {
                modifiers: Modifiers::none(),
                key: "tab".into(),
                key_char: None,
            },
            window,
            cx,
        );
    }

    fn on_send_shift_tab(&mut self, _: &SendShiftTab, window: &mut Window, cx: &mut Context<Self>) {
        if self.sheet_shown || !self.focus.is_focused(window) {
            cx.propagate();

            return;
        }

        self.feed_terminal_key(
            &Keystroke {
                modifiers: Modifiers::shift(),
                key: "tab".into(),
                key_char: None,
            },
            window,
            cx,
        );
    }

    fn on_file_drop(&mut self, paths: &ExternalPaths, window: &mut Window, cx: &mut Context<Self>) {
        if self.sheet_shown {
            return;
        }

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
        if self.sheet_shown {
            return;
        }

        self.reclaim_shared_size();

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
        // excluded), so the cursor cell offsets from its origin directly, plus
        // the inter-block gap offset for the cursor's row.
        let cursor_y = self.model.viewport().cursor_y(cursor.row, cell.height_px);

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
        if !self.focus.is_focused(window) {
            self.model.interaction.clear_pressed_keys();
        }

        if !self.image_releases_attached {
            self.attach_image_releases(window, cx);
        }

        self.wake.mark_delivered();

        // Host events are drained by the shell pump (observer), and the surface
        // is resized from the leaf's painted bounds in paint; neither happens
        // here, so background tabs and chrome offsets are handled correctly.
        let cell = self.cell_metrics(window, cx);

        let frame = self.model.begin_frame();

        search::refresh(self, cx);

        let show_block_chrome = self.model.settings.command_blocks;

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

        let scrollbar_info = self.model.viewport().scrollbar_info();

        // Keep the transparent track hit-testable so hovering the scrollbar
        // region can reveal it after the activity fade has completed.
        let scrollbar = scrollbar_element(scrollbar_info, scrollbar_opacity.unwrap_or(0.0), cx);

        let sheet = self.control_sheet(window, cx);

        div()
            // Stateful id: hover-end tracking (the link-underline clear
            // below) needs element state.
            .id(("terminal-pane", self.identity.id as usize))
            // The pane holds keyboard focus, so it needs its own node for
            // screen readers to announce a terminal instead of the window.
            .role(Role::Terminal)
            .size_full()
            .relative()
            // This is the terminal region's single full-bleed background;
            // cells with explicit background colors stay opaque on top.
            .bg(rgb(self.model.theme.background.into())
                .opacity(cx.global::<TerminalSettings>().background_opacity))
            .text_color(rgb(self.model.theme.foreground.into()))
            .font(cx.global::<TerminalSettings>().font())
            .text_size(px(metrics::font_size_px(cx)))
            .line_height(px(cell.height_px))
            .p(px(metrics::PADDING_PX))
            .overflow_hidden()
            .track_focus(&self.focus)
            .key_context("Terminal")
            .on_action(cx.listener(|pane, _: &OpenFind, window, cx| search::open(pane, window, cx)))
            .on_action(cx.listener(Self::on_send_tab))
            .on_action(cx.listener(Self::on_send_shift_tab))
            .on_action(cx.listener(Self::on_previous_block))
            .on_action(cx.listener(Self::on_next_block))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_key_up(cx.listener(Self::on_key_up))
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
            .children(self.search.bar.clone().filter(|bar| bar.read(cx).visible))
            .children(sheet)
            // Ctrl-hover link underline. Rects are content-origin-relative;
            // absolute children position from the padding box, so shift by
            // the content padding.
            .children(self.remote_banner().map(|text| {
                div()
                    .absolute()
                    .top(px(metrics::PADDING_PX))
                    .right(px(metrics::PADDING_PX))
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(self.model.theme.foreground.into()).opacity(0.85))
                    .text_color(rgb(self.model.theme.background.into()))
                    .child(text)
            }))
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
