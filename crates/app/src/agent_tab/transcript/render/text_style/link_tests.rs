use std::cell::Cell;
use std::path::Path;
use std::rc::Rc;

use gpui::{
    Context, InteractiveElement as _, IntoElement, Modifiers, MouseButton, ParentElement as _,
    Render, Styled as _, TestAppContext, Window, div, point, px,
};
use gpui_component::modern_menu::ModernMenuExt as _;

use crate::agent_tab::transcript::render::text_style::{markdown_view, resolve_local_path};

struct TranscriptLinkTestView {
    row_menu_opens: Rc<Cell<usize>>,
    markdown: &'static str,
}

impl Render for TranscriptLinkTestView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let opens = self.row_menu_opens.clone();

        div()
            .id("reply-row")
            .w(px(400.))
            .modern_context_menu(move |menu, _, _| {
                opens.set(opens.get() + 1);

                menu
            })
            .child(markdown_view("reply-text", self.markdown, None))
    }
}

#[gpui::test]
fn right_clicking_a_transcript_link_skips_the_whole_reply_menu(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);

    let opens = Rc::new(Cell::new(0));
    let row_menu_opens = opens.clone();

    let (_, cx) = cx.add_window_view(move |_, _| TranscriptLinkTestView {
        row_menu_opens,
        markdown: "[Repository](https://example.com/repository)\n\nRest of the answer.",
    });

    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));

    let position = point(px(10.), px(10.));

    cx.simulate_mouse_down(position, MouseButton::Right, Modifiers::default());
    cx.simulate_mouse_up(position, MouseButton::Right, Modifiers::default());
    cx.run_until_parked();

    assert_eq!(opens.get(), 0);
    assert_eq!(cx.opened_url(), None);

    cx.simulate_click(position, Modifiers::default());

    assert_eq!(
        cx.opened_url().as_deref(),
        Some("https://example.com/repository")
    );

    let position = point(px(10.), px(50.));

    cx.simulate_mouse_down(position, MouseButton::Right, Modifiers::default());
    cx.simulate_mouse_up(position, MouseButton::Right, Modifiers::default());

    assert_eq!(opens.get(), 1);
}

#[gpui::test]
fn right_clicking_a_file_icon_skips_the_whole_reply_menu(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);

    let opens = Rc::new(Cell::new(0));
    let row_menu_opens = opens.clone();

    let (_, cx) = cx.add_window_view(move |_, _| TranscriptLinkTestView {
        row_menu_opens,
        markdown: "[Cargo.toml](Cargo.toml:7)",
    });

    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));

    for x in [5., 30.] {
        let position = point(px(x), px(10.));

        cx.simulate_mouse_down(position, MouseButton::Right, Modifiers::default());
        cx.simulate_mouse_up(position, MouseButton::Right, Modifiers::default());
        cx.run_until_parked();

        assert_eq!(opens.get(), 0);
        assert_eq!(cx.opened_url(), None);
    }
}

#[test]
fn resolves_agent_file_locations() {
    let cwd = Path::new("C:/Workspace/NiumaTerm");

    assert_eq!(
        resolve_local_path("crates/app/src/main.rs:42", Some(cwd)),
        Some(cwd.join("crates/app/src/main.rs"))
    );
    assert_eq!(
        resolve_local_path("C:/Workspace/NiumaTerm/Cargo.toml:582", Some(cwd)),
        Some("C:/Workspace/NiumaTerm/Cargo.toml".into())
    );
    assert_eq!(
        resolve_local_path("/C:/Workspace/NiumaTerm/Cargo.toml:111", Some(cwd)),
        Some("C:/Workspace/NiumaTerm/Cargo.toml".into())
    );
    assert_eq!(
        resolve_local_path("crates/app/src/main.rs:42:7", Some(cwd)),
        Some(cwd.join("crates/app/src/main.rs"))
    );
    assert_eq!(
        resolve_local_path("https://example.com/file.rs:42", Some(cwd)),
        None
    );
}
