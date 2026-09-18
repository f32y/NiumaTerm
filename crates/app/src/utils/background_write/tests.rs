use std::fs;

use gpui::TestAppContext;
use tempfile::tempdir;

use crate::utils::{background_write, background_write_reply};

#[gpui::test]
async fn accepted_writes_keep_their_order_when_the_caller_drops_its_reply(cx: &mut TestAppContext) {
    let directory = tempdir().unwrap();
    let path = directory.path().join("settings");

    fs::write(&path, "original").unwrap();

    let saved = cx.update(|cx| {
        let first_path = path.clone();

        background_write(cx, move || fs::write(first_path, "first").unwrap());

        let second_path = path.clone();

        let saved = background_write_reply(cx, move || {
            let previous = fs::read_to_string(&second_path).unwrap();

            fs::write(second_path, "second").unwrap();

            previous
        });

        assert_eq!(fs::read_to_string(&path).unwrap(), "original");

        saved
    });

    assert_eq!(saved.await, "first");
    assert_eq!(fs::read_to_string(&path).unwrap(), "second");
}
