mod store;

#[cfg(test)]
mod tests;

use std::ops::Range;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::{env, fs, io, process, thread};

use gpui::{App, Context, Entity, Global, Window};
use gpui_component::input::InputState;
use nmt_agent_utils::AgentWorkspace;
use tracing::warn;

use crate::agent::input_history::store::{
    HistoryStore, StoredHistory, load_from_path, save_to_path,
};
use crate::agent::{AgentKind, AgentPane};

const LOCAL_TARGET: &str = "local";

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct InputHistoryScope {
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
    pub(crate) fn local(kind: AgentKind, workspace: &AgentWorkspace) -> Self {
        Self::new(LOCAL_TARGET, kind, workspace)
    }

    fn new(target: impl Into<String>, kind: AgentKind, workspace: &AgentWorkspace) -> Self {
        Self {
            target: target.into(),
            backend: kind.id().to_string(),
            cwd: normalize_working_directory(workspace.primary()),
            additional: workspace.history_signature(),
        }
    }
}

fn normalize_working_directory(cwd: Option<&str>) -> String {
    let supplied = cwd
        .filter(|cwd| !cwd.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let absolute = if supplied.is_absolute() {
        supplied
    } else {
        env::current_dir()
            .map(|current| current.join(&supplied))
            .unwrap_or(supplied)
    };
    let normalized =
        fs::canonicalize(&absolute).unwrap_or_else(|_| normalize_path_components(&absolute));
    let mut normalized = normalized.to_string_lossy().replace('\\', "/");
    normalized.make_ascii_lowercase();
    normalized
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

pub(crate) struct AgentInputHistory {
    store: HistoryStore,
    path: PathBuf,
    writer: Option<HistoryWriter>,
}

impl Global for AgentInputHistory {}

impl AgentInputHistory {
    pub(crate) fn entries(&self, scope: &InputHistoryScope) -> Arc<[String]> {
        Arc::from(self.store.entries(scope))
    }

    pub(crate) fn record(&mut self, scope: &InputHistoryScope, text: String) -> bool {
        if !self.store.record(scope, text) {
            return false;
        }

        if let Some(writer) = self.writer.as_ref()
            && writer.save(self.store.snapshot()).is_err()
        {
            warn!("failed to queue Agent input history save");
        }
        true
    }

    fn flush(&self) -> io::Result<()> {
        let snapshot = self.store.snapshot();
        if let Some(writer) = self.writer.as_ref()
            && writer.flush(snapshot.clone()).is_ok()
        {
            return Ok(());
        }
        save_to_path(&self.path, &snapshot)
    }
}

pub(crate) fn initialize(testing: bool, cx: &mut App) {
    let path = history_file_path(testing);
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
    cx.set_global(AgentInputHistory {
        store,
        path,
        writer,
    });
}

pub(crate) fn flush(cx: &App) -> io::Result<()> {
    cx.global::<AgentInputHistory>().flush()
}

fn history_file_path(testing: bool) -> PathBuf {
    if testing {
        env::temp_dir()
            .join("NiumaTerm")
            .join(format!("input-history-testing-{}", process::id()))
            .join("agent-input-history.json")
    } else {
        nmt_config::config_dir_path().join("agent-input-history.json")
    }
}

struct HistoryWriter {
    sender: mpsc::Sender<WriteRequest>,
}

enum WriteRequest {
    Save(StoredHistory),
    Flush(StoredHistory, mpsc::SyncSender<io::Result<()>>),
}

impl HistoryWriter {
    fn spawn(path: PathBuf) -> io::Result<Self> {
        let (sender, receiver) = mpsc::channel();
        thread::Builder::new()
            .name("agent-input-history".to_string())
            .spawn(move || run_writer(path, receiver))?;
        Ok(Self { sender })
    }

    fn save(&self, snapshot: StoredHistory) -> Result<(), mpsc::SendError<WriteRequest>> {
        self.sender.send(WriteRequest::Save(snapshot))
    }

    fn flush(&self, snapshot: StoredHistory) -> io::Result<()> {
        let (sender, receiver) = mpsc::sync_channel(0);
        self.sender
            .send(WriteRequest::Flush(snapshot, sender))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "history writer stopped"))?;
        receiver
            .recv()
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "history writer stopped"))?
    }
}

