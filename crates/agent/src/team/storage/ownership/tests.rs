use std::fs;

use tempfile::tempdir;

use crate::AgentWorkspace;
use crate::chat::ThreadSettings;
use crate::session::AgentKind;
use crate::team::identity::{ConversationId, MemberId, RoomId};
use crate::team::member::ProfileReference;
use crate::team::storage::ownership::{ConversationOwner, OwnershipRecord, OwnershipStore};

fn record() -> OwnershipRecord {
    OwnershipRecord::new(
        ConversationId::new(),
        "provider-conversation-1".into(),
        ProfileReference {
            kind: AgentKind::Codex,
            name: "Development".into(),
        },
        AgentWorkspace::single(Some("C:/frontend".into())),
        ThreadSettings {
            sandbox: Some("workspace-write".into()),
            ..ThreadSettings::default()
        },
        ConversationOwner::Independent,
    )
}

#[test]
fn crashes_before_and_after_commit_leave_exactly_one_authoritative_owner() {
    let directory = tempdir().unwrap();
    let record = record();
    let source = record.owner;
    let generation = record.generation;
    let roots = record.roots.clone();
    let provider_id = record.provider_id.clone();

    let target = ConversationOwner::Team {
        room: RoomId::new(),
        member: MemberId::new(),
    };

    let effective = ThreadSettings {
        sandbox: Some("read-only".into()),
        approval: Some("never".into()),
        ..ThreadSettings::default()
    };

    let mut store = OwnershipStore::create(directory.path(), record).unwrap();

    assert!(OwnershipStore::open(directory.path(), AgentKind::Codex, &provider_id).is_err());

    let ticket = store
        .prepare_transfer(source, generation, target, effective.clone())
        .unwrap();

    drop(store);

    let mut store = OwnershipStore::open(directory.path(), AgentKind::Codex, &provider_id).unwrap();

    assert!(store.authorizes(source, generation));
    assert!(!store.authorizes(target, ticket.generation()));
    assert_eq!(store.record().pending.as_ref(), Some(&ticket));

    store.commit_transfer(&ticket).unwrap();

    assert!(store.commit_transfer(&ticket).is_err());

    drop(store);

    let mut store = OwnershipStore::open(directory.path(), AgentKind::Codex, &provider_id).unwrap();

    assert!(!store.authorizes(source, generation));
    assert!(store.authorizes(target, ticket.generation()));
    assert_eq!(store.record().settings, effective);
    assert_eq!(store.record().roots, roots);

    let outgoing = store
        .prepare_transfer(
            target,
            ticket.generation(),
            ConversationOwner::Independent,
            effective.clone(),
        )
        .unwrap();

    store.commit_transfer(&outgoing).unwrap();

    assert!(!store.authorizes(target, ticket.generation()));
    assert!(store.authorizes(ConversationOwner::Independent, outgoing.generation()));
    assert_eq!(store.record().settings, effective);
    assert_eq!(store.record().roots, roots);
}

#[test]
fn failed_ownership_write_revokes_live_authority_until_reopening() {
    let directory = tempdir().unwrap();
    let record = record();
    let generation = record.generation;
    let mut store = OwnershipStore::create(directory.path(), record).unwrap();
    let path = store.directory.join("ownership.json");
    let saved = store.directory.join("ownership.saved");

    fs::rename(&path, &saved).unwrap();
    fs::create_dir(&path).unwrap();

    let target = ConversationOwner::Team {
        room: RoomId::new(),
        member: MemberId::new(),
    };

    assert!(
        store
            .prepare_transfer(
                ConversationOwner::Independent,
                generation,
                target,
                ThreadSettings::default()
            )
            .is_err()
    );
    assert!(!store.authorizes(ConversationOwner::Independent, generation));
    assert!(!store.authorizes(target, generation.next().unwrap()));

    drop(store);
    fs::remove_dir(&path).unwrap();
    fs::rename(&saved, &path).unwrap();

    let store = OwnershipStore::open(
        directory.path(),
        AgentKind::Codex,
        "provider-conversation-1",
    )
    .unwrap();

    assert!(store.authorizes(ConversationOwner::Independent, generation));
}
