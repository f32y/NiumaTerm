use std::fs::{self, File, FileTimes};
use std::time::{Duration, SystemTime};

use tempfile::tempdir;

use crate::AgentWorkspace;
use crate::team::model::{Author, MessageId, PublicMessage, Publication, RoomId};
use crate::team::room::Room;
use crate::team::storage::RoomStore;
use crate::team::storage::history::recent_rooms;
use crate::team::tests::config;

#[test]
fn history_reads_owned_rooms_without_changing_them_and_skips_empty_or_damaged_rooms() {
    let directory = tempdir().unwrap();

    let mut room = Room::new(AgentWorkspace::single(Some("C:/project".into())));

    room.add_member(config("Alice", "C:/project")).unwrap();

    room.messages.push(PublicMessage {
        id: MessageId::new(),
        author: Author::User,
        publication: Publication::UserInput,
        text: "Review the project\nMore details".into(),
        replies_to: Vec::new(),
    });

    let store = RoomStore::create(directory.path(), room).unwrap();

    let path = directory
        .path()
        .join("agent-teams")
        .join(store.room().id().to_string())
        .join("room.json");

    let before = fs::read(&path).unwrap();
    let _empty = RoomStore::create(directory.path(), Room::new(AgentWorkspace::default())).unwrap();

    let damaged = directory
        .path()
        .join("agent-teams")
        .join(RoomId::new().to_string());

    fs::create_dir(&damaged).unwrap();
    fs::write(damaged.join("room.json"), "invalid").unwrap();

    let summaries = recent_rooms(directory.path()).unwrap();

    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].id, store.room().id());
    assert_eq!(summaries[0].title, "Review the project");
    assert_eq!(summaries[0].cwd.as_deref(), Some("C:/project"));
    assert_eq!(fs::read(path).unwrap(), before);
}

#[test]
fn history_orders_by_saved_time_and_uses_members_before_the_first_prompt() {
    let directory = tempdir().unwrap();

    let mut ids = Vec::new();

    for (name, seconds) in [("Older", 1), ("Newer", 2)] {
        let mut room = Room::new(AgentWorkspace::default());

        room.add_member(config(name, "C:/project")).unwrap();

        let store = RoomStore::create(directory.path(), room).unwrap();

        ids.push(store.room().id());

        let path = directory
            .path()
            .join("agent-teams")
            .join(store.room().id().to_string())
            .join("room.json");

        File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_times(
                FileTimes::new()
                    .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)),
            )
            .unwrap();
    }

    let summaries = recent_rooms(directory.path()).unwrap();

    assert_eq!(
        summaries
            .iter()
            .map(|summary| summary.id)
            .collect::<Vec<_>>(),
        vec![ids[1], ids[0]]
    );
    assert_eq!(summaries[0].title, "Newer");
}
