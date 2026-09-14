use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::Duration;
use std::{env, fs, process};

use nmt_platform::process::hidden_command;
use serde_json::{Value, json};

use crate::AgentWorkspace;
use crate::input_history::store::{HistoryStore, StoredHistory, load_from_path, save_to_path};
use crate::input_history::{AgentInputHistory, HistoryWriter, InputHistoryScope};
use crate::session::AgentKind;

static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(1);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let path = env::temp_dir().join(format!(
            "niumaterm-input-history-test-{}-{}",
            process::id(),
            NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));

        fs::create_dir_all(&path).expect("create test directory");

        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn scope(target: &str, kind: AgentKind, cwd: &Path) -> InputHistoryScope {
    InputHistoryScope::new(
        target,
        kind,
        &AgentWorkspace::single(cwd.to_str().map(str::to_string)),
    )
}

/// A scope for a workspace whose primary directory is `cwd` and which also
/// owns `additional`.
fn multi_root_scope(kind: AgentKind, cwd: &Path, additional: &[&Path]) -> InputHistoryScope {
    InputHistoryScope::new(
        "local",
        kind,
        &AgentWorkspace::new(
            cwd.to_str().map(str::to_string),
            additional
                .iter()
                .filter_map(|path| path.to_str().map(str::to_string))
                .collect(),
        ),
    )
}

#[test]
fn scope_uses_target_backend_and_normalized_directory() {
    let directory = TestDirectory::new();
    let first_cwd = directory.path().join("first");
    let second_cwd = directory.path().join("second");

    fs::create_dir_all(&first_cwd).expect("create first directory");

    fs::create_dir_all(&second_cwd).expect("create second directory");

    let local_codex = scope("local", AgentKind::Codex, &first_cwd);
    let equivalent = scope("local", AgentKind::Codex, &first_cwd.join("."));
    let local_claude = scope("local", AgentKind::Claude, &first_cwd);
    let remote_codex = scope("remote-a", AgentKind::Codex, &first_cwd);
    let other_cwd = scope("local", AgentKind::Codex, &second_cwd);

    assert_eq!(local_codex, equivalent);
    assert_ne!(local_codex, local_claude);
    assert_ne!(local_codex, remote_codex);
    assert_ne!(local_codex, other_cwd);

    let mut history = HistoryStore::default();

    history.record(&local_codex, "local codex".into());

    history.record(&local_claude, "local claude".into());

    history.record(&remote_codex, "remote codex".into());

    history.record(&other_cwd, "other directory".into());

    assert_eq!(history.entries(&local_codex), ["local codex"]);
    assert_eq!(history.entries(&local_claude), ["local claude"]);
    assert_eq!(history.entries(&remote_codex), ["remote codex"]);
    assert_eq!(history.entries(&other_cwd), ["other directory"]);
}

#[test]
fn history_directory_keys_follow_native_spelling() {
    let directory = TestDirectory::new();
    let upper = directory.path().join("Project");
    let lower = directory.path().join("project");
    let upper_scope = scope("local", AgentKind::Codex, &upper);
    let lower_scope = scope("local", AgentKind::Codex, &lower);

    let mut history = HistoryStore::default();

    history.record(&upper_scope, "upper directory".into());

    history.record(&lower_scope, "lower directory".into());

    let path = directory.path().join("history.json");

    save_to_path(&path, &(&history).into()).expect("save scoped history");

    let restored = load_from_path(&path).expect("restore scoped history");

    #[cfg(windows)]
    {
        assert_eq!(upper_scope, lower_scope);
        assert!(!upper_scope.cwd.contains('\\'));
        assert_eq!(upper_scope.cwd, upper_scope.cwd.to_ascii_lowercase());
        assert_eq!(
            restored.entries(&upper_scope),
            ["upper directory", "lower directory"]
        );
    }

    #[cfg(unix)]
    {
        assert_ne!(upper_scope, lower_scope);
        assert_eq!(restored.entries(&upper_scope), ["upper directory"]);
        assert_eq!(restored.entries(&lower_scope), ["lower directory"]);
        assert_ne!(
            scope("local", AgentKind::Codex, &directory.path().join(r"a\b")),
            scope("local", AgentKind::Codex, &directory.path().join("a/b")),
        );
    }
}

