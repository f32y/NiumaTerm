#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures::FutureExt as _;
use futures::future::Shared;
use gpui::{App, SvgRenderer, Task};
use merman::OperationControl;
use parking_lot::Mutex;
use tokio::sync::Semaphore;

use crate::agent_tab::transcript::mermaid::render::{DiagramTheme, PreparedDiagram, prepare};

type PreparedResult = Result<Arc<PreparedDiagram>, Arc<str>>;

type Preparation = Shared<Task<PreparedResult>>;

#[derive(Clone, PartialEq, Eq, Hash)]
struct Key {
    source: Arc<str>,
    theme: DiagramTheme,
}

struct CachedResult {
    prepared: Arc<PreparedDiagram>,
    used: u64,
}

#[derive(Default)]
struct Results {
    ready: HashMap<Key, CachedResult>,
    pending: HashMap<Key, Preparation>,
    clock: u64,
    bytes: usize,
    preparations: usize,
}

pub(super) struct DiagramStore {
    results: Mutex<Results>,
    workers: Arc<Semaphore>,
}

impl Default for DiagramStore {
    fn default() -> Self {
        Self {
            results: Mutex::new(Results::default()),
            workers: Arc::new(Semaphore::new(2)),
        }
    }
}

impl DiagramStore {
    #[cfg(test)]
    pub(super) fn preparations(&self) -> usize {
        self.results.lock().preparations
    }

    pub(super) fn size(&self, source: &Arc<str>, theme: &DiagramTheme) -> Option<(f32, f32)> {
        self.results
            .lock()
            .ready
            .get(&Key {
                source: source.clone(),
                theme: theme.clone(),
            })
            .map(|entry| entry.prepared.size)
    }

    pub(super) fn request(
        self: &Arc<Self>,
        source: Arc<str>,
        theme: DiagramTheme,
        renderer: SvgRenderer,
        cx: &App,
    ) -> Preparation {
        let key = Key { source, theme };

        let mut results = self.results.lock();

        results.clock += 1;

        let clock = results.clock;

        if let Some(cached) = results.ready.get_mut(&key) {
            cached.used = clock;

            let prepared = cached.prepared.clone();

            return Task::ready(Ok(prepared)).shared();
        }

        if let Some(pending) = results.pending.get(&key) {
            return pending.clone();
        }

        let store = self.clone();
        let request_key = key.clone();

        let task = cx
            .background_executor()
            .spawn(async move {
                let permit = store.workers.clone().acquire_owned().await;

                let prepared = match permit {
                    Ok(_permit) => prepare(
                        &request_key.source,
                        &request_key.theme,
                        OperationControl::new().with_deadline(Duration::from_secs(5)),
                        &renderer,
                    )
                    .map(Arc::new)
                    .map_err(|error| Arc::<str>::from(format!("{error:#}"))),
                    Err(error) => Err(Arc::<str>::from(error.to_string())),
                };

                let mut results = store.results.lock();

                results.pending.remove(&request_key);

                results.preparations += 1;

                if let Ok(prepared) = &prepared {
                    results.bytes += prepared.bytes;

                    let used = results.clock;

                    results.ready.insert(
                        request_key.clone(),
                        CachedResult {
                            prepared: prepared.clone(),
                            used,
                        },
                    );

                    while results.ready.len() > 256 || results.bytes > 32 * 1024 * 1024 {
                        let oldest = results
                            .ready
                            .iter()
                            .min_by_key(|(_, entry)| entry.used)
                            .map(|(key, _)| key.clone());

                        let Some(oldest) = oldest else { break };

                        if let Some(removed) = results.ready.remove(&oldest) {
                            results.bytes -= removed.prepared.bytes;
                        }
                    }
                }

                prepared
            })
            .shared();

        results.pending.insert(key, task.clone());

        task
    }
}
