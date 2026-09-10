use std::hint::black_box;
use std::io::{self, Write};
use std::str;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{AppContext as _, TestAppContext, frame_stats};
use nmt_agent::chat::Item;
use nmt_agent::transcript::TextField;
use nmt_config::agent::CollapseRows;
use nmt_profiling::allocation::{AllocationCounts, AllocationScope, ProfilingAllocator};
use nmt_profiling::transcript::{OPERATIONS, Operation, Probe, Totals, flush, take_samples};
use parking_lot::Mutex;
use tracing::subscriber::with_default;
use tracing_subscriber::fmt;

use crate::profile::AgentKind;
use crate::transcript::{Entry, TranscriptView};

#[global_allocator]
static ALLOCATOR: ProfilingAllocator = ProfilingAllocator;

struct Enabled;

#[derive(Clone)]
struct LogBuffer(Arc<Mutex<Vec<u8>>>);

impl Write for LogBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Enabled {
    fn new() -> Self {
        take_totals();
        ALLOCATOR.set_enabled(true);
        frame_stats::set_enabled(true);
        Self
    }
}

impl Drop for Enabled {
    fn drop(&mut self) {
        frame_stats::set_enabled(false);
        ALLOCATOR.set_enabled(false);
        take_totals();
    }
}

fn take_totals() -> [Totals; 9] {
    take_samples()
}

fn history(turns: u64, live: Item) -> TranscriptView {
    let mut view = TranscriptView::new(AgentKind::Codex, None);

    for turn in 0..turns {
        for item in [
            Item::UserMessage {
                text: Some("question".into()),
            },
            Item::AgentMessage {
                id: format!("reply-{turn}"),
                text: Some("answer".into()),
                questions: None,
            },
        ] {
            view.append_entry(Entry {
                turn,
                item,
                metadata: Default::default(),
            });
        }

        view.turn_ledger.settle_replayed(turn, None);
    }

    view.append_entry(Entry {
        turn: turns,
        item: live,
        metadata: Default::default(),
    });
    view.refresh_rows(CollapseRows::WorkAndToolCalls);
    view
}

fn reasoning(capacity: usize) -> Item {
    let mut text = String::with_capacity(capacity);

    text.push_str("working");

    Item::Reasoning {
        id: "live".into(),
        summary: Some(text),
    }
}

#[gpui::test]
fn profiles_real_updates_and_mirror_revisions_without_changing_results(cx: &mut TestAppContext) {
    assert!(Probe::start(Operation::AppendDelta).is_none());
    assert!(AllocationScope::start().is_none());

    cx.update(|cx| {
        let entity = cx.new(|_| history(4, reasoning(1024)));
        let _enabled = Enabled::new();

        entity.update(cx, |view, cx| {
            assert!(view.append_delta("live", " more", TextField::ReasoningSummary));
            assert!(!view.append_delta("missing", "ignored", TextField::ReasoningSummary));
            view.refresh_rows(CollapseRows::WorkAndToolCalls);
            view.refresh_rows(CollapseRows::WorkAndToolCalls);

            let totals = take_totals();
            let delta = totals[Operation::AppendDelta as usize];

            assert_eq!(delta.calls, 2);
            assert_eq!(delta.allocation_samples, 2);
            assert_eq!(delta.allocations, AllocationCounts::default());
            assert_eq!(totals[Operation::RowsRebuild as usize].calls, 1);
            assert_eq!(view.row_cache.rebuilt_entries, 0);

            let growth = "x".repeat(2048);

            assert!(view.append_delta("live", &growth, TextField::ReasoningSummary));
            let grown = take_totals()[Operation::AppendDelta as usize];

            assert_eq!(grown.allocations.reallocations, 1);
            assert!(grown.allocations.reallocated_bytes >= 2048);

            let source = [Item::AgentMessage {
                id: "mirrored".into(),
                text: Some("mirror content".repeat(128)),
                questions: None,
            }];

            for operation in [Operation::BackgroundSnapshot, Operation::WorkflowSnapshot] {
                let snapshot = {
                    let _profile = Probe::start(operation);
                    source.to_vec()
                };

                view.show_items(&snapshot, 1, cx);
            }

            let totals = take_totals();

            for operation in [
                Operation::BackgroundSnapshot,
                Operation::WorkflowSnapshot,
                Operation::MirrorRebuild,
            ] {
                let total = totals[operation as usize];

                assert_eq!(total.calls, 1);
                assert!(total.allocations.allocated_bytes >= 128 * 14);
            }

            assert!(view.contains_item("mirrored"));
            assert!(!view.contains_item("live"));
            view.show_items(&source, 2, cx);
            assert_eq!(take_totals()[Operation::MirrorRebuild as usize].calls, 1);

            let log = LogBuffer(Arc::default());
            let writer = log.clone();
            let subscriber = fmt()
                .without_time()
                .with_ansi(false)
                .with_writer(move || writer.clone())
                .finish();

            with_default(subscriber, || {
                view.append_delta("mirrored", " additional", TextField::Reply);
                flush();
                flush();
            });

            let bytes = log.0.lock();
            let output = str::from_utf8(&bytes).unwrap();

            assert_eq!(output.matches("transcript operation costs").count(), 1);
            assert!(output.contains("transcript_perf"));
            assert!(output.contains("operation=\"append-delta\""));
            assert!(output.contains("allocation_samples=1"));
            assert!(!output.contains("mirror content"));
        });
    });

    assert!(Probe::start(Operation::AppendDelta).is_none());

    let _enabled = Enabled::new();

    ALLOCATOR.set_enabled(false);
    drop(Probe::start(Operation::AppendDelta));
    let unavailable = take_totals()[Operation::AppendDelta as usize];

    assert_eq!(unavailable.calls, 1);
    assert_eq!(unavailable.allocation_samples, 0);
}

