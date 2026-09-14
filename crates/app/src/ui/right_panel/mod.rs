//! Background tasks and workflows share one auxiliary column. Git review
//! belongs to the workspace tab strip and uses the central content area.

#[cfg(test)]
mod tests;

use app::design::{
    AUXILIARY_MAX_WIDTH, AUXILIARY_MIN_WIDTH, AUXILIARY_WIDTH_SHARE, PRIMARY_CONTENT_MIN_WIDTH,
    SPACE_2,
};
use gpui::prelude::*;
use gpui::{AnyElement, Context, DragMoveEvent, Entity, Pixels, Window, div};
use gpui_component::{StyledExt as _, v_flex};

use crate::ui::background_tasks::BackgroundTasksView;
use crate::ui::composition::sidebar_surface;
use crate::ui::sidebar_resize::{self, ResizeDrag};
use crate::ui::workflows::WorkflowsView;

/// Handle id for this column's resize grip. Every resizable column receives
/// every other column's drag-move events, so this is what distinguishes them.
pub(super) const RESIZE_HANDLE: &str = "right-panel-resize";

#[derive(Default)]
struct PanelSizing {
    available: Pixels,
    preferred: Option<Pixels>,
}

impl PanelSizing {
    fn width(&self) -> Pixels {
        let maximum = (self.available - PRIMARY_CONTENT_MIN_WIDTH)
            .max(Pixels::ZERO)
            .min(AUXILIARY_MAX_WIDTH);

        let minimum = AUXILIARY_MIN_WIDTH.min(maximum);

        self.preferred
            .unwrap_or(self.available * AUXILIARY_WIDTH_SHARE)
            .clamp(minimum, maximum)
    }

    fn resize(&mut self, requested: Pixels) {
        self.preferred = Some(requested);
        self.preferred = Some(self.width());
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RightPanelKind {
    BackgroundTasks,
    Workflows,
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
            kind: RightPanelKind::BackgroundTasks,
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
}

pub(crate) struct RightPanel {
    selection: RightPanelSelection,
    sizing: PanelSizing,

    /// False on startup and during a live drag so only explicit toggles slide.
    animated: bool,

    tasks: Entity<BackgroundTasksView>,
    workflows: Entity<WorkflowsView>,
}

impl RightPanel {
    pub(crate) fn new(
        tasks: Entity<BackgroundTasksView>,
        workflows: Entity<WorkflowsView>,
    ) -> Self {
        Self {
            selection: RightPanelSelection::new(),
            sizing: PanelSizing::default(),
            animated: false,
            tasks,
            workflows,
        }
    }

    pub(crate) fn set_available_width(&mut self, available: Pixels, cx: &mut Context<Self>) {
        let available = available.max(Pixels::ZERO);

        if self.sizing.available != available {
            self.sizing.available = available;
            self.animated = false;

            cx.notify();
        }
    }

    pub(crate) fn shows(&self, kind: RightPanelKind) -> bool {
        self.selection.shows(kind)
    }

    pub(crate) fn tasks(&self) -> &Entity<BackgroundTasksView> {
        &self.tasks
    }

    pub(crate) fn workflows(&self) -> &Entity<WorkflowsView> {
        &self.workflows
    }

    /// Choose what the right-side area shows. Selecting the visible content
    /// closes the area; selecting the other replaces it at the current width.
    /// Returns the open state so callers can mark newly visible activity as seen.
    pub(crate) fn select(&mut self, kind: RightPanelKind, cx: &mut Context<Self>) -> bool {
        let open = self.selection.select(kind);

        self.animated = true;

        self.sync_task_visibility(cx);

        cx.notify();

        open
    }

    /// Each content polls or repaints only while it is the visible one, so
    /// both are told on every selection change.
    fn sync_task_visibility(&self, cx: &mut Context<Self>) {
        let tasks_visible = self.shows(RightPanelKind::BackgroundTasks);

        self.tasks
            .update(cx, |view, cx| view.set_visible(tasks_visible, cx));

        let workflows_visible = self.shows(RightPanelKind::Workflows);

        self.workflows
            .update(cx, |view, cx| view.set_visible(workflows_visible, cx));
    }
}

impl Render for RightPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let width = self.sizing.width();

        let open = self.selection.shows(RightPanelKind::BackgroundTasks)
            || self.selection.shows(RightPanelKind::Workflows);

        // One content is mounted at a time; two right-side columns are not
        // representable by this layout.
        let body: AnyElement = match self.selection.kind {
            RightPanelKind::BackgroundTasks => self.tasks.clone().into_any_element(),
            RightPanelKind::Workflows => self.workflows.clone().into_any_element(),
        };

        // The auxiliary card shares the content baseline and uses the same
        // spacing scale as other surfaces while leaving a readable gutter.
        let card = v_flex()
            .refine_style(&sidebar_surface(cx))
            .size_full()
            .child(body);

        let content = div()
            .w(width)
            .h_full()
            .flex_none()
            .relative()
            .px(SPACE_2)
            .pb(SPACE_2)
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
                let previous = this.sizing.width();

                this.sizing.resize(e.bounds.right() - e.event.position.x);

                if this.sizing.width() != previous {
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
