mod store;
#[cfg(test)]
mod tests;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::{env, fs, io, thread};

use nmt_platform::filesystem::installation_path_spelling;
use parking_lot::Mutex;
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

    let normalized =
        fs::canonicalize(&absolute).unwrap_or_else(|_| normalize_path_components(&absolute));
    let spelling = installation_path_spelling(&normalized);

    // Windows keeps its persisted slash spelling. On Unix both case and a
    // backslash can distinguish directories, including after canonicalization.
    #[cfg(windows)]
    let spelling = spelling.replace('\\', "/");

    spelling
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
    writer: Option<HistoryWriter>,
}

impl AgentInputHistory {
    pub fn entries(&self, scope: &InputHistoryScope) -> Arc<[String]> {
        self.store.entries(scope).into()
    }

    pub fn record(&mut self, scope: &InputHistoryScope, text: String) -> bool {
        if !self.store.record(scope, text) {
            return false;
        }

        if let Some(writer) = self.writer.as_ref()
            && writer.save((&self.store).into()).is_err()
        {
            warn!("failed to queue Agent input history save");
        }

        true
    }

    pub fn flush(&self) -> io::Result<()> {
        let snapshot: StoredHistory = (&self.store).into();

        if let Some(writer) = self.writer.as_ref()
            && writer.flush(snapshot.clone()).is_ok()
        {
            return Ok(());
        }

        save_to_path(&self.path, &snapshot)
    }
}

impl AgentInputHistory {
    pub fn open(path: PathBuf) -> Self {
        let store = load_from_path(&path).unwrap_or_else(|error| {
            warn!("failed to load Agent input history: {error}");
            HistoryStore::default()
        });

        let writer = match HistoryWriter::spawn(path.clone()) {
            Ok(writer) => Some(writer),
            Err(error) => {
                warn!("failed to start Agent input history writer: {error}");
                None
            }
        };

        Self {
            store,
            path,
            writer,
        }
    }
}
struct HistoryWriter {
    sender: mpsc::SyncSender<()>,
    pending: Arc<Mutex<Option<PendingWrite>>>,
}

struct PendingWrite {
    snapshot: StoredHistory,
    waiters: Vec<mpsc::SyncSender<io::Result<()>>>,
}

impl HistoryWriter {
    fn spawn(path: PathBuf) -> io::Result<Self> {
        Self::start(move |snapshot| save_to_path(&path, snapshot))
    }

    fn start(
        save: impl FnMut(&StoredHistory) -> io::Result<()> + Send + 'static,
    ) -> io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel(1);
        let pending = Arc::new(Mutex::new(None));
        let worker_pending = Arc::clone(&pending);

        thread::Builder::new()
            .name("agent-input-history".to_string())
            .spawn(move || run_writer(receiver, worker_pending, save))?;

        Ok(Self { sender, pending })
    }

    fn save(&self, snapshot: StoredHistory) -> io::Result<()> {
        self.queue(snapshot, None)
    }

    fn flush(&self, snapshot: StoredHistory) -> io::Result<()> {
        let (sender, receiver) = mpsc::sync_channel(0);

        self.queue(snapshot, Some(sender))?;

        receiver
            .recv()
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "history writer stopped"))?
    }

    fn queue(
        &self,
        snapshot: StoredHistory,
        waiter: Option<mpsc::SyncSender<io::Result<()>>>,
    ) -> io::Result<()> {
        let mut pending = self.pending.lock();
        let mut waiters = pending
            .take()
            .map_or_else(Vec::new, |pending| pending.waiters);

        waiters.extend(waiter);
        *pending = Some(PendingWrite { snapshot, waiters });

        // Only the newest snapshot matters. The wake token carries no history,
        // so a slow disk cannot accumulate a queue of obsolete copies.
        match self.sender.try_send(()) {
            Ok(()) | Err(mpsc::TrySendError::Full(())) => Ok(()),
            Err(mpsc::TrySendError::Disconnected(())) => {
                pending.take();

                Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "history writer stopped",
                ))
            }
        }
    }
}

fn run_writer(
    receiver: mpsc::Receiver<()>,
    pending: Arc<Mutex<Option<PendingWrite>>>,
    mut save: impl FnMut(&StoredHistory) -> io::Result<()>,
) {
    while receiver.recv().is_ok() {
        let request = pending.lock().take();
        let Some(request) = request else { continue };
        let result = save(&request.snapshot);

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
    }
}
