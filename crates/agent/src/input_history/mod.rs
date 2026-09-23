mod store;

#[cfg(test)]
mod tests;

use std::future::Future;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::{env, io};

use nmt_platform::filesystem::history_path_spelling;
use parking_lot::Mutex;
use tokio::sync::{Notify, oneshot};
use tracing::warn;

use crate::AgentWorkspace;
use crate::input_history::store::{HistoryStore, StoredHistory, load_from_path, save_to_path};
use crate::session::AgentKind;

const LOCAL_TARGET: &str = "local";

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct InputHistoryScope {
    target: String,
    backend: String,
    cwd: String,

    /// Signature of the additional workspace directories, empty for a
    /// single-directory workspace. Two workspaces that share a primary
    /// directory but attach different ones are different working contexts, so
    /// their prompt histories stay apart; keeping the field empty when there
    /// are no additions is what leaves every existing history entry reachable.
    additional: String,
}

impl InputHistoryScope {
    pub fn local(kind: AgentKind, workspace: &AgentWorkspace) -> Self {
        Self::new(LOCAL_TARGET, kind, workspace)
    }

    fn new(target: impl Into<String>, kind: AgentKind, workspace: &AgentWorkspace) -> Self {
        let backend: &str = kind.into();

        Self {
            target: target.into(),
            backend: backend.into(),
            cwd: normalize_working_directory(workspace.primary()),
            additional: workspace.history_signature(),
        }
    }
}

fn normalize_working_directory(cwd: Option<&str>) -> String {
    let supplied = cwd
        .filter(|cwd| !cwd.trim().is_empty())
        .map(Into::into)
        .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| ".".into()));

    let absolute = if supplied.is_absolute() {
        supplied
    } else {
        env::current_dir()
            .map(|current| current.join(&supplied))
            .unwrap_or(supplied)
    };

    // `dunce` drops the verbatim `\\?\` prefix Windows canonicalization adds,
    // so a directory keys the same whether it exists now or the lexical
    // fallback has to name it.
    let normalized =
        dunce::canonicalize(&absolute).unwrap_or_else(|_| normalize_path_components(&absolute));

    history_path_spelling(&normalized)
}

/// The key a stored directory spelling has today. Histories saved before the
/// verbatim prefix was dropped keyed existing directories as `//?/c:/...`
/// (or `//?/unc/server/...` for a share), which no longer matches the key
/// the same directory produces now.
pub(super) fn current_cwd_key(cwd: &str) -> String {
    if let Some(share) = cwd.strip_prefix("//?/unc/") {
        return format!("//{share}");
    }

    cwd.strip_prefix("//?/").unwrap_or(cwd).to_string()
}

fn normalize_path_components(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();

    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            component => normalized.push(component.as_os_str()),
        }
    }

    normalized
}

pub struct AgentInputHistory {
    store: HistoryStore,
    path: PathBuf,
    writer: HistoryWriter,
}

impl AgentInputHistory {
    pub fn entries(&self, scope: &InputHistoryScope) -> Arc<[String]> {
        self.store.entries(scope).into()
    }

    pub fn record(&mut self, scope: &InputHistoryScope, text: String) -> bool {
        if !self.store.record(scope, text) {
            return false;
        }

        if self.writer.save((&self.store).into()).is_err() {
            warn!("failed to queue Agent input history save");
        }

        true
    }

    /// Complete once the current history has reached storage. The returned
    /// future owns what it needs, so an exit hook can await it after the
    /// caller's borrow ends.
    pub fn flush(&self) -> impl Future<Output = io::Result<()>> + Send + use<> {
        let snapshot: StoredHistory = (&self.store).into();
        let written = self.writer.flush(snapshot.clone());
        let path = self.path.clone();

        async move {
            if let Ok(written) = written
                && matches!(written.await, Ok(Ok(())))
            {
                return Ok(());
            }

            // The writer could not confirm the save, so this one writes it
            // directly and reports that outcome instead.
            nmt_runtime::handle()
                .spawn_blocking(move || save_to_path(&path, &snapshot))
                .await
                .unwrap_or_else(|error| Err(io::Error::other(error)))
        }
    }

