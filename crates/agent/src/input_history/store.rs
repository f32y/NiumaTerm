use std::collections::{BTreeMap, VecDeque};
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use nmt_platform::filesystem::replace_file;
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use uuid::Uuid;

use crate::input_history::InputHistoryScope;

const HISTORY_FILE_VERSION: u32 = 2;
const MAX_ENTRIES_PER_SCOPE: usize = 100;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct StoredHistory<E = HistoryEntry> {
    version: u32,
    scopes: Vec<StoredScope<E>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct StoredScope<E> {
    target: String,
    backend: String,
    cwd: String,

    /// Absent from a file written before workspaces could own more than one
    /// directory, which is exactly the single-directory case this field is
    /// empty for.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    additional: String,

    entries: Arc<VecDeque<E>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct HistoryEntry {
    id: Uuid,
    created_at: u64,
    text: String,
}

#[derive(Clone, Debug, Default)]
pub(super) struct HistoryStore {
    scopes: BTreeMap<InputHistoryScope, Arc<VecDeque<HistoryEntry>>>,
}

impl HistoryStore {
    pub(super) fn record(&mut self, scope: &InputHistoryScope, text: String) -> bool {
        let text = text.trim().to_string();

        if text.is_empty() {
            return false;
        }

        let entries = self.scopes.entry(scope.clone()).or_default();

        if entries.back().is_some_and(|entry| entry.text == text) {
            return false;
        }

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros();

        let created_at = u64::try_from(timestamp).unwrap_or(u64::MAX).max(
            entries
                .back()
                .map_or(0, |entry| entry.created_at.saturating_add(1)),
        );

        let entries = Arc::make_mut(entries);

        entries.push_back(HistoryEntry {
            id: Uuid::new_v4(),
            created_at,
            text,
        });

        while entries.len() > MAX_ENTRIES_PER_SCOPE {
            entries.pop_front();
        }

        true
    }

    pub(super) fn entries(&self, scope: &InputHistoryScope) -> Vec<String> {
        self.scopes
            .get(scope)
            .map(|entries| entries.iter().map(|entry| entry.text.clone()).collect())
            .unwrap_or_default()
    }

    fn merge(&mut self, snapshot: &StoredHistory) {
        for scope in &snapshot.scopes {
            let key: InputHistoryScope = scope.into();
            let entries = self.scopes.entry(key).or_default();

            // Stable identities make repeated saves idempotent even when a
            // snapshot predates another process's writes. Sorting before the
            // cap prevents old snapshots from reviving expired entries.
            let unique: BTreeMap<Uuid, HistoryEntry> = entries
                .iter()
                .chain(scope.entries.iter())
                .map(|entry| (entry.id, entry.clone()))
                .collect();

            let mut merged: Vec<_> = unique.into_values().collect();

            merged.sort_by_key(|entry| (entry.created_at, entry.id));

            let keep_from = merged.len().saturating_sub(MAX_ENTRIES_PER_SCOPE);

            *entries = Arc::new(merged.into_iter().skip(keep_from).collect());
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

    #[derive(Deserialize)]
    struct Version {
        version: u32,
    }

    let version: Version = serde_json::from_slice(&bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;

    let mut history = HistoryStore::default();

    match version.version {
        1 => {
            let stored: StoredHistory<String> = serde_json::from_slice(&bytes)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;

            for scope in stored.scopes {
                let key: InputHistoryScope = (&scope).into();
                let mut entries = VecDeque::<HistoryEntry>::new();

                for (index, text) in scope.entries.iter().enumerate() {
                    let text = text.trim();

                    if text.is_empty() || entries.back().is_some_and(|entry| entry.text == text) {
                        continue;
                    }

                    // Every process migrating the same legacy row must assign
                    // the same identity, including repeated nonadjacent text.
                    let identity = serde_json::to_vec(&(
                        &key.target,
                        &key.backend,
                        &key.cwd,
                        &key.additional,
                        index,
                        text,
                    ))
                    .map_err(io::Error::other)?;

                    entries.push_back(HistoryEntry {
                        id: Uuid::new_v5(&Uuid::NAMESPACE_URL, &identity),
                        created_at: u64::try_from(index).unwrap_or(u64::MAX),
                        text: text.to_string(),
                    });

                    if entries.len() > MAX_ENTRIES_PER_SCOPE {
                        entries.pop_front();
                    }
                }

                history.scopes.insert(key, Arc::new(entries));
            }
        }

        HISTORY_FILE_VERSION => {
            let stored: StoredHistory = serde_json::from_slice(&bytes)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;

            history.merge(&stored);
        }

        version => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unsupported Agent input history version {version}"),
            ));
        }
    }

    Ok(history)
}

pub(super) fn save_to_path(path: &Path, history: &StoredHistory) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));

    fs::create_dir_all(parent)?;

    let mut lock_name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing history filename"))?
        .to_os_string();

    lock_name.push(".lock");

    // The destination is replaced on every save. A persistent sibling lock
    // keeps all writers synchronized on one unchanged file identity.
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(parent.join(lock_name))?;

    lock.lock()?;

    let mut merged = load_from_path(path)?;

    merged.merge(history);

    let content =
        serde_json::to_vec_pretty::<StoredHistory>(&(&merged).into()).map_err(io::Error::other)?;

    let mut temporary = NamedTempFile::new_in(parent)?;

    temporary.write_all(&content)?;
    temporary.as_file().sync_all()?;

    let temporary = temporary.into_temp_path();

    replace_file(&temporary, path)
}

impl From<&HistoryStore> for StoredHistory {
    fn from(value: &HistoryStore) -> Self {
        StoredHistory {
            version: HISTORY_FILE_VERSION,
            scopes: value
                .scopes
                .iter()
                .map(|(scope, entries)| StoredScope {
                    target: scope.target.clone(),
                    backend: scope.backend.clone(),
                    cwd: scope.cwd.clone(),
                    additional: scope.additional.clone(),
                    entries: Arc::clone(entries),
                })
                .collect(),
        }
    }
}

impl<E> From<&StoredScope<E>> for InputHistoryScope {
    fn from(value: &StoredScope<E>) -> Self {
        InputHistoryScope {
            target: value.target.clone(),
            backend: value.backend.clone(),
            cwd: value.cwd.clone(),
            additional: value.additional.clone(),
        }
    }
}
