use std::sync::Arc;

use gpui::TestAppContext;

use crate::agent_tab::settings::AgentSettings;
use crate::agent_tab::transcript::mermaid::render::DiagramTheme;
use crate::agent_tab::transcript::mermaid::store::DiagramStore;

#[gpui::test]
async fn concurrent_and_repeated_requests_share_one_preparation(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.set_global(AgentSettings::default());

    let store = Arc::new(DiagramStore::default());

    let (first, second) = cx.update(|cx| {
        let theme = DiagramTheme::current(cx);
        let source: Arc<str> = Arc::from("flowchart TD\nA --> B");

        (
            store.request(source.clone(), theme.clone(), cx.svg_renderer(), cx),
            store.request(source, theme, cx.svg_renderer(), cx),
        )
    });

    let first = first.await.unwrap();
    let second = second.await.unwrap();

    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(store.results.lock().preparations, 1);

    let repeated = cx.update(|cx| {
        store.request(
            Arc::from("flowchart TD\nA --> B"),
            DiagramTheme::current(cx),
            cx.svg_renderer(),
            cx,
        )
    });

    let repeated = repeated.await.unwrap();

    assert!(Arc::ptr_eq(&first, &repeated));
    assert_eq!(store.results.lock().preparations, 1);

    let edited = cx.update(|cx| {
        store.request(
            Arc::from("flowchart TD\nA --> C"),
            DiagramTheme::current(cx),
            cx.svg_renderer(),
            cx,
        )
    });

    let edited = edited.await.unwrap();

    assert!(!Arc::ptr_eq(&first, &edited));
    assert_eq!(store.preparations(), 2);
}