    pub fn open(path: PathBuf) -> Self {
        let store = load_from_path(&path).unwrap_or_else(|error| {
            warn!("failed to load Agent input history: {error}");

            HistoryStore::default()
        });

        let writer = HistoryWriter::spawn(path.clone());

        Self {
            store,
            path,
            writer,
        }
    }
}

/// Serialized history saves on the shared runtime. Only the newest snapshot
/// waits, so a slow disk cannot accumulate a queue of obsolete copies; each
/// save is one blocking batch that keeps merge, write, and replace together.
struct HistoryWriter {
    shared: Arc<WriterState>,
}

struct WriterState {
    pending: Mutex<Option<PendingWrite>>,

    /// Stores one permit while the writer is busy, so every queued snapshot
    /// is seen without a wake per save.
    wake: Notify,

    closed: AtomicBool,

    /// Set when the writer task ends, including by a panicking save.
    stopped: AtomicBool,
}

struct PendingWrite {
    snapshot: StoredHistory,
    waiters: Vec<oneshot::Sender<io::Result<()>>>,
}

impl HistoryWriter {
    fn spawn(path: PathBuf) -> Self {
        Self::start(move |snapshot| save_to_path(&path, snapshot))
    }

    fn start(save: impl FnMut(&StoredHistory) -> io::Result<()> + Send + 'static) -> Self {
        let shared = Arc::new(WriterState {
            pending: Mutex::new(None),
            wake: Notify::new(),
            closed: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
        });

        nmt_runtime::handle().spawn(run_writer(Arc::clone(&shared), save));

        Self { shared }
    }

    fn save(&self, snapshot: StoredHistory) -> io::Result<()> {
        self.queue(snapshot, None)
    }

    /// Queue `snapshot`, answering once it or a newer one reached storage.
    fn flush(&self, snapshot: StoredHistory) -> io::Result<oneshot::Receiver<io::Result<()>>> {
        let (sender, receiver) = oneshot::channel();

        self.queue(snapshot, Some(sender))?;

        Ok(receiver)
    }

    fn queue(
        &self,
        snapshot: StoredHistory,
        waiter: Option<oneshot::Sender<io::Result<()>>>,
    ) -> io::Result<()> {
        if self.shared.stopped.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "history writer stopped",
            ));
        }

        let mut pending = self.shared.pending.lock();

        let mut waiters = pending
            .take()
            .map_or_else(Vec::new, |pending| pending.waiters);

        waiters.extend(waiter);

        *pending = Some(PendingWrite { snapshot, waiters });

        drop(pending);

        self.shared.wake.notify_one();

        Ok(())
    }
}

impl Drop for HistoryWriter {
    fn drop(&mut self) {
        // A write still pending is finished before the task ends.
        self.shared.closed.store(true, Ordering::Release);

        self.shared.wake.notify_one();
    }
}

async fn run_writer(
    shared: Arc<WriterState>,
    mut save: impl FnMut(&StoredHistory) -> io::Result<()> + Send + 'static,
) {
    struct Stopped(Arc<WriterState>);

    impl Drop for Stopped {
        fn drop(&mut self) {
            self.0.stopped.store(true, Ordering::Release);
        }
    }

    let _stopped = Stopped(Arc::clone(&shared));

    loop {
        shared.wake.notified().await;

        let request = shared.pending.lock().take();

        let Some(request) = request else {
            if shared.closed.load(Ordering::Acquire) {
                return;
            }

            continue;
        };

        let Ok((returned, result, request)) = nmt_runtime::handle()
            .spawn_blocking(move || {
                let result = save(&request.snapshot);

                (save, result, request)
            })
            .await
        else {
            return;
        };

        save = returned;

        if let Err(error) = &result {
            warn!("failed to save Agent input history: {error}");
        }

        // A flush completes only after its snapshot, or a newer replacement,
        // has reached storage. Waiters arriving during I/O join the next write.
        for sender in request.waiters {
            let result = result
                .as_ref()
                .copied()
                .map_err(|error| io::Error::new(error.kind(), error.to_string()));

            let _ = sender.send(result);
        }

        if shared.closed.load(Ordering::Acquire) && shared.pending.lock().is_none() {
            return;
        }
    }
}
