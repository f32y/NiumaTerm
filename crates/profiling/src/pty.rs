//! Owner-thread timings, reported in batches without retaining terminal output.
//! Flush includes capture and publication; those durations must not be added
//! together. Poll measures waiting for readiness, including idle time.

use std::mem;
use std::time::{Duration, Instant};

use tracing::info;

use crate::enabled;

const REPORT_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Clone, Copy)]
pub enum Stage {
    Poll,
    Read,
    Ingest,
    Flush,
    CaptureEager,
    CaptureSaturated,
    CaptureResize,
    Publish,
    Query,
    Checkpoint,
}

const STAGES: [(Stage, &str); 10] = [
    (Stage::Poll, "poll"),
    (Stage::Read, "read"),
    (Stage::Ingest, "ingest"),
    (Stage::Flush, "flush"),
    (Stage::CaptureEager, "capture-eager"),
    (Stage::CaptureSaturated, "capture-saturated"),
    (Stage::CaptureResize, "capture-resize"),
    (Stage::Publish, "publish"),
    (Stage::Query, "query"),
    (Stage::Checkpoint, "checkpoint"),
];

pub enum BatchEnd {
    Drained,
    Saturated,
}

#[derive(Clone, Copy, Default)]
struct Series {
    calls: u64,
    elapsed: Duration,
    max_elapsed: Duration,
}

#[derive(Default)]
struct Totals {
    bytes: u64,
    drained_batches: u64,
    saturated_batches: u64,
    empty_batches: u64,
    commands: u64,
    stages: [Series; STAGES.len()],
}

pub struct PtyProfiler {
    window: u64,
    route: usize,
    cols: u16,
    rows: u16,
    interval: u64,
    started: Option<Instant>,
    totals: Totals,
}

impl PtyProfiler {
    pub fn new(window: u64, route: usize, cols: u16, rows: u16) -> Self {
        Self {
            window,
            route,
            cols,
            rows,
            interval: 0,
            started: None,
            totals: Totals::default(),
        }
    }

    pub fn start(&mut self) -> Option<Instant> {
        if !enabled() {
            if self.started.take().is_some() {
                self.totals = Totals::default();
            }

            return None;
        }

        let now = Instant::now();

        self.started.get_or_insert(now);

        Some(now)
    }

    pub fn record(&mut self, stage: Stage, started: Option<Instant>) {
        let Some(started) = started else {
            return;
        };

        let elapsed = started.elapsed();
        let series = &mut self.totals.stages[stage as usize];

        series.calls += 1;
        series.elapsed += elapsed;
        series.max_elapsed = series.max_elapsed.max(elapsed);
    }

    pub fn read(&mut self, started: Option<Instant>, bytes: usize) {
        self.record(Stage::Read, started);

        if started.is_some() {
            self.totals.bytes += bytes as u64;
        }
    }

    pub fn batch(&mut self, end: BatchEnd, bytes: usize) {
        if self.started.is_none() {
            return;
        }

        match end {
            BatchEnd::Drained => self.totals.drained_batches += 1,
            BatchEnd::Saturated => self.totals.saturated_batches += 1,
        }

        if bytes == 0 {
            self.totals.empty_batches += 1;
        }
    }

    pub fn command(&mut self) {
        if self.started.is_some() {
            self.totals.commands += 1;
        }
    }

    pub fn set_grid(&mut self, cols: u16, rows: u16) {
        // A resize starts a new interval so per-cell costs can be compared
        // without averaging captures of different grid dimensions together.
        if (cols, rows) != (self.cols, self.rows) {
            self.flush();
            self.cols = cols;
            self.rows = rows;
        }
    }

    pub fn report_due(&mut self) {
        if self
            .started
            .is_some_and(|at| at.elapsed() >= REPORT_INTERVAL)
        {
            self.flush();
        }
    }

    pub fn flush(&mut self) {
        let Some(started) = self.started.take() else {
            return;
        };

        let elapsed = started.elapsed();
        let totals = mem::take(&mut self.totals);

        if !enabled() || totals.stages.iter().all(|stage| stage.calls == 0) {
            return;
        }

        self.interval += 1;

        let mib = totals.bytes as f64 / 1_048_576.0;

        let captures = [
            Stage::CaptureEager,
            Stage::CaptureSaturated,
            Stage::CaptureResize,
        ]
        .into_iter()
        .map(|stage| totals.stages[stage as usize].calls)
        .sum::<u64>();

        let captures_per_mib = (totals.bytes != 0).then(|| captures as f64 / mib);

        info!(
            target: "terminal_perf",
            window = self.window, route = self.route, interval = self.interval,
            cols = self.cols, rows = self.rows,
            elapsed_ms = elapsed.as_secs_f64() * 1000.0,
            bytes = totals.bytes, mib,
            drained_batches = totals.drained_batches,
            saturated_batches = totals.saturated_batches,
            empty_batches = totals.empty_batches, commands = totals.commands,
            captures, captures_per_mib,
            "pty interval"
        );

        for (stage, label) in STAGES {
            let series = totals.stages[stage as usize];

            if series.calls == 0 {
                continue;
            }

            let total_ms = series.elapsed.as_secs_f64() * 1000.0;
            let ms_per_mib = (totals.bytes != 0).then(|| total_ms / mib);

            info!(
                target: "terminal_perf",
                window = self.window, route = self.route, interval = self.interval,
                stage = label, calls = series.calls, total_ms,
                avg_us = total_ms * 1000.0 / series.calls as f64,
                max_us = series.max_elapsed.as_secs_f64() * 1_000_000.0,
                ms_per_mib,
                "pty stage"
            );
        }
    }
}
