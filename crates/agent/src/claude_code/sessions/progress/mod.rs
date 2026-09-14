pub(crate) use crate::claude_code::sessions::progress::tracker::ProgressSnapshot;

mod tracker;

#[cfg(test)]
mod tests;

use std::fs::File;
use std::io::{self, BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};

use crate::claude_code::sessions::paths::session_path;
use crate::claude_code::sessions::progress::tracker::ProgressTracker;

pub(crate) const PROGRESS_METHOD: &str = "nmt/claudeProgress";

/// The CLI omits goal attachments from SDK output. Read its append-only log on
/// a worker so goal evaluations and restored checklists never block rendering.
pub(crate) struct ProgressMonitor {
    sender: Sender<Option<(String, PathBuf)>>,
    session_id: Option<String>,
    cwd: Option<String>,
}

impl ProgressMonitor {
    pub(crate) fn new(
        cwd: Option<String>,
        deliver: Arc<dyn Fn(Value) + Send + Sync>,
    ) -> io::Result<Self> {
        let (sender, receiver) = mpsc::channel();

        thread::Builder::new()
            .name("claude-progress".into())
            .spawn(move || {
                let mut target: Option<(String, PathBuf)> = None;
                let mut reader = ProgressReader::default();
                let mut reported = None;

                loop {
                    match receiver.recv_timeout(Duration::from_millis(750)) {
                        Ok(Some(next)) => {
                            target = Some(next);
                            reader = ProgressReader::default();
                            reported = None;
                        }
                        Ok(None) | Err(RecvTimeoutError::Disconnected) => break,
                        Err(RecvTimeoutError::Timeout) => {}
                    }

                    let Some((session_id, path)) = &target else {
                        continue;
                    };

                    let Ok(snapshot) = reader.read(path) else {
                        continue;
                    };

                    if reported.as_ref() != Some(snapshot) {
                        let update = json!({
                            "method": PROGRESS_METHOD,
                            "session_id": session_id,
                            "progress": snapshot,
                        });

                        deliver(update);

                        reported = Some(snapshot.clone());
                    }
                }
            })?;

        Ok(Self {
            sender,
            session_id: None,
            cwd,
        })
    }

    pub(crate) fn watch(&mut self, session_id: &str) {
        if self.session_id.as_deref() == Some(session_id) {
            return;
        }

        if let Some(path) = session_path(self.cwd.as_deref(), session_id) {
            let _ = self.sender.send(Some((session_id.to_owned(), path)));

            self.session_id = Some(session_id.to_owned());
        }
    }

    pub(crate) fn stop_on_exit(&self) -> impl Fn() + Send + 'static {
        let sender = self.sender.clone();

        move || {
            let _ = sender.send(None);
        }
    }
}

impl Drop for ProgressMonitor {
    fn drop(&mut self) {
        let _ = self.sender.send(None);
    }
}

#[derive(Default)]
struct ProgressReader {
    cursor: u64,
    length: u64,
    modified: Option<SystemTime>,
    tracker: ProgressTracker,
}

impl ProgressReader {
    fn read(&mut self, path: &Path) -> io::Result<&ProgressSnapshot> {
        let mut file = File::open(path)?;

        let metadata = file.metadata()?;
        let modified = metadata.modified().ok();

        if metadata.len() < self.length
            || (metadata.len() == self.length && modified != self.modified)
        {
            self.cursor = 0;
            self.tracker = ProgressTracker::default();
        }

        self.length = metadata.len();
        self.modified = modified;

        file.seek(SeekFrom::Start(self.cursor))?;

        let mut reader = BufReader::new(file);
        let mut line = Vec::new();

        loop {
            line.clear();

            let length = reader.read_until(b'\n', &mut line)?;

            if length == 0 || line.last() != Some(&b'\n') {
                break;
            }

            self.cursor += length as u64;

            if let Ok(record) = serde_json::from_slice::<Value>(&line) {
                self.tracker.observe(&record);
            }
        }

        Ok(&self.tracker.snapshot)
    }
}
