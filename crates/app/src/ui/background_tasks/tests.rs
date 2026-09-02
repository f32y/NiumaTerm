use std::time::{Duration, SystemTime, UNIX_EPOCH};

use nmt_agent_utils::background_task::{
    BackgroundTaskDiscoveryState, BackgroundTaskKey, BackgroundTaskRegistry,
    BackgroundTaskSnapshot, BackgroundTaskState, BackgroundTaskUpdate,
};

use crate::ui::background_tasks::rows::{
    duration_label, finished_heading, finished_rows, row_detail, row_timing, running_heading,
    running_rows, section_control_label, visible_rows,
};
use crate::ui::background_tasks::{COMPACT_FINISHED_ROWS, COMPACT_RUNNING_ROWS};

fn at(seconds: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(seconds)
}

struct Builder(BackgroundTaskRegistry);

impl Builder {
    fn codex() -> Self {
        Self(BackgroundTaskRegistry::new(BackgroundTaskKey::codex(
            "thr_parent",
        )))
    }

    fn claude() -> Self {
        Self(BackgroundTaskRegistry::new(BackgroundTaskKey::claude_code(
            "sess-1",
        )))
    }

    fn task(mut self, key: BackgroundTaskKey, update: BackgroundTaskUpdate) -> Self {
        self.0.apply(key, update);
        self
    }

    fn discovery(mut self, discovery: BackgroundTaskDiscoveryState) -> Self {
        self.0.set_discovery(discovery);
        self
    }

    fn build(self) -> BackgroundTaskSnapshot {
        self.0.snapshot()
    }
}

fn running(name: &str, started: Option<u64>) -> BackgroundTaskUpdate {
    BackgroundTaskUpdate {
        state: Some(BackgroundTaskState::Working),
        display_name: Some(name.to_owned()),
        started_at: started.map(at),
        ..BackgroundTaskUpdate::default()
    }
}

fn finished(state: BackgroundTaskState, completed: Option<u64>) -> BackgroundTaskUpdate {
    BackgroundTaskUpdate {
        state: Some(state),
        completed_at: completed.map(at),
        ..BackgroundTaskUpdate::default()
    }
}

#[test]
fn mixed_states_group_into_running_and_finished_with_their_counts() {
    let snapshot = Builder::codex()
        .task(BackgroundTaskKey::codex("a"), running("worker", Some(100)))
        .task(
            BackgroundTaskKey::codex("b"),
            BackgroundTaskUpdate::state(BackgroundTaskState::NeedsInput),
        )
        .task(
            BackgroundTaskKey::codex("c"),
            finished(BackgroundTaskState::Done, Some(300)),
        )
        .task(
            BackgroundTaskKey::codex("d"),
            finished(BackgroundTaskState::Failed, Some(400)),
        )
        .build();

    assert_eq!(running_rows(&snapshot).len(), 2);
    assert_eq!(finished_rows(&snapshot).len(), 2);
    assert_eq!(running_heading(&snapshot), "Running · 2 · 1 need input");
    assert_eq!(finished_heading(&snapshot), "Finished · 2");

    // Failed and Stopped keep their own labels rather than a shared one.
    let labels: Vec<_> = finished_rows(&snapshot)
        .iter()
        .map(|task| task.state.label())
        .collect();
    assert!(labels.contains(&"Failed"));
    assert!(labels.contains(&"Done"));
}

#[test]
fn running_rows_lead_with_the_earliest_start_and_finished_with_the_latest_end() {
    let snapshot = Builder::codex()
        .task(BackgroundTaskKey::codex("late"), running("late", Some(300)))
        .task(
            BackgroundTaskKey::codex("early"),
            running("early", Some(100)),
        )
        .task(
            BackgroundTaskKey::codex("untimed"),
            running("untimed", None),
        )
        .task(
            BackgroundTaskKey::codex("old"),
            finished(BackgroundTaskState::Done, Some(500)),
        )
        .task(
            BackgroundTaskKey::codex("new"),
            finished(BackgroundTaskState::Stopped, Some(900)),
        )
        .build();

    let running: Vec<_> = running_rows(&snapshot)
        .iter()
        .map(|task| task.key.id.clone())
        .collect();
    assert_eq!(running, ["early", "late", "untimed"]);

    let finished: Vec<_> = finished_rows(&snapshot)
        .iter()
        .map(|task| task.key.id.clone())
        .collect();
    assert_eq!(finished, ["new", "old"]);
}

#[test]
fn both_providers_render_from_the_same_snapshot_shape() {
    let claude = Builder::claude()
        .task(
            BackgroundTaskKey::claude_code("toolu_1"),
            running("Review the diff", Some(100)),
        )
        .build();

    let rows = running_rows(&claude);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].key.provider.label(), "Claude Code");
    assert_eq!(rows[0].display_label(), "Review the diff");
}

#[test]
fn a_row_without_optional_metadata_still_reads_as_an_entry() {
    let snapshot = Builder::codex()
        .task(
            BackgroundTaskKey::codex("thr_01H9ZQF4"),
            BackgroundTaskUpdate::state(BackgroundTaskState::Working),
        )
        .build();

    let task = &running_rows(&snapshot)[0];
    assert_eq!(task.display_label(), "Agent 01H9ZQF4");
    assert_eq!(row_detail(task), "No description reported");
    assert_eq!(row_timing(task, at(200)), None);
}

