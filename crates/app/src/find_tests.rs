use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    AppContext as _, Context, Entity, FocusHandle, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, Styled as _, TestAppContext, Window, div, px,
};
use gpui_component::input::Escape;

use crate::find::{FindBar, FindEvent, OpenFind, bind_keys};

#[cfg(windows)]
const FIND_SHORTCUT: &str = "ctrl-f";

#[cfg(target_os = "macos")]
const FIND_SHORTCUT: &str = "cmd-f";

struct FindProbe {
    bar: Entity<FindBar>,
    focus: FocusHandle,
    parent_escapes: Rc<RefCell<usize>>,
}

impl Render for FindProbe {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .w(px(600.))
            .h(px(300.))
            .relative()
            .track_focus(&self.focus)
            .key_context("AgentPane")
            .on_action(cx.listener(|this, _: &OpenFind, window, cx| {
                this.bar.update(cx, |bar, cx| bar.open(window, cx))
            }))
            .on_action(cx.listener(|this, _: &Escape, _, _| *this.parent_escapes.borrow_mut() += 1))
            .children(self.bar.read(cx).visible.then(|| self.bar.clone()))
    }
}

#[gpui::test]
fn find_bar_debounces_input_navigates_and_consumes_escape(cx: &mut TestAppContext) {
    let events = Rc::new(RefCell::new(Vec::new()));
    let escapes = Rc::new(RefCell::new(0));

    cx.update(|cx| {
        gpui_component::init(cx);
        bind_keys(cx);
    });

    let (probe, cx) = cx.add_window_view(|window, cx| {
        let bar = cx.new(|cx| FindBar::new(window, cx));
        let focus = cx.focus_handle();

        window.focus(&focus, cx);

        let events = events.clone();

        cx.subscribe(&bar, move |_, _, event, _| {
            let value = match event {
                FindEvent::Editing => "editing".into(),
                FindEvent::Query(query) => format!("query:{query}"),
                FindEvent::Navigate { backwards } => format!("navigate:{backwards}"),
                FindEvent::Close => "close".into(),
            };

            events.borrow_mut().push(value);
        })
        .detach();

        FindProbe {
            bar,
            focus,
            parent_escapes: escapes.clone(),
        }
    });

    let bar = probe.read_with(cx, |probe, _| probe.bar.clone());

    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    cx.simulate_keystrokes(FIND_SHORTCUT);
    probe.update(cx, |_, cx| cx.notify());
    cx.update(|window, cx| window.draw(cx).clear(cx));
    cx.simulate_input("Needle");
    cx.run_until_parked();

    assert_eq!(
        bar.read_with(cx, |bar, cx| bar.input.read(cx).value().to_string()),
        "Needle",
        "events: {:?}",
        events.borrow()
    );
    assert!(!events.borrow().iter().any(|event| event == "query:Needle"));

    cx.executor().advance_clock(Duration::from_millis(110));
    cx.run_until_parked();

    assert!(
        events.borrow().iter().any(|event| event == "query:Needle"),
        "events: {:?}",
        events.borrow()
    );

    bar.update(cx, |bar, cx| bar.results(Some(0), 2, false, cx));
    cx.simulate_keystrokes("enter shift-enter");
    cx.run_until_parked();

    assert!(
        events
            .borrow()
            .iter()
            .any(|event| event == "navigate:false")
    );
    assert!(
        events.borrow().iter().any(|event| event == "navigate:true"),
        "events: {:?}",
        events.borrow()
    );

    cx.simulate_keystrokes(FIND_SHORTCUT);
    cx.simulate_input("replacement");

    assert_eq!(
        bar.read_with(cx, |bar, cx| bar.input.read(cx).value().to_string()),
        "replacement"
    );

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();

    assert!(!bar.read_with(cx, |bar, _| bar.visible));
    assert_eq!(*escapes.borrow(), 0);

    cx.update(|window, cx| probe.read_with(cx, |probe, _| assert!(probe.focus.is_focused(window))));
}
