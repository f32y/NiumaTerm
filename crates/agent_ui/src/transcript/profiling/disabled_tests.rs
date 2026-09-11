use std::mem::size_of;

use gpui::{TestAppContext, frame_stats};
use nmt_agent::chat::Item;
use nmt_agent::transcript::TextField;
use nmt_config::agent::CollapseRows;
use nmt_profiling::transcript::{Operation, Probe, flush};

use crate::profile::AgentKind;
use crate::transcript::{Entry, TranscriptView};

#[gpui::test]
fn disabled_hooks_leave_transcript_updates_available(_cx: &mut TestAppContext) {
    frame_stats::set_enabled(true);

    assert_eq!(size_of::<Option<Probe>>(), 0);
    assert!(Probe::start(Operation::AppendEntry).is_none());

    let mut view = TranscriptView::new(AgentKind::Codex, None);

    view.append_entry(Entry {
        turn: 1,
        metadata: Default::default(),
        item: Item::Reasoning {
            id: "reasoning".into(),
            summary: Some("before".into()),
        },
    });

    assert!(view.append_delta("reasoning", "-after", TextField::ReasoningSummary));

    view.refresh_rows(CollapseRows::WorkAndToolCalls);

    assert!(matches!(
        &view.conversation.borrow().content.entries()[0].item,
        Item::Reasoning { summary: Some(text), .. } if text == "before-after"
    ));

    flush();

    assert!(Probe::start(Operation::AppendDelta).is_none());

    frame_stats::set_enabled(false);
}
