use crate::chat::{Compaction, Item};
use crate::transcript::{TextField, TranscriptContent, TranscriptEntry};

fn entry(turn: u64, item: Item) -> TranscriptEntry {
    TranscriptEntry {
        turn,
        item,
        metadata: (),
    }
}

fn reply(id: &str, text: Option<&str>) -> Item {
    Item::AgentMessage {
        id: id.into(),
        text: text.map(str::to_owned),
        questions: None,
    }
}

fn reasoning(id: &str, summary: Option<&str>) -> Item {
    Item::Reasoning {
        id: id.into(),
        summary: summary.map(str::to_owned),
    }
}

#[test]
fn indexed_deltas_select_the_first_compatible_duplicate_without_changing_other_entries() {
    let mut content = TranscriptContent::default();

    assert_eq!(
        content.append(entry(1, reasoning("shared", Some("thinking")))),
        0
    );
    assert_eq!(
        content.append(entry(1, reply("shared", Some("hello 世界")))),
        1
    );
    content.append(entry(2, reply("shared", Some("later"))));

    let update = content
        .append_delta("shared", "!", TextField::Reply)
        .unwrap();

    assert_eq!(update.index, 1);
    assert_eq!(update.previous_bytes, "hello 世界".len());
    assert!(update.non_blank);
    assert_eq!(
        content.entries()[0].item,
        reasoning("shared", Some("thinking"))
    );
    assert_eq!(
        content.entries()[1].item,
        reply("shared", Some("hello 世界!"))
    );
    assert_eq!(content.entries()[2].item, reply("shared", Some("later")));

    assert!(
        content
            .append_delta("missing", "ignored", TextField::Reply)
            .is_none()
    );
    assert!(
        content
            .append_delta("shared", "ignored", TextField::CommandOutput)
            .is_none()
    );

    let update = content
        .append_delta("shared", " more", TextField::ReasoningSummary)
        .unwrap();

    assert_eq!(update.index, 0);
    assert_eq!(
        content.entries()[0].item,
        reasoning("shared", Some("thinking more"))
    );
}

#[test]
fn empty_and_whitespace_deltas_are_accepted_without_claiming_non_blank_content() {
    let mut content = TranscriptContent::default();
    content.append(entry(1, reply("answer", None)));

    for delta in ["", " \n", "\u{3000}"] {
        assert!(
            !content
                .append_delta("answer", delta, TextField::Reply)
                .unwrap()
                .non_blank
        );
    }

    let update = content
        .append_delta("answer", "yes", TextField::Reply)
        .unwrap();

    assert_eq!(update.previous_bytes, " \n\u{3000}".len());
    assert!(update.non_blank);
    assert_eq!(content.latest_agent_message(1), Some(" \n\u{3000}yes"));
}

#[test]
fn completions_keep_streamed_fields_and_report_the_matching_entry() {
    let mut content = TranscriptContent::default();
    content.append(entry(1, reasoning("shared", Some("keep this"))));
    content.append(entry(1, reply("shared", Some("streamed answer"))));
    content.append(entry(2, reply("shared", Some("later"))));

    assert_eq!(content.merge_completed(&reply("shared", None)), Some(1));
    assert_eq!(
        content.entries()[1].item,
        reply("shared", Some("streamed answer"))
    );
    assert_eq!(
        content.merge_completed(&reply("shared", Some("complete"))),
        Some(1)
    );
    assert_eq!(content.entries()[2].item, reply("shared", Some("later")));
    assert_eq!(
        content.merge_completed(&reasoning("shared", Some("replacement"))),
        Some(0)
    );
    assert_eq!(
        content.entries()[0].item,
        reasoning("shared", Some("keep this"))
    );
    assert_eq!(
        content.merge_completed(&reply("missing", Some("ignored"))),
        None
    );
    assert_eq!(
        content.merge_completed(&Item::Error {
            text: "error".into()
        }),
        None
    );

    content.append(entry(
        3,
        Item::CommandExecution {
            id: "command".into(),
            command: "echo hello".into(),
            purpose: Some("original purpose".into()),
            aggregated_output: None,
            status: Some("running".into()),
            exit_code: None,
        },
    ));

    let update = content
        .append_delta("command", "hello\n", TextField::CommandOutput)
        .unwrap();

    assert_eq!(update.index, 3);
    assert_eq!(update.previous_bytes, 0);
    assert!(update.non_blank);
    assert_eq!(
        content.merge_completed(&Item::CommandExecution {
            id: "command".into(),
            command: "echo hello".into(),
            purpose: None,
            aggregated_output: None,
            status: Some("completed".into()),
            exit_code: Some(0),
        }),
        Some(3)
    );
    assert!(
        matches!(&content.entries()[3].item, Item::CommandExecution {
        purpose: Some(purpose), aggregated_output: Some(output), status: Some(status), exit_code: Some(0), ..
    } if purpose == "original purpose" && output == "hello\n" && status == "completed")
    );
}

