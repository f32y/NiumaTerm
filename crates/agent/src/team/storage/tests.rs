use std::fs;

use tempfile::tempdir;

use crate::AgentWorkspace;
use crate::chat::SendOutcome;
use crate::team::attempt::{AttemptState, BudgetScope, DispatchIntent};
use crate::team::budget::{BudgetError, TurnPurpose};
use crate::team::discussion::{
    Arrangement, ArrangementState, DiscussionMode, PauseReason, PublicSnapshot, Stage, StageKind,
};
use crate::team::model::{
    AttemptId, Author, MessageId, OperationId, PublicMessage, Publication, RoomId, StageId,
    Summary, SummaryId, UserInput,
};
use crate::team::room::Room;
use crate::team::session::dispatch::{DispatchError, dispatch, reserve_dispatches};
use crate::team::storage::{RoomStore, StorageError};
use crate::team::tests::config;

#[test]
fn restart_retains_sources_scopes_controls_pending_work_and_budget() {
    let directory = tempdir().unwrap();

    let mut room = Room::new(AgentWorkspace::single(Some("C:/room".into())));

    let alice = room.add_member(config("Alice", "C:/frontend")).unwrap();
    let bob = room.add_member(config("Bob", "C:/backend")).unwrap();
    let id = room.id();

    let mut store = RoomStore::create(directory.path(), room.clone()).unwrap();

    let message = MessageId::new();

    room.messages.push(PublicMessage {
        id: message,
        author: Author::Member {
            id: alice,
            name: "Alice".into(),
        },
        publication: Publication::RootReply,
        text: "Use separate roots".into(),
        replies_to: Vec::new(),
    });

    room.summaries.push(Summary {
        fragments: Vec::new(),
        id: SummaryId::new(),
        version: 1,
        owner: bob,
        sources: vec![message],
        prior_summaries: Vec::new(),
        goals: "Compare approaches".into(),
        constraints: "Keep roots separate".into(),
        agreements: String::new(),
        disagreements: Vec::new(),
    });

    room.messages.push(PublicMessage {
        id: MessageId::new(),
        author: Author::User,
        publication: Publication::UserInput,
        text: "Review this".into(),
        replies_to: vec![message],
    });

    room.create_discussion(
        "Compare".into(),
        vec![alice, bob],
        DiscussionMode::Moderated { moderator: bob },
    )
    .unwrap();

    room.discussions[0].pause(PauseReason::User);

    room.discussions[0].stages.push(Stage {
        decision: None,
        id: StageId::new(),
        kind: StageKind::InvitedResponses,
        arrangements: vec![Arrangement {
            operation: OperationId::new(),
            recipient: alice,
            state: ArrangementState::Pending,
        }],
        segments: vec![PublicSnapshot {
            messages: vec![message],
            summaries: vec![room.summaries[0].id],
        }],
    });

    room.controls.automatic_summaries = false;

    store.commit(room.clone()).unwrap();

    room.rename_member(alice, "Alice frontend").unwrap();

    store.commit(room.clone()).unwrap();

    drop(store);

    let store = RoomStore::open(directory.path(), id).unwrap();

    assert_eq!(store.revision(), 2);
    assert_eq!(store.room(), &room);
    assert!(RoomStore::open(directory.path(), id).is_err());

    let metadata = fs::read_to_string(store.directory.join("room.json")).unwrap();

    for forbidden_field in ["api_key", "api_base_url", "executable", "env"] {
        assert!(!metadata.contains(&format!("\"{forbidden_field}\"")));
    }
}

#[test]
fn invalid_or_unsupported_snapshot_preserves_saved_bytes_and_other_rooms() {
    let directory = tempdir().unwrap();
    let room = Room::new(AgentWorkspace::default());
    let id = room.id();
    let store = RoomStore::create(directory.path(), room).unwrap();
    let path = store.directory.join("room.json");
    let original = fs::read(&path).unwrap();

    drop(store);

    let other = Room::new(AgentWorkspace::default());
    let other_id = other.id();

    drop(RoomStore::create(directory.path(), other).unwrap());

    for invalid in [
        b"{\"version\":3,".to_vec(),
        b"{\"version\":2}".to_vec(),
        b"{\"version\":999}".to_vec(),
    ] {
        fs::write(&path, &invalid).unwrap();

        assert!(RoomStore::open(directory.path(), id).is_err());
        assert_eq!(fs::read(&path).unwrap(), invalid);
        assert!(RoomStore::open(directory.path(), other_id).is_ok());
    }

    fs::write(&path, original).unwrap();

    assert!(RoomStore::open(directory.path(), id).is_ok());
}