/// Each timed batch starts with fresh, warmed state. Allocation batches run
/// separately so per-allocation counting is not charged to the timing result.
fn measure<C, T>(
    context: &mut C,
    label: &str,
    iterations: usize,
    make: impl Fn(&mut C) -> T,
    run: impl Fn(&mut T, usize, &mut C),
) {
    let mut warm = make(context);

    for index in 0..64 {
        run(&mut warm, index, context);
    }

    drop(warm);

    let mut samples = Vec::new();

    for _ in 0..5 {
        let mut state = make(context);
        let started = Instant::now();

        for index in 0..iterations {
            run(black_box(&mut state), index, context);
        }

        samples.push(started.elapsed());
        black_box(&state);
    }

    samples.sort_unstable();

    let mut state = make(context);
    let _enabled = Enabled::new();
    let allocations = AllocationScope::start().unwrap();

    for index in 0..iterations {
        run(black_box(&mut state), index, context);
    }

    let allocations = allocations.finish();
    let totals = take_totals();

    eprintln!(
        "{label}: iterations={iterations} median_us={:.3} min_us={:.3} max_us={:.3} allocations={allocations:?}",
        samples[2].as_secs_f64() * 1e6 / iterations as f64,
        samples[0].as_secs_f64() * 1e6 / iterations as f64,
        samples[4].as_secs_f64() * 1e6 / iterations as f64,
    );

    for (operation, total) in OPERATIONS.into_iter().zip(totals) {
        if total.calls > 0 {
            eprintln!(
                "  {}: calls={} allocations={:?}",
                operation.label(),
                total.calls,
                total.allocations
            );
        }
    }
}

#[gpui::test]
#[ignore = "manual transcript timing and allocation baseline; run alone"]
fn long_transcript_profile(cx: &mut TestAppContext) {
    eprintln!(
        "transcript baseline: debug_assertions={} timings exclude active probes; allocations are separate inclusive batches",
        cfg!(debug_assertions)
    );

    for turns in [0, 500, 5_000] {
        measure(
            &mut (),
            &format!("delta-reserved/{turns}"),
            200,
            |_| history(turns, reasoning(4096)),
            |view, _, _| {
                black_box(view.append_delta("live", "x", TextField::ReasoningSummary));
            },
        );

        measure(
            &mut (),
            &format!("delta-rows/{turns}"),
            200,
            |_| history(turns, reasoning(4096)),
            |view, _, _| {
                black_box(view.append_delta("live", "x", TextField::ReasoningSummary));
                view.refresh_rows(CollapseRows::WorkAndToolCalls);
                black_box(&view.rows);
            },
        );
    }

    let chunk = "additional output ".repeat(64);

    measure(
        &mut (),
        "delta-growth/5000",
        200,
        |_| history(5_000, reasoning(7)),
        |view, _, _| {
            black_box(view.append_delta("live", &chunk, TextField::ReasoningSummary));
        },
    );

    measure(
        &mut (),
        "reply-unicode-typewriter-rows/5000",
        200,
        |_| {
            (
                history(
                    5_000,
                    Item::AgentMessage {
                        id: "live".into(),
                        text: Some("reply \u{4e2d}\u{6587} ".repeat(16_384)),
                        questions: None,
                    },
                ),
                Instant::now(),
            )
        },
        |(view, started), index, _| {
            black_box(view.append_delta("live", " more \u{4e2d}\u{6587}", TextField::Reply));
            view.advance_typing(*started + Duration::from_millis((index as u64 + 1) * 16));
            view.refresh_rows(CollapseRows::WorkAndToolCalls);
            black_box(&view.rows);
        },
    );

    let completed = Item::AgentMessage {
        id: "reply-2500".into(),
        text: Some("completed answer".into()),
        questions: None,
    };

    measure(
        &mut (),
        "middle-completion-rows/5000",
        20,
        |_| history(5_000, reasoning(4096)),
        |view, _, _| {
            view.merge_completed(&completed);
            view.refresh_rows(CollapseRows::WorkAndToolCalls);
            black_box(&view.rows);
        },
    );

    for (name, operation, entries) in [
        ("background", Operation::BackgroundSnapshot, 512),
        ("workflow", Operation::WorkflowSnapshot, 10_000),
    ] {
        let source: Vec<_> = (0..entries)
            .map(|index| Item::AgentMessage {
                id: format!("mirror-{index}"),
                text: Some("stored output ".repeat(32)),
                questions: None,
            })
            .collect();

        for changed in [false, true] {
            let entity = cx.new(|_| TranscriptView::new(AgentKind::Codex, None));
            cx.update(|cx| {
                entity.update(cx, |_, cx| {
                    measure(
                        cx,
                        &format!("{name}-snapshot-changed={changed}/{entries}"),
                        20,
                        |cx| {
                            let mut view = TranscriptView::new(AgentKind::Codex, None);

                            view.show_items(&source, 1, cx);
                            view.refresh_rows(CollapseRows::WorkAndToolCalls);
                            view
                        },
                        |view, index, cx| {
                            // Keep the source copy before the revision check,
                            // matching the current detail-panel update order.
                            let snapshot = {
                                let _profile = Probe::start(operation);
                                source.clone()
                            };
                            let revision = if changed { index as u64 + 2 } else { 1 };

                            view.show_items(&snapshot, revision, cx);
                            view.refresh_rows(CollapseRows::WorkAndToolCalls);
                            black_box(&view.rows);
                        },
                    );
                });
            });
        }
    }
}
