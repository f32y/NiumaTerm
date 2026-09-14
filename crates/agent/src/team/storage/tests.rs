use crate::AgentWorkspace;
use crate::chat::SendOutcome;
use crate::team::attempt::{AttemptState, BudgetScope, DispatchIntent};
use crate::team::budget::TurnPurpose;
use crate::team::content::{
    AttachmentReference, Author, PublicMessage, Publication, Summary, UserInput,
};
use crate::team::discussion::{
    Arrangement, ArrangementState, DiscussionMode, PauseReason, PublicSnapshot, Stage, StageKind,
};
#[cfg(test)]
use crate::team::identity::AttachmentId;
use crate::team::identity::{
    AttemptId, MessageId, OperationId, OwnershipGeneration, StageId, SummaryId,
};
use crate::team::room::Room;
use crate::team::session::attachments::read_attachment;
use crate::team::session::dispatch::{dispatch, reserve_dispatches};
#[cfg(test)]
use crate::team::storage::atomic_write;
use crate::team::storage::records::digest;
use crate::team::storage::{RoomStore, StorageError};
use crate::team::tests::config;
use serde_json::Value;
use std::fs::File;
use std::io::Write as _;
use std::path::Path;
use std::{fs, mem};
use tempfile::tempdir;

#[test]
fn restart_retains_sources_scopes_controls_pending_work_and_budget() {
    let directory = tempdir().unwrap();
    let mut room = Room::new(AgentWorkspace::single(Some("C:/room".into())));
    let alice = room.add_member(config("Alice", "C:/frontend")).unwrap();
    let bob = room.add_member(config("Bob", "C:/backend")).unwrap();
    let id = room.id();
    let mut store = RoomStore::create(directory.path(), room.clone()).unwrap();

    let attachment = store
        .save_attachment("image/png", b"retained image bytes")
        .unwrap();

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
        attachments: vec![attachment.clone()],
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

    room.input_history.push(UserInput {
        text: "Review this".into(),
        references: vec![message],
        attachments: vec![attachment.clone()],
    });

    room.create_discussion(
        "Compare".into(),
        vec![alice, bob],
        DiscussionMode::Moderated { moderator: bob },
    )
    .unwrap();

    room.discussions[0].pause(PauseReason::User);

    let attempt = AttemptId::new();

    room.discussions[0]
        .budget
        .reserve(&[(attempt, TurnPurpose::Moderation)])
        .unwrap();

    room.discussions[0].budget.charge(attempt).unwrap();

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
    store.checkpoint().unwrap();
    room.rename_member(alice, "Alice frontend").unwrap();
    store.commit(room.clone()).unwrap();
    drop(store);

    let (store, truncated) = RoomStore::open(directory.path(), id).unwrap();

    assert!(!truncated);
    assert_eq!(store.room(), &room);
    assert_eq!(
        read_attachment(store.directory(), &attachment).unwrap(),
        b"retained image bytes"
    );
    assert!(RoomStore::open(directory.path(), id).is_err());

    let metadata = fs::read_to_string(store.directory.join("checkpoint.json")).unwrap();

    for forbidden_field in ["api_key", "api_base_url", "executable", "env"] {
        assert!(!metadata.contains(&format!("\"{forbidden_field}\"")));
    }
}

#[test]
fn incomplete_final_record_recovers_prefix_and_prior_corruption_blocks_only_that_room() {
    let directory = tempdir().unwrap();
    let mut room = Room::new(AgentWorkspace::default());
    let id = room.id();
    let mut store = RoomStore::create(directory.path(), room.clone()).unwrap();

    room.controls.automatic_summaries = false;
    store.commit(room.clone()).unwrap();
    store.journal.write_all(b"{\"payload\":").unwrap();
    store.journal.sync_all().unwrap();

    let journal_path = store.directory.join("journal.jsonl");

    drop(store);

    let (store, truncated) = RoomStore::open(directory.path(), id).unwrap();

    assert!(truncated);
    assert_eq!(store.room(), &room);

    drop(store);

    let mut bytes = fs::read(&journal_path).unwrap();

    bytes[0] = b'!';
    fs::write(&journal_path, bytes).unwrap();

    assert!(RoomStore::open(directory.path(), id).is_err());

    let other = Room::new(AgentWorkspace::default());
    let other_id = other.id();

    drop(RoomStore::create(directory.path(), other).unwrap());

    assert!(RoomStore::open(directory.path(), other_id).is_ok());
}