#[test]
fn legacy_migration_and_stale_saves_preserve_distinct_records() {
    let directory = TestDirectory::new();
    let path = directory.path().join("history.json");
    let scope = scope("local", AgentKind::Codex, directory.path());

    let legacy = json!({"version":1,"scopes":[{
        "target":scope.target,"backend":scope.backend,"cwd":scope.cwd,
        "entries":["first","second","first"]
    }]});

    fs::write(&path, legacy.to_string()).unwrap();

    let mut first = load_from_path(&path).unwrap();
    let mut second = load_from_path(&path).unwrap();

    first.record(&scope, "from first".into());

    second.record(&scope, "from second".into());

    save_to_path(&path, &(&first).into()).unwrap();

    save_to_path(&path, &(&second).into()).unwrap();

    save_to_path(&path, &(&first).into()).unwrap();

    let entries = load_from_path(&path).unwrap().entries(&scope);

    assert_eq!(&entries[..3], ["first", "second", "first"]);
    assert_eq!(entries.len(), 5);
    assert!(entries.contains(&"from first".to_string()));
    assert!(entries.contains(&"from second".to_string()));
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(&path).unwrap()).unwrap()["version"],
        2
    );
}

#[test]
fn stale_saves_do_not_revive_expired_entries_or_overwrite_invalid_files() {
    let directory = TestDirectory::new();
    let path = directory.path().join("history.json");
    let scope = scope("local", AgentKind::Codex, directory.path());

    let mut store = HistoryStore::default();

    store.record(&scope, "expired".into());

    let stale: StoredHistory = (&store).into();

    for index in 0..100 {
        store.record(&scope, format!("new-{index}"));
    }

    save_to_path(&path, &(&store).into()).unwrap();

    save_to_path(&path, &stale).unwrap();

    let entries = load_from_path(&path).unwrap().entries(&scope);

    assert_eq!(entries, store.entries(&scope));

    fs::write(&path, b"invalid history").unwrap();

    assert!(save_to_path(&path, &(&store).into()).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"invalid history");
}

#[test]
fn history_process_writer() {
    let Some(path) = env::var_os("NMT_HISTORY_TEST_PATH") else {
        return;
    };

    let path: PathBuf = path.into();
    let writer = env::var("NMT_HISTORY_TEST_WRITER").unwrap();
    let scope = scope("local", AgentKind::Codex, path.parent().unwrap());

    let mut history = HistoryStore::default();

    for index in 0..15 {
        history.record(&scope, format!("writer-{writer}-{index}"));

        save_to_path(&path, &(&history).into()).unwrap();
    }
}

#[test]
fn concurrent_processes_merge_history_without_losing_entries() {
    let directory = TestDirectory::new();
    let path = directory.path().join("history.json");
    let executable = env::current_exe().unwrap();

    let mut children = Vec::new();

    for writer in 0..3 {
        children.push(
            hidden_command(&executable)
                .args(["--exact", "input_history::tests::history_process_writer"])
                .env("NMT_HISTORY_TEST_PATH", &path)
                .env("NMT_HISTORY_TEST_WRITER", writer.to_string())
                .spawn()
                .unwrap(),
        );
    }

    for mut child in children {
        assert!(child.wait().unwrap().success());
    }

    let scope = scope("local", AgentKind::Codex, directory.path());
    let entries = load_from_path(&path).unwrap().entries(&scope);

    assert_eq!(entries.len(), 45);

    for writer in 0..3 {
        for index in 0..15 {
            assert!(entries.contains(&format!("writer-{writer}-{index}")));
        }
    }

    assert_eq!(
        fs::read_dir(directory.path()).unwrap().count(),
        2,
        "only the history and persistent lock should remain"
    );
}

