#[cfg(test)]
#[path = "usage_refresh_tests.rs"]
mod usage_refresh_tests;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub(crate) enum FetchError {
    Cancelled,
    Failed(String),
}

pub(crate) trait UsageSource<T>: Send + Sync {
    fn fetch(&self, cancelled: &AtomicBool) -> Result<T, FetchError>;
}

impl<T, F> UsageSource<T> for F
where
    F: Fn(&AtomicBool) -> Result<T, FetchError> + Send + Sync,
{
    fn fetch(&self, cancelled: &AtomicBool) -> Result<T, FetchError> {
        self(cancelled)
    }
}

pub(crate) struct Refresh<T> {
    pub(crate) value: T,
    pub(crate) failed: bool,
    enabled: bool,
    pending: Option<Arc<AtomicBool>>,
    source: Arc<dyn UsageSource<T>>,
}

pub(crate) struct Fetch<T> {
    cancelled: Arc<AtomicBool>,
    source: Arc<dyn UsageSource<T>>,
}

pub(crate) struct Fetched<T> {
    cancelled: Arc<AtomicBool>,
    result: Result<T, FetchError>,
}

pub(crate) enum Completion {
    Updated,
    Retry,
    Discarded,
    Failed(String),
}

impl<T> Fetch<T> {
    pub(crate) fn run(self) -> Fetched<T> {
        let result = if self.cancelled.load(Ordering::Relaxed) {
            Err(FetchError::Cancelled)
        } else {
            self.source.fetch(&self.cancelled)
        };

        Fetched {
            cancelled: self.cancelled,
            result,
        }
    }
}

impl<T> Refresh<T> {
    pub(crate) fn new(value: T, source: Arc<dyn UsageSource<T>>, enabled: bool) -> Self {
        Self {
            value,
            failed: false,
            enabled,
            pending: None,
            source,
        }
    }

    pub(crate) fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;

        if !enabled && let Some(cancelled) = &self.pending {
            cancelled.store(true, Ordering::Relaxed);
        }
    }

    pub(crate) fn refreshing(&self) -> bool {
        self.pending.is_some()
    }

    pub(crate) fn begin(&mut self) -> Option<Fetch<T>> {
        if !self.enabled || self.pending.is_some() {
            return None;
        }

        let cancelled = Arc::new(AtomicBool::new(false));

        self.pending = Some(cancelled.clone());

        Some(Fetch {
            cancelled,
            source: self.source.clone(),
        })
    }

    pub(crate) fn complete(&mut self, fetched: Fetched<T>) -> Completion {
        if !self
            .pending
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &fetched.cancelled))
        {
            return Completion::Discarded;
        }

        self.pending = None;

        if fetched.cancelled.load(Ordering::Relaxed) {
            return if self.enabled {
                Completion::Retry
            } else {
                Completion::Discarded
            };
        }

        match fetched.result {
            Ok(value) => {
                self.value = value;
                self.failed = false;

                Completion::Updated
            }

            Err(FetchError::Failed(message)) => {
                self.failed = true;

                Completion::Failed(message)
            }

            Err(FetchError::Cancelled) => {
                if self.enabled {
                    Completion::Retry
                } else {
                    Completion::Discarded
                }
            }
        }
    }
}

impl<T> Drop for Refresh<T> {
    fn drop(&mut self) {
        if let Some(cancelled) = &self.pending {
            cancelled.store(true, Ordering::Relaxed);
        }
    }
}
