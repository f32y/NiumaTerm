//! The single right-side area. Git and `Background Tasks` are two contents of
//! one host rather than two sidebars, so choosing one replaces the other at the
//! current width and the main pane can never be narrowed by two columns.

use gpui::prelude::*;
use gpui::{AnyElement, Context, DragMoveEvent, Entity, Pixels, Window, div, px};
use gpui_component::{ActiveTheme as _, v_flex};

use crate::ui::UI_RADIUS;
use crate::ui::background_tasks::BackgroundTasksView;
use crate::ui::git_sidebar::GitSidebar;
use crate::ui::sidebar_resize::{self, ResizeDrag};

/// Handle id for this column's resize grip. Every resizable column receives
/// every other column's drag-move events, so this is what distinguishes them.
pub(super) const RESIZE_HANDLE: &str = "right-panel-resize";

const PANEL_WIDTH: f32 = 360.0;
/// Drag limits: keep the panel usable and leave room for the terminal.
const MIN_WIDTH: f32 = 240.0;
const MAX_WIDTH: f32 = 900.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RightPanelKind {
    Git,
    BackgroundTasks,
}

/// Which content the right-side area shows, and whether it is open at all.
/// One selection means the two views can never render as two columns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RightPanelSelection {
    kind: RightPanelKind,
    open: bool,
}

impl RightPanelSelection {
    pub(crate) fn new() -> Self {
        Self {
            kind: RightPanelKind::Git,
            open: false,
        }
    }

    pub(crate) fn shows(self, kind: RightPanelKind) -> bool {
        self.open && self.kind == kind
    }

    /// Selecting the visible content closes the area; selecting the other
    /// replaces it. Returns the resulting open state.
    pub(crate) fn select(&mut self, kind: RightPanelKind) -> bool {
        if self.shows(kind) {
            self.open = false;
        } else {
            self.kind = kind;
            self.open = true;
        }
        self.open
    }

    /// Close the area when it currently shows `kind`. Returns whether the
    /// selection changed.
    pub(crate) fn close_if_showing(&mut self, kind: RightPanelKind) -> bool {
        if !self.shows(kind) {
            return false;
        }
        self.open = false;
        true
    }
}

pub(crate) struct RightPanel {
    selection: RightPanelSelection,
    width: Pixels,
    /// False on startup and during a live drag so only explicit toggles slide.
    animated: bool,
    git: Entity<GitSidebar>,
    tasks: Entity<BackgroundTasksView>,
}

impl RightPanel {
    pub(crate) fn new(git: Entity<GitSidebar>, tasks: Entity<BackgroundTasksView>) -> Self {
        Self {
            selection: RightPanelSelection::new(),
            width: px(PANEL_WIDTH),
            animated: false,
            git,
            tasks,
        }
    }

    pub(crate) fn shows(&self, kind: RightPanelKind) -> bool {
        self.selection.shows(kind)
    }

    pub(crate) fn tasks(&self) -> &Entity<BackgroundTasksView> {
        &self.tasks
    }

    /// Choose what the right-side area shows. Selecting the visible content
    /// closes the area; selecting the other replaces it at the current width.
    /// Returns the open state so the caller can react (Git refreshes on open,
    /// `Background Tasks` records its activity as seen).
    pub(crate) fn select(&mut self, kind: RightPanelKind, cx: &mut Context<Self>) -> bool {
        let open = self.selection.select(kind);
        self.animated = true;
        self.sync_task_visibility(cx);
        cx.notify();
        open
    }

    /// Close the area when its current content has nothing to show, for
    /// example after the active pane stops being a supported Agent tab.
    pub(crate) fn close_if_showing(&mut self, kind: RightPanelKind, cx: &mut Context<Self>) {
        if !self.selection.close_if_showing(kind) {
            return;
        }
        self.animated = true;
        self.sync_task_visibility(cx);
        cx.notify();
    }

    fn sync_task_visibility(&self, cx: &mut Context<Self>) {
        let visible = self.shows(RightPanelKind::BackgroundTasks);
        self.tasks
            .update(cx, |view, cx| view.set_visible(visible, cx));
    }
}

impl Render for RightPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let width = self.width;
        let open = self.selection.shows(RightPanelKind::Git)
            || self.selection.shows(RightPanelKind::BackgroundTasks);

        // One content is mounted at a time; two right-side columns are not
        // representable by this layout.
        let body: AnyElement = match self.selection.kind {
            RightPanelKind::Git => self.git.clone().into_any_element(),
            RightPanelKind::BackgroundTasks => self.tasks.clone().into_any_element(),
        };

        // The panel surface is a floating card (own background, 1px border,
        // large radius) in a gutter cut from the fixed width: right inset
        // clears the window edge, the top inset lines up with the tab pills,
        // and the terminal column's own gutter provides the left gap — so the
        // resize handle keeps riding the card's left edge.
        let card = v_flex()
            .size_full()
            .bg(cx.theme().sidebar)
            .border_1()
            .border_color(cx.theme().sidebar_border)
            .rounded(UI_RADIUS)
            .overflow_hidden()
            .child(body);

        let content = div()
            .w(width)
            .h_full()
            .flex_none()
            .relative()
            .pr(px(6.))
            .pt(px(4.))
            .pb(px(6.))
            .child(card);

        let wrapper = div()
            .h_full()
            .flex_none()
            .relative()
            .overflow_hidden()
            .on_drag_move(cx.listener(|this, e: &DragMoveEvent<ResizeDrag>, _, cx| {
                // The sidebar on the other side drags the same type, and these
                // events carry no bounds test, so a gesture that did not start
                // here would otherwise resize this column too.
                if !e.drag(cx).is_from(RESIZE_HANDLE) {
                    return;
                }
                // The panel's right edge is pinned to the window edge, so
                // the new width is right edge minus pointer x.
                let width = (e.bounds.right() - e.event.position.x)
                    .max(px(MIN_WIDTH))
                    .min(px(MAX_WIDTH));
                if width != this.width {
                    this.width = width;
                    // Render at the live drag width; the next toggle re-arms
                    // the slide animation.
                    this.animated = false;
                    cx.notify();
                }
            }))
            .child(content)
            .children(open.then(|| sidebar_resize::resize_handle(RESIZE_HANDLE, true, cx)));

        // Keep the entity mounted at width zero while closed so it can render
        // the closing frames instead of disappearing in the toggle render.
        sidebar_resize::slide_width(wrapper, "right-panel", open, width, self.animated)
    }
}