#[test]
fn recording_collapses_neighbors_and_keeps_the_newest_hundred() {
    let directory = TestDirectory::new();
    let codex_scope = scope("local", AgentKind::Codex, directory.path());

    let mut history = HistoryStore::default();

    assert!(history.record(&codex_scope, "first".into()));
    assert!(!history.record(&codex_scope, "first".into()));
    assert!(history.record(&codex_scope, "second".into()));
    assert!(history.record(&codex_scope, "first".into()));
    assert_eq!(history.entries(&codex_scope), ["first", "second", "first"]);

    let limited = scope("remote-a", AgentKind::Claude, directory.path());

    for index in 0..=100 {
        assert!(history.record(&limited, format!("entry-{index}")));
    }

    let entries = history.entries(&limited);

    assert_eq!(entries.len(), 100);
    assert_eq!(entries.first().map(String::as_str), Some("entry-1"));
    assert_eq!(entries.last().map(String::as_str), Some("entry-100"));
}

#[test]
fn json_round_trip_preserves_scoped_entries() {
    let directory = TestDirectory::new();
    let path = directory.path().join("agent-input-history.json");
    let codex = scope("local", AgentKind::Codex, directory.path());
    let claude = scope("local", AgentKind::Claude, directory.path());

    let mut history = HistoryStore::default();

    history.record(&codex, "line one\nline two".into());

    history.record(&claude, "/status".into());

    save_to_path(&path, &(&history).into()).expect("save history");

    let restored = load_from_path(&path).expect("load history");

    assert_eq!(restored.entries(&codex), ["line one\nline two"]);
    assert_eq!(restored.entries(&claude), ["/status"]);
}

#[test]
fn missing_json_is_empty_and_invalid_json_is_reported() {
    let directory = TestDirectory::new();
    let missing = directory.path().join("missing.json");

    assert!(
        load_from_path(&missing)
            .expect("load missing history")
            .entries(&scope("local", AgentKind::Codex, directory.path()))
            .is_empty()
    );

    let invalid = directory.path().join("invalid.json");

    fs::write(&invalid, b"not json").expect("write invalid history");

    let error = load_from_path(&invalid).expect_err("invalid history must fail");

    assert_eq!(error.kind(), ErrorKind::InvalidData);
}

#[test]
fn failed_save_leaves_the_in_memory_entry_available() {
    let directory = TestDirectory::new();
    let blocker = directory.path().join("not-a-directory");

    fs::write(&blocker, b"blocked").expect("write blocker");

    let path = blocker.join("agent-input-history.json");
    let scope = scope("local", AgentKind::Codex, directory.path());

    let mut history = HistoryStore::default();

    history.record(&scope, "still available".into());

    assert!(save_to_path(&path, &(&history).into()).is_err());
    assert_eq!(history.entries(&scope), ["still available"]);
}

#[test]
fn slow_storage_coalesces_saves_and_flush_waits_for_latest_write() {
    let directory = TestDirectory::new();
    let path = directory.path().join("history.json");
    let saved_path = path.clone();
    let scope = scope("local", AgentKind::Codex, directory.path());
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();

    let writer = HistoryWriter::start(move |snapshot| {
        started_tx.send(()).unwrap();

        release_rx.recv().unwrap();

        save_to_path(&saved_path, snapshot)
    })
    .unwrap();

    let mut store = HistoryStore::default();

    store.record(&scope, "first".into());

    writer.save((&store).into()).unwrap();

    started_rx.recv_timeout(Duration::from_secs(5)).unwrap();

    for index in 0..50 {
        store.record(&scope, format!("entry {index}"));

        writer.save((&store).into()).unwrap();
    }

    let (flushed_tx, flushed_rx) = mpsc::sync_channel(0);

    writer.queue((&store).into(), Some(flushed_tx)).unwrap();

    assert!(flushed_rx.try_recv().is_err());

    release_tx.send(()).unwrap();

    started_rx.recv_timeout(Duration::from_secs(5)).unwrap();

    assert!(flushed_rx.try_recv().is_err());

    release_tx.send(()).unwrap();

    flushed_rx
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();

    assert_eq!(
        load_from_path(&path).unwrap().entries(&scope),
        store.entries(&scope)
    );

    drop(writer);

    assert!(matches!(
        started_rx.recv_timeout(Duration::from_secs(5)),
        Err(mpsc::RecvTimeoutError::Disconnected)
    ));
}