#[test]
fn replacement_and_clear_retire_old_ids_and_preserve_incoming_order() {
    let mut content = TranscriptContent::default();
    content.append(entry(1, reply("old", Some("old"))));
    content.replace(vec![
        entry(5, reasoning("new", None)),
        entry(6, reply("new", None)),
    ]);

    assert!(!content.contains_item("old"));
    assert!(content.contains_item("new"));
    assert!(
        content
            .append_delta("old", "stale", TextField::Reply)
            .is_none()
    );
    assert_eq!(
        content
            .append_delta("new", "restored", TextField::Reply)
            .unwrap()
            .index,
        1
    );
    assert_eq!(content.entries()[1].turn, 6);

    content.clear();

    assert!(content.entries().is_empty());
    assert!(!content.contains_item("new"));
    content.append(entry(1, reply("new", None)));
    assert_eq!(
        content
            .append_delta("new", "fresh", TextField::Reply)
            .unwrap()
            .index,
        0
    );
}

#[test]
fn content_queries_respect_turns_blank_replies_and_latest_task_lists() {
    let mut content = TranscriptContent::default();
    content.append(entry(
        1,
        Item::UserMessage {
            text: Some("question".into()),
        },
    ));
    content.append(entry(1, reasoning("reason", None)));
    content.append(entry(
        1,
        Item::FileChange {
            id: "file".into(),
            paths: "main.rs".into(),
            diff: None,
            status: None,
        },
    ));
    content.append(entry(
        1,
        Item::Other {
            id: "tasks".into(),
            kind: "TodoWrite".into(),
            title: "plan".into(),
            output: Some("- [x] done\n- [ ] pending".into()),
            status: None,
        },
    ));
    content.append(entry(
        1,
        Item::Compaction {
            id: "compact".into(),
            detail: Compaction::default(),
        },
    ));
    content.append(entry(
        1,
        Item::Error {
            text: "failure".into(),
        },
    ));
    content.append(entry(1, reply("first", Some("answer"))));
    content.append(entry(1, reply("blank", Some(" \n"))));
    content.append(entry(2, reply("second", Some("other turn"))));

    assert_eq!(content.latest_agent_message(1), Some("answer"));
    assert_eq!(content.latest_agent_message(3), None);
    assert!(content.turn_has_error(1, "failure"));
    assert!(!content.turn_has_error(2, "failure"));
    assert!(!content.turn_has_error(1, "different"));
    assert_eq!(content.turn_steps(1), 3);
    assert_eq!(content.turn_steps(2), 0);
    assert_eq!(content.task_tally(), Some((1, 2)));

    content.append(entry(
        2,
        Item::Other {
            id: "tasks-2".into(),
            kind: "TodoWrite".into(),
            title: "plan".into(),
            output: Some("- [x] finished".into()),
            status: None,
        },
    ));
    assert_eq!(content.task_tally(), Some((1, 1)));
}

#[test]
fn appends_move_content_and_metadata_without_cloning_them() {
    struct Metadata(String);

    let mut text = String::with_capacity(1024);
    text.push_str("existing");
    let original_text = text.as_ptr();
    let metadata = Metadata("local display data".into());
    let original_metadata = metadata.0.as_ptr();
    let mut content = TranscriptContent::default();

    content.append(TranscriptEntry {
        turn: 7,
        item: Item::AgentMessage {
            id: "answer".into(),
            text: Some(text),
            questions: None,
        },
        metadata,
    });
    content
        .append_delta("answer", " suffix", TextField::Reply)
        .unwrap();

    assert_eq!(
        content.latest_agent_message(7).unwrap().as_ptr(),
        original_text
    );
    assert_eq!(content.entries()[0].metadata.0.as_ptr(), original_metadata);
    assert_eq!(content.entries()[0].metadata.0, "local display data");
    assert_eq!(content.latest_agent_message(7), Some("existing suffix"));
}