#[test]
fn active_rows_show_elapsed_time_and_terminal_rows_show_a_relative_end() {
    let snapshot = Builder::codex()
        .task(BackgroundTaskKey::codex("a"), running("worker", Some(100)))
        .task(
            BackgroundTaskKey::codex("b"),
            finished(BackgroundTaskState::Done, Some(500)),
        )
        .build();

    assert_eq!(
        row_timing(running_rows(&snapshot)[0], at(190)),
        Some("1m".to_string())
    );
    assert_eq!(
        row_timing(finished_rows(&snapshot)[0], at(560)),
        Some("1m ago".to_string())
    );

    assert_eq!(duration_label(at(105), at(100)), "5s");
    assert_eq!(duration_label(at(7300), at(100)), "2h");
    assert_eq!(duration_label(at(200_000), at(100)), "2d");
}

#[test]
fn compact_sections_hide_the_tail_behind_a_control() {
    assert_eq!(visible_rows(9, COMPACT_RUNNING_ROWS, false), 4);
    assert_eq!(visible_rows(9, COMPACT_RUNNING_ROWS, true), 9);
    assert_eq!(visible_rows(3, COMPACT_RUNNING_ROWS, false), 3);
    assert_eq!(visible_rows(25, COMPACT_FINISHED_ROWS, false), 10);

    assert_eq!(
        section_control_label(5, false).as_deref(),
        Some("Show 5 more")
    );
    assert_eq!(section_control_label(0, false), None);
    assert_eq!(
        section_control_label(0, true).as_deref(),
        Some("Show fewer")
    );
}

#[test]
fn a_failed_restoration_with_no_rows_is_distinguishable_from_an_empty_session() {
    let unavailable = Builder::codex()
        .discovery(BackgroundTaskDiscoveryState::Unavailable {
            message: "thread/list failed".into(),
        })
        .build();
    assert!(unavailable.tasks.is_empty());
    assert!(matches!(
        unavailable.discovery,
        BackgroundTaskDiscoveryState::Unavailable { .. }
    ));

    let empty = Builder::codex()
        .discovery(BackgroundTaskDiscoveryState::Ready)
        .build();
    assert!(empty.tasks.is_empty());
    assert_eq!(empty.discovery, BackgroundTaskDiscoveryState::Ready);
}

mod detail_navigation {
    use nmt_agent_utils::background_task::{
        BackgroundTaskKey, BackgroundTaskTranscript, BackgroundTaskTranscriptState,
        BackgroundTaskTranscriptUpdate, MAX_TRANSCRIPT_ITEMS,
    };
    use nmt_agent_utils::chat::Item;

    use crate::ui::background_tasks::PanelMode;

    #[test]
    fn opening_a_child_replaces_the_list_and_returning_restores_it() {
        let mut mode = PanelMode::List;
        assert_eq!(mode.detail_key(), None);
        assert_eq!(mode.close(), None, "the list is already showing");

        let key = BackgroundTaskKey::codex("thr_child");
        mode.open(key.clone(), true, false);

        assert_eq!(mode.detail_key(), Some(&key));
        // One view at a time: opening a child is not a second column.
        assert_eq!(
            mode.close(),
            Some((true, false)),
            "returning restores the sections the user had open"
        );
        assert_eq!(mode.detail_key(), None);
    }

    #[test]
    fn each_child_is_opened_in_its_own_right() {
        let mut mode = PanelMode::List;
        mode.open(BackgroundTaskKey::codex("a"), false, false);
        mode.open(BackgroundTaskKey::claude_code("a"), false, true);

        // Same local id, different providers: the qualified key keeps them apart.
        assert_eq!(
            mode.detail_key(),
            Some(&BackgroundTaskKey::claude_code("a"))
        );
        assert_eq!(mode.close(), Some((false, true)));
    }

    #[test]
    fn a_failed_read_reports_itself_without_hiding_what_is_known() {
        let mut transcript = BackgroundTaskTranscript::default();
        BackgroundTaskTranscriptUpdate::appended(vec![Item::AgentMessage {
            id: "a".into(),
            text: Some("partial output".into()),
        }])
        .apply_to(&mut transcript);

        BackgroundTaskTranscriptUpdate::state(BackgroundTaskTranscriptState::Unavailable {
            message: "thread/read failed".into(),
        })
        .apply_to(&mut transcript);

        assert_eq!(transcript.items().len(), 1);
        assert!(matches!(
            transcript.state(),
            BackgroundTaskTranscriptState::Unavailable { .. }
        ));
    }

    #[test]
    fn a_truncated_conversation_reports_what_is_missing() {
        let mut transcript = BackgroundTaskTranscript::default();
        for index in 0..MAX_TRANSCRIPT_ITEMS + 3 {
            transcript.push(Item::AgentMessage {
                id: format!("m{index}"),
                text: Some("line".into()),
            });
        }

        assert_eq!(
            transcript.dropped(),
            3,
            "the view states what the retention bound removed"
        );
    }

    #[test]
    fn a_revision_changes_only_when_the_conversation_does() {
        let mut transcript = BackgroundTaskTranscript::default();
        let start = transcript.revision();

        BackgroundTaskTranscriptUpdate::appended(vec![Item::AgentMessage {
            id: "a".into(),
            text: Some("one".into()),
        }])
        .apply_to(&mut transcript);
        let after_append = transcript.revision();
        assert!(after_append > start);

        BackgroundTaskTranscriptUpdate::appended(Vec::new()).apply_to(&mut transcript);
        assert_eq!(
            transcript.revision(),
            after_append,
            "an empty update must not force the view to rebuild"
        );
    }
}
