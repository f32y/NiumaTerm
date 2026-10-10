use std::cell::{Cell, RefCell};
use std::rc::Rc;

struct WorkspaceButtonProbe;

impl gpui::Render for WorkspaceButtonProbe {
    fn render(
        &mut self,
        _: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        use gpui::{InteractiveElement as _, ParentElement as _, Styled as _, div, px};

        use crate::ui::workspace_sidebar::list::{WORKSPACE_NAME_TEXT, workspace_row_button};

        div().size_full().child(
            div()
                .w(px(300.))
                .child(
                    workspace_row_button("idle-workspace", cx).child(
                        div()
                            .text_size(px(WORKSPACE_NAME_TEXT))
                            .truncate()
                            .child("mipmap gyjp 工作区")
                            .debug_selector(|| "workspace-name".into()),
                    ),
                )
                .debug_selector(|| "workspace-row".into()),
        )
    }
}

#[gpui::test]
fn idle_workspace_button_renders_and_accepts_hover(cx: &mut gpui::TestAppContext) {
    use gpui::{Modifiers, VisualTestContext, point, px};

    cx.update(gpui_component::init);

    let window = cx.add_window(|_, _| WorkspaceButtonProbe);

    let mut cx = VisualTestContext::from_window(window.into(), cx);

    cx.run_until_parked();
    cx.refresh().unwrap();

    let bounds = cx
        .debug_bounds("workspace-row")
        .expect("workspace row was not painted");

    assert_eq!(bounds.size.width, px(300.));

    let name = cx
        .debug_bounds("workspace-name")
        .expect("workspace name was not painted");

    assert!(
        name.size.height >= px(17.),
        "the clipped text box needs room for descenders: {name:?}"
    );
    assert!(bounds.top() < name.top() && bounds.bottom() > name.bottom());

    cx.simulate_mouse_move(bounds.center(), None, Modifiers::default());
    cx.refresh().unwrap();
    cx.simulate_mouse_move(point(px(350.), px(100.)), None, Modifiers::default());
    cx.refresh().unwrap();

    assert_eq!(cx.debug_bounds("workspace-row"), Some(bounds));
}

struct WorkspaceHoverProbe {
    transitions: Rc<RefCell<Vec<bool>>>,
    clicks: Rc<Cell<usize>>,
    drags: Rc<Cell<usize>>,
}

impl gpui::Render for WorkspaceHoverProbe {
    fn render(
        &mut self,
        _: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        use gpui::{
            AppContext as _, InteractiveElement as _, ParentElement as _,
            StatefulInteractiveElement as _, Styled as _, div, px,
        };
        use gpui_component::modern_menu::ModernMenuExt as _;
        use gpui_component::tooltip::ManagedTooltipExt as _;

        use crate::ui::workspace_sidebar::drag::WorkspaceDrag;
        use crate::ui::workspace_sidebar::list::workspace_row_button;

        let transitions = self.transitions.clone();
        let clicks = self.clicks.clone();
        let drags = self.drags.clone();

        div().size_full().child(
            div()
                .id("workspace-menu")
                .relative()
                .w(px(300.))
                .on_drag(WorkspaceDrag { from: 0 }, move |_, _, _, cx| {
                    drags.set(drags.get() + 1);

                    cx.new(|_| WorkspaceButtonProbe)
                })
                .modern_context_menu(|menu, _, _| menu)
                .managed_tooltip_right("Primary directory: /Users/test/work")
                .child(
                    workspace_row_button("hover-workspace", cx)
                        .child("Workspace")
                        .on_click(move |_, _, _| clicks.set(clicks.get() + 1))
                        .on_hover(move |hovered, _, _| {
                            transitions.borrow_mut().push(*hovered);
                        }),
                ),
        )
    }
}

#[gpui::test]
fn workspace_hover_recovers_after_a_missing_mouse_release(cx: &mut gpui::TestAppContext) {
    use gpui::{Modifiers, MouseButton, VisualTestContext, point, px};

    cx.update(gpui_component::init);

    let transitions = Rc::new(RefCell::new(Vec::new()));
    let clicks = Rc::new(Cell::new(0));
    let drags = Rc::new(Cell::new(0));

    let window = cx.add_window({
        let transitions = transitions.clone();
        let clicks = clicks.clone();
        let drags = drags.clone();

        move |_, _| WorkspaceHoverProbe {
            transitions,
            clicks,
            drags,
        }
    });

    let mut cx = VisualTestContext::from_window(window.into(), cx);

    cx.refresh().unwrap();

    for x in 20..40 {
        cx.simulate_mouse_move(point(px(x as f32), px(10.)), None, Modifiers::default());
        cx.refresh().unwrap();
    }

    assert_eq!(*transitions.borrow(), [true]);

    cx.simulate_mouse_down(
        point(px(40.), px(10.)),
        MouseButton::Left,
        Modifiers::default(),
    );

    // Native menus can consume the release before the source window receives it.
    for x in 41..60 {
        cx.simulate_mouse_move(point(px(x as f32), px(10.)), None, Modifiers::default());
        cx.refresh().unwrap();
    }

    assert_eq!(*transitions.borrow(), [true]);

    cx.simulate_mouse_up(
        point(px(60.), px(10.)),
        MouseButton::Left,
        Modifiers::default(),
    );

    cx.refresh().unwrap();

    assert_eq!(*transitions.borrow(), [true]);
    assert_eq!(clicks.get(), 0);
    assert_eq!(drags.get(), 0);

    cx.simulate_mouse_move(point(px(350.), px(100.)), None, Modifiers::default());
    cx.refresh().unwrap();

    assert_eq!(*transitions.borrow(), [true, false]);

    cx.simulate_click(point(px(30.), px(10.)), Modifiers::default());

    assert_eq!(clicks.get(), 1);

    cx.simulate_mouse_down(
        point(px(30.), px(10.)),
        MouseButton::Left,
        Modifiers::default(),
    );

    cx.simulate_mouse_move(
        point(px(31.), px(10.)),
        Some(MouseButton::Left),
        Modifiers::default(),
    );

    cx.simulate_mouse_up(
        point(px(31.), px(10.)),
        MouseButton::Left,
        Modifiers::default(),
    );

    assert_eq!(clicks.get(), 2);

    cx.simulate_mouse_down(
        point(px(30.), px(10.)),
        MouseButton::Left,
        Modifiers::default(),
    );

    cx.simulate_mouse_move(
        point(px(60.), px(10.)),
        Some(MouseButton::Left),
        Modifiers::default(),
    );

    assert_eq!(drags.get(), 1);

    cx.simulate_mouse_up(
        point(px(60.), px(10.)),
        MouseButton::Left,
        Modifiers::default(),
    );
}
