use std::cell::Cell;
use std::rc::Rc;

use gpui::{EmptyView, TestAppContext, actions};

use crate::menu::{Quit, install, with_active_window};

actions!(menu_tests, [ProbeWindow]);

#[gpui::test]
fn window_commands_run_after_action_dispatch_releases_the_window(cx: &mut TestAppContext) {
    let window = cx.add_window(|_, _| EmptyView);
    let invoked = Rc::new(Cell::new(false));
    let completed = invoked.clone();

    cx.update(|cx| {
        cx.on_action(move |_: &ProbeWindow, cx| {
            let completed = completed.clone();

            with_active_window(cx, move |target, _| {
                assert_eq!(target.window_handle(), window.into());

                completed.set(true);
            });
        });
    });

    window
        .update(cx, |_, target, cx| {
            target.activate_window();
            target.dispatch_action(Box::new(ProbeWindow), cx);
        })
        .unwrap();

    cx.run_until_parked();

    assert!(invoked.get());
}

#[gpui::test]
fn quit_without_a_window_reaches_the_platform(cx: &mut TestAppContext) {
    cx.update(|cx| {
        install(cx);

        cx.dispatch_action(&Quit);
    });

    assert!(cx.did_request_quit());
}