#[test]
fn unsupported_version_preserves_saved_bytes() {
    let directory = tempdir().unwrap();
    let room = Room::new(AgentWorkspace::default());
    let id = room.id();
    let store = RoomStore::create(directory.path(), room).unwrap();
    let path = store.directory.join("checkpoint.json");

    drop(store);

    let newer = fs::read_to_string(&path)
        .unwrap()
        .replacen("\"version\":1", "\"version\":9000", 1);

    fs::write(&path, &newer).unwrap();

    assert!(matches!(
        RoomStore::open(directory.path(), id),
        Err(StorageError::UnsupportedVersion(9000))
    ));
    assert_eq!(fs::read_to_string(path).unwrap(), newer);
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
        attachments: Vec::new(),
        backend_generation: 1,
        coverage: Default::default(),
        recipient: alice,
        ownership: OwnershipGeneration::default(),
        operation,
        stage: None,
        budget: BudgetScope::Direct(operation),
        purpose: TurnPurpose::Response,
        input: input.clone(),
        prepared_text: input.text.clone(),
        snapshot: PublicSnapshot::default(),
    };

    store.journal = File::open(store.directory.join("journal.jsonl")).unwrap();

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
    assert!(store.room.direct_allowances.is_empty());
    assert_eq!(input.text, "Keep this draft");

    drop(store);

    let (mut store, _) = RoomStore::open(directory.path(), room_id).unwrap();
    let ids = reserve_dispatches(&mut store, vec![intent.clone()]).unwrap();

    store.journal = File::open(store.directory.join("journal.jsonl")).unwrap();

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

    drop(store);

    let (mut store, _) = RoomStore::open(directory.path(), room_id).unwrap();

    assert_eq!(
        store.room.direct_allowances[&operation]
            .reservations()
            .len(),
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
        store.room.direct_allowances[&operation]
            .reservations()
            .len(),
        1
    );

    drop(store);

    let (mut store, _) = RoomStore::open(directory.path(), room_id).unwrap();

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
fn legacy_member_columns_survive_checkpoint_and_journal_replay() {
    let directory = tempdir().unwrap();
    let mut room = Room::new(AgentWorkspace::single(Some("C:/room".into())));
    let alice = room.add_member(config("Alice", "C:/frontend")).unwrap();
    let id = room.id();
    let mut store = RoomStore::create(directory.path(), room).unwrap();
    let mut next = store.room().clone();

    next.add_member(config("Bob", "C:/backend")).unwrap();
    store.commit(next).unwrap();
    drop(store);

    let room_directory = directory.path().join("agent-teams").join(id.to_string());
    let checkpoint = room_directory.join("checkpoint.json");
    let journal = room_directory.join("journal.jsonl");

    restore_legacy_member_columns(&checkpoint, "/payload/room/members");
    restore_legacy_member_columns(&journal, "/payload/changes/members");

    let (mut store, truncated) = RoomStore::open(directory.path(), id).unwrap();

    assert!(!truncated);
    assert_eq!(store.room().members().len(), 2);
    assert_eq!(store.room().member(alice).unwrap().name(), "Alice");

    let mut next = store.room().clone();

    next.members[0].role = "Updated after reopening".into();
    store.commit(next).unwrap();
    drop(store);

    let (store, truncated) = RoomStore::open(directory.path(), id).unwrap();

    assert!(!truncated);
    assert_eq!(store.room().members().len(), 2);
    assert_eq!(
        store.room().member(alice).unwrap().role(),
        "Updated after reopening"
    );

    drop(store);

    let mut stored: Vec<Value> = fs::read_to_string(&journal)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();

    stored[0]["payload"]["changes"]["members"][0]["conversation"] =
        Value::String("00000000-0000-4000-8000-000000000099".into());

    let modified: String = stored
        .iter()
        .map(|record| serde_json::to_string(record).unwrap() + "\n")
        .collect();

    fs::write(journal, modified).unwrap();

    assert!(matches!(
        RoomStore::open(directory.path(), id),
        Err(StorageError::Invalid("record checksum mismatch"))
    ));
}

fn restore_legacy_member_columns(path: &Path, pointer: &str) {
    let mut record: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();

    for member in record.pointer_mut(pointer).unwrap().as_array_mut().unwrap() {
        let object = member.as_object_mut().unwrap();
        let conversation = object["id"].clone();
        let fields = mem::take(object);

        for (key, value) in fields {
            if key == "conversation" {
                continue;
            }

            let profile = key == "profile";

            object.insert(key, value);

            if profile {
                object.insert("conversation".into(), conversation.clone());
            }
        }
    }

    record["digest"] = Value::String(digest(&serde_json::to_vec(&record["payload"]).unwrap()));

    let mut bytes = serde_json::to_vec(&record).unwrap();

    bytes.push(b'\n');
    fs::write(path, bytes).unwrap();
}

impl RoomStore {
    #[cfg(test)]
    pub(crate) fn save_attachment(
        &self,
        media_type: &str,
        bytes: &[u8],
    ) -> Result<AttachmentReference, StorageError> {
        if self.failed {
            return Err(StorageError::ReopenRequired);
        }

        if bytes.is_empty() {
            return Err(StorageError::Invalid("empty attachment"));
        }

        let directory = self.directory.join("attachments");

        fs::create_dir_all(&directory)?;

        let reference = AttachmentReference {
            id: AttachmentId::new(),
            media_type: media_type.into(),
            bytes: bytes.len() as u64,
            digest: digest(bytes),
        };

        atomic_write(&directory.join(reference.id.to_string()), bytes)?;

        Ok(reference)
    }
}