#[test]
fn legacy_room_is_rejected_without_modifying_saved_files() {
    let directory = tempdir().unwrap();
    let id = RoomId::new();
    let room_directory = directory.path().join("agent-teams").join(id.to_string());

    fs::create_dir_all(&room_directory).unwrap();

    for filename in ["checkpoint.json", "journal.jsonl"] {
        fs::write(room_directory.join(filename), b"legacy bytes").unwrap();
    }

    assert!(matches!(
        RoomStore::open(directory.path(), id),
        Err(StorageError::LegacyFormat)
    ));
    assert!(!room_directory.join("room.json").exists());

    for filename in ["checkpoint.json", "journal.jsonl"] {
        assert_eq!(
            fs::read(room_directory.join(filename)).unwrap(),
            b"legacy bytes"
        );
    }
}

#[test]
fn storage_failures_preserve_input_and_reservations_without_backend_dispatch() {
    let directory = tempdir().unwrap();

    let mut room = Room::new(AgentWorkspace::default());

    let alice = room.add_member(config("Alice", "C:/a")).unwrap();
    let room_id = room.id();

    let mut store = RoomStore::create(directory.path(), room).unwrap();

    let operation = OperationId::new();

    let input = UserInput {
        text: "Keep this draft".into(),
        ..UserInput::default()
    };

    let intent = DispatchIntent {
        invocation: Default::default(),
        backend_generation: 1,
        coverage: Default::default(),
        recipient: alice,

        operation,
        stage: None,
        budget: BudgetScope::Direct(operation),
        purpose: TurnPurpose::Response,
        input: input.clone(),
        prepared_text: input.text.clone(),
        snapshot: PublicSnapshot::default(),
    };

    let directory_before_failure = store.directory.clone();

    store.directory = store.directory.join("missing-directory");

    let mut sends = 0;

    let result = reserve_dispatches(&mut store, vec![intent.clone()]).and_then(|ids| {
        dispatch(&mut store, ids[0], |_| {
            sends += 1;

            SendOutcome::StartedTurn
        })
    });

    assert!(result.is_err());
    assert_eq!(sends, 0);
    assert!(store.room().attempts().is_empty());
    assert_eq!(input.text, "Keep this draft");

    assert!(matches!(
        store.commit(store.room().clone()),
        Err(StorageError::ReopenRequired)
    ));

    store.directory = directory_before_failure;

    drop(store);

    let mut store = RoomStore::open(directory.path(), room_id).unwrap();

    let ids = reserve_dispatches(&mut store, vec![intent.clone()]).unwrap();

    let directory_before_failure = store.directory.clone();

    store.directory = store.directory.join("missing-directory");

    assert!(
        dispatch(&mut store, ids[0], |_| {
            sends += 1;
            SendOutcome::StartedTurn
        })
        .is_err()
    );
    assert_eq!(sends, 0);
    assert_eq!(store.room().attempts()[0].state, AttemptState::Reserved);
    assert_eq!(store.room().attempts()[0].intent.input, input);

    assert!(matches!(
        store.commit(store.room().clone()),
        Err(StorageError::ReopenRequired)
    ));

    store.directory = directory_before_failure;

    drop(store);

    let mut store = RoomStore::open(directory.path(), room_id).unwrap();

    assert_eq!(
        store
            .room
            .budget_attempts(BudgetScope::Direct(operation))
            .count(),
        1
    );

    dispatch(&mut store, ids[0], |_| {
        sends += 1;

        SendOutcome::StartedTurn
    })
    .unwrap();

    assert!(
        dispatch(&mut store, ids[0], |_| {
            sends += 1;
            SendOutcome::StartedTurn
        })
        .is_err()
    );
    assert_eq!(sends, 1);
    assert_eq!(
        store
            .room
            .budget_attempts(BudgetScope::Direct(operation))
            .count(),
        1
    );

    drop(store);

    let mut store = RoomStore::open(directory.path(), room_id).unwrap();

    assert!(
        dispatch(&mut store, ids[0], |_| {
            sends += 1;
            SendOutcome::StartedTurn
        })
        .is_err()
    );
    assert_eq!(sends, 1);
}