#[test]
fn snapshots_keep_entries_from_before_later_edits() {
    let directory = TestDirectory::new();
    let path = directory.path().join("history.json");
    let scope = scope("local", AgentKind::Codex, directory.path());

    let mut store = HistoryStore::default();

    store.record(&scope, "original".into());

    let snapshot: StoredHistory = (&store).into();

    store.record(&scope, "later".into());

    save_to_path(&path, &snapshot).unwrap();

    assert_eq!(load_from_path(&path).unwrap().entries(&scope), ["original"]);
    assert_eq!(store.entries(&scope), ["original", "later"]);
}

#[test]
fn background_writer_flushes_the_latest_snapshot_in_order() {
    let directory = TestDirectory::new();
    let path = directory.path().join("agent-input-history.json");
    let scope = scope("local", AgentKind::Codex, directory.path());
    let writer = HistoryWriter::spawn(path.clone()).expect("start history writer");

    let mut history = AgentInputHistory {
        store: HistoryStore::default(),
        path: path.clone(),
        writer: Some(writer),
    };

    history.record(&scope, "first".into());

    history.record(&scope, "second".into());

    history.flush().expect("flush history");

    assert_eq!(
        load_from_path(&path).expect("load history").entries(&scope),
        ["first", "second"]
    );
}

#[test]
fn service_keeps_entries_when_background_writes_fail() {
    let directory = TestDirectory::new();
    let blocker = directory.path().join("not-a-directory");

    fs::write(&blocker, b"blocked").expect("write blocker");

    let path = blocker.join("agent-input-history.json");
    let scope = scope("local", AgentKind::Codex, directory.path());
    let writer = HistoryWriter::spawn(path.clone()).expect("start history writer");

    let mut history = AgentInputHistory {
        store: HistoryStore::default(),
        path,
        writer: Some(writer),
    };

    history.record(&scope, "still available".into());

    assert!(history.flush().is_err());
    assert_eq!(&*history.entries(&scope), ["still available"]);
}

#[test]
fn workspaces_sharing_a_primary_directory_keep_separate_histories() {
    let directory = TestDirectory::new();
    let primary = directory.path().join("api");
    let web = directory.path().join("web");
    let docs = directory.path().join("docs");

    for path in [&primary, &web, &docs] {
        fs::create_dir_all(path).expect("create directory");
    }

    let alone = multi_root_scope(AgentKind::Codex, &primary, &[]);
    let with_web = multi_root_scope(AgentKind::Codex, &primary, &[&web]);
    let with_docs = multi_root_scope(AgentKind::Codex, &primary, &[&docs]);
    let both = multi_root_scope(AgentKind::Codex, &primary, &[&web, &docs]);
    let reordered = multi_root_scope(AgentKind::Codex, &primary, &[&docs, &web]);

    // Attaching a directory changes the working context, so the prompts
    // recorded in one root set do not surface in another.
    assert_ne!(alone, with_web);
    assert_ne!(with_web, with_docs);
    assert_ne!(both, reordered);

    // An equivalent spelling of the same ordered directories is the same
    // scope, so history survives a path written with other separators.
    let equivalent = multi_root_scope(AgentKind::Codex, &primary, &[&web.join(".")]);

    assert_eq!(with_web, equivalent);

    // A single-directory workspace still resolves to the scope that predates
    // multi-directory workspaces, which is what keeps its history reachable.
    assert_eq!(alone, scope("local", AgentKind::Codex, &primary));

    let mut history = HistoryStore::default();

    history.record(&alone, "alone".into());

    history.record(&with_web, "with web".into());

    history.record(&both, "both".into());

    assert_eq!(history.entries(&alone), ["alone"]);
    assert_eq!(history.entries(&with_web), ["with web"]);
    assert_eq!(history.entries(&both), ["both"]);
}