fn run_writer(path: PathBuf, receiver: mpsc::Receiver<WriteRequest>) {
    while let Ok(request) = receiver.recv() {
        match request {
            WriteRequest::Save(snapshot) => {
                if let Err(error) = save_to_path(&path, &snapshot) {
                    warn!("failed to save Agent input history: {error}");
                }
            }
            WriteRequest::Flush(snapshot, sender) => {
                let _ = sender.send(save_to_path(&path, &snapshot));
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::agent) enum InputHistoryDirection {
    Older,
    Newer,
}

#[derive(Debug, PartialEq, Eq)]
enum InputHistoryAction {
    Declined,
    Keep,
    Replace(String),
    Clear,
}

#[derive(Default)]
pub(in crate::agent) struct InputHistoryNavigation {
    entries: Arc<[String]>,
    index: Option<usize>,
}

impl InputHistoryNavigation {
    pub(in crate::agent) fn reset(&mut self) {
        self.entries = Arc::from([]);
        self.index = None;
    }

    fn navigate(
        &mut self,
        direction: InputHistoryDirection,
        text: &str,
        selection: Range<usize>,
        cursor: usize,
        available: Arc<[String]>,
    ) -> InputHistoryAction {
        if let Some(index) = self.index {
            let Some(recalled) = self.entries.get(index) else {
                self.reset();
                return InputHistoryAction::Declined;
            };
            if text != recalled {
                self.reset();
            } else {
                if !selection.is_empty() || (cursor != 0 && cursor != text.len()) {
                    return InputHistoryAction::Declined;
                }
                return match direction {
                    InputHistoryDirection::Older if index > 0 => {
                        let index = index - 1;
                        self.index = Some(index);
                        InputHistoryAction::Replace(self.entries[index].clone())
                    }
                    InputHistoryDirection::Older => InputHistoryAction::Keep,
                    InputHistoryDirection::Newer if index + 1 < self.entries.len() => {
                        let index = index + 1;
                        self.index = Some(index);
                        InputHistoryAction::Replace(self.entries[index].clone())
                    }
                    InputHistoryDirection::Newer => {
                        self.reset();
                        InputHistoryAction::Clear
                    }
                };
            }
        }

        if direction == InputHistoryDirection::Newer || !text.is_empty() || !selection.is_empty() {
            return InputHistoryAction::Declined;
        }
        let Some(index) = available.len().checked_sub(1) else {
            return InputHistoryAction::Declined;
        };
        let text = available[index].clone();
        self.entries = available;
        self.index = Some(index);
        InputHistoryAction::Replace(text)
    }
}

impl AgentPane {
    pub(in crate::agent) fn handle_input_history_control(
        &mut self,
        direction: InputHistoryDirection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let (text, selection, cursor) = {
            let input = self.input.read(cx);
            (
                input.text().to_string(),
                input.selected_range(),
                input.cursor(),
            )
        };
        let available = cx
            .global::<AgentInputHistory>()
            .entries(&self.input_history_scope);
        let action = self
            .input_history_navigation
            .navigate(direction, &text, selection, cursor, available);

        match action {
            InputHistoryAction::Declined => false,
            InputHistoryAction::Keep => {
                cx.stop_propagation();
                true
            }
            InputHistoryAction::Replace(text) => {
                self.palette.skill_binding = None;
                self.palette.dismissed = true;
                self.palette.selected = 0;
                replace_input_with_history(&self.input, text, window, cx);
                cx.stop_propagation();
                cx.notify();
                true
            }
            InputHistoryAction::Clear => {
                self.input
                    .update(cx, |input, cx| input.set_value("", window, cx));
                cx.stop_propagation();
                cx.notify();
                true
            }
        }
    }

    pub(super) fn record_input_history(&mut self, text: &str, cx: &mut Context<Self>) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        cx.global_mut::<AgentInputHistory>()
            .record(&self.input_history_scope, text.to_string());
        self.input_history_navigation.reset();
    }
}

fn replace_input_with_history<T: 'static>(
    input: &Entity<InputState>,
    text: String,
    window: &mut Window,
    cx: &mut Context<T>,
) {
    let end = text.len();
    input.update(cx, |input, cx| {
        input.set_value(text, window, cx);
        input.set_selected_range(end..end, cx);
    });
}
