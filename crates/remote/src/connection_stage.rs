use std::future::Future;

use anyhow::Result;
use tokio::time::Instant;
use tracing::{Span, info};

struct Stage {
    name: &'static str,
    started: Instant,
    span: Span,
    finished: bool,
}

impl Drop for Stage {
    fn drop(&mut self) {
        if !self.finished {
            info!(parent: &self.span, stage = self.name,
                elapsed_ms = self.started.elapsed().as_millis(), outcome = "cancelled",
                "connection stage ended");
        }
    }
}

/// Cancellation drops the future before it returns an error, so record it
/// separately from network failures and completed stages.
pub(crate) async fn run<T>(name: &'static str, work: impl Future<Output = Result<T>>) -> Result<T> {
    let mut stage = Stage {
        name,
        started: Instant::now(),
        span: Span::current(),
        finished: false,
    };

    info!(stage = name, "connection stage started");

    let result = work.await;

    stage.finished = true;

    match &result {
        Ok(_) => info!(
            stage = name,
            elapsed_ms = stage.started.elapsed().as_millis(),
            outcome = "completed",
            "connection stage ended"
        ),
        Err(error) => info!(stage = name, elapsed_ms = stage.started.elapsed().as_millis(),
            outcome = "failed", error = %format_args!("{error:#}"), "connection stage ended"),
    }

    result
}