#[test]
fn batch_admission_is_atomic_and_only_unsent_or_rejected_work_releases_allowance() {
    let directory = tempdir().unwrap();

    let mut room = Room::new(AgentWorkspace::default());

    let member = room.add_member(config("Alice", "C:/a")).unwrap();

    let discussion = room
        .create_discussion(
            "Review".into(),
            vec![member],
            DiscussionMode::Fixed {
                report_author: member,
            },
        )
        .unwrap()
        .id();

    let mut store = RoomStore::create(directory.path(), room).unwrap();

    let intent = |purpose| DispatchIntent {
        invocation: Default::default(),
        recipient: member,
        backend_generation: 1,
        operation: OperationId::new(),
        stage: None,
        budget: BudgetScope::Discussion(discussion),
        purpose,
        input: UserInput::default(),
        prepared_text: "Review".into(),
        snapshot: PublicSnapshot::default(),
        coverage: Default::default(),
    };

    reserve_dispatches(
        &mut store,
        (0..10).map(|_| intent(TurnPurpose::Response)).collect(),
    )
    .unwrap();

    let before = store.room().clone();

    assert!(matches!(
        reserve_dispatches(
            &mut store,
            vec![
                intent(TurnPurpose::Summary),
                intent(TurnPurpose::Moderation)
            ]
        ),
        Err(DispatchError::Budget(BudgetError::InsufficientTurns))
    ));
    assert_eq!(store.room(), &before);

    let summary = reserve_dispatches(&mut store, vec![intent(TurnPurpose::Summary)]).unwrap()[0];
    let report = reserve_dispatches(&mut store, vec![intent(TurnPurpose::Report)]).unwrap()[0];

    let mut room = store.room().clone();

    room.attempts
        .iter_mut()
        .find(|attempt| attempt.id == summary)
        .unwrap()
        .state = AttemptState::Rejected;

    room.attempts
        .iter_mut()
        .find(|attempt| attempt.id == report)
        .unwrap()
        .state = AttemptState::Sending;

    store.commit(room).unwrap();

    assert!(matches!(
        reserve_dispatches(&mut store, vec![intent(TurnPurpose::Moderation)]),
        Err(DispatchError::Budget(BudgetError::InsufficientTurns))
    ));

    let mut room = store.room().clone();

    room.discussion_mut(discussion)
        .unwrap()
        .budget
        .add_turns(1)
        .unwrap();

    store.commit(room).unwrap();
    reserve_dispatches(&mut store, vec![intent(TurnPurpose::Moderation)]).unwrap();

    let room_id = store.room().id();
    let expected = store.room().clone();

    drop(store);

    let store = RoomStore::open(directory.path(), room_id).unwrap();

    assert_eq!(store.room(), &expected);
    assert_eq!(
        store
            .room()
            .discussion(discussion)
            .unwrap()
            .remaining_non_report_turns(store.room().attempts()),
        0
    );
}

#[test]
fn direct_request_budget_stays_scoped_and_bounded_after_reopening() {
    let directory = tempdir().unwrap();

    let mut room = Room::new(AgentWorkspace::default());

    let member = room.add_member(config("Alice", "C:/a")).unwrap();
    let room_id = room.id();
    let operation = OperationId::new();

    let intent = |operation| DispatchIntent {
        invocation: Default::default(),
        recipient: member,
        backend_generation: 1,
        operation,
        stage: None,
        budget: BudgetScope::Direct(operation),
        purpose: TurnPurpose::Response,
        input: UserInput::default(),
        prepared_text: "Review".into(),
        snapshot: PublicSnapshot::default(),
        coverage: Default::default(),
    };

    let mut store = RoomStore::create(directory.path(), room).unwrap();

    reserve_dispatches(&mut store, vec![intent(operation); 12]).unwrap();

    drop(store);

    let mut store = RoomStore::open(directory.path(), room_id).unwrap();

    assert!(matches!(
        reserve_dispatches(&mut store, vec![intent(operation)]),
        Err(DispatchError::Budget(BudgetError::InsufficientTurns))
    ));
    assert_eq!(store.room().attempts().len(), 12);

    let mut invalid = store.room().clone();
    let mut extra = invalid.attempts[0].clone();

    extra.id = AttemptId::new();

    invalid.attempts.push(extra);

    assert!(matches!(
        store.commit(invalid),
        Err(StorageError::Invalid(_))
    ));

    reserve_dispatches(&mut store, vec![intent(OperationId::new())]).unwrap();

    assert_eq!(store.room().attempts().len(), 13);
}

/// Rooms written before member coverage, the input history and the
/// discussion objective were derived still load.
#[test]
fn rooms_with_retired_records_still_load() {
    let mut room = Room::new(AgentWorkspace::default());

    let alice = room.add_member(config("Alice", "C:/a")).unwrap();

    room.create_discussion(
        "Compare".into(),
        vec![alice],
        DiscussionMode::Moderated { moderator: alice },
    )
    .unwrap();

    let mut stored = serde_json::to_value(&room).unwrap();

    stored["members"][0]["coverage"] = serde_json::json!({"messages": [], "summaries": []});
    stored["input_history"] = serde_json::json!([{"text": "Compare", "references": []}]);
    stored["discussions"][0]["objective"] = serde_json::json!("Compare");

    let loaded: Room = serde_json::from_value(stored).unwrap();

    assert_eq!(loaded, room);
}
