use std::collections::VecDeque;

use crate::agent_pane::view::queued_message_label;

#[test]
fn queued_message_label_keeps_order_and_flattens_lines() {
    let messages = VecDeque::from(["first\nline".to_string(), "second".to_string()]);

    assert_eq!(
        queued_message_label(&messages).as_deref(),
        Some("Queued message: first line · second")
    );
}
