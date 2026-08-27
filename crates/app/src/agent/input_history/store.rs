use std::collections::{BTreeMap, VecDeque};
use std::fs::{self, File};
use std::io::{self, Write as _};
use std::path::Path;

use nmt_platform::windows::filesystem::replace_file;
use serde::{Deserialize, Serialize};

use crate::agent::input_history::InputHistoryScope;

const HISTORY_FILE_VERSION: u32 = 1;
const MAX_ENTRIES_PER_SCOPE: usize = 100;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct StoredHistory {
    version: u32,
    scopes: Vec<StoredScope>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct StoredScope {
    target: String,
    backend: String,
    cwd: String,
    /// Absent from a file written before workspaces could own more than one
    /// directory, which is exactly the single-directory case this field is
    /// empty for.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    additional: String,
    entries: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub(super) struct HistoryStore {
    scopes: BTreeMap<InputHistoryScope, VecDeque<String>>,
}

impl HistoryStore {
    pub(super) fn record(&mut self, scope: &InputHistoryScope, text: String) -> bool {
        let text = text.trim().to_string();
        if text.is_empty() {
            return false;
        }

        let entries = self.scopes.entry(scope.clone()).or_default();
        if entries.back() == Some(&text) {
            return false;
        }

        entries.push_back(text);
        while entries.len() > MAX_ENTRIES_PER_SCOPE {
            entries.pop_front();
        }
        true
    }

    pub(super) fn entries(&self, scope: &InputHistoryScope) -> Vec<String> {
        self.scopes
            .get(scope)
            .map(|entries| entries.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub(super) fn snapshot(&self) -> StoredHistory {
        StoredHistory {
            version: HISTORY_FILE_VERSION,
            scopes: self
                .scopes
                .iter()
                .map(|(scope, entries)| StoredScope {
                    target: scope.target.clone(),
                    backend: scope.backend.clone(),
                    cwd: scope.cwd.clone(),
                    additional: scope.additional.clone(),
                    entries: entries.iter().cloned().collect(),
                })
                .collect(),
        }
    }
}

pub(super) fn load_from_path(path: &Path) -> io::Result<HistoryStore> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(HistoryStore::default());
        }
        Err(error) => return Err(error),
    };
    let stored: StoredHistory = serde_json::from_slice(&bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if stored.version != HISTORY_FILE_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported Agent input history version {}", stored.version),
        ));
    }

    let mut history = HistoryStore::default();
    for scope in stored.scopes {
        let key = InputHistoryScope {
            target: scope.target,
            backend: scope.backend,
            cwd: scope.cwd,
            additional: scope.additional,
        };
        for entry in scope.entries {
            history.record(&key, entry);
        }
    }
    Ok(history)
}

pub(super) fn save_to_path(path: &Path, history: &StoredHistory) -> io::Result<()> {
    let content = serde_json::to_vec_pretty(history)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let temporary = path.with_extension("json.tmp");
    let mut file = File::create(&temporary)?;
    file.write_all(&content)?;
    file.sync_all()?;
    drop(file);

    if let Err(error) = replace_file(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    Ok(())
}
