# Terminal engine performance collection

The engine ownership change is committed as `3879568d`. User-run vtebench
measurements at matching grid sizes still show increased average time for
`medium_cells` and `sync_medium_cells`. The PTY probes distinguish capture
frequency, capture cost, publication cost, queued reads, and readiness waits.
The probes themselves do not change batching limits or scheduling policy.

## Build and capture

Run from the repository root in PowerShell:

```powershell
.\scripts\profiling.ps1 build -p app --bin NiumaTerm --release
.\target\profiling\release\NiumaTerm.exe --testing --enable-profiling
```

The isolated application logs to
`$env:LOCALAPPDATA\NiumaTerm\Test\logs\app.log`. Keep the terminal grid dimensions,
build mode, font settings, and benchmark arguments identical between versions.
Run one benchmark at a time in that application's terminal. For example:

```powershell
Set-Location D:\Park\vtebench
.\target\release\vtebench.exe --benchmarks .\benchmarks\medium_cells --warmup 3 --max-secs 30
```

Repeat with `sync_medium_cells` and `light_cells`. Close the terminal tab after
the command returns so the final partial reporting interval is flushed. PTY-loop
shutdown reports remaining samples; forcibly terminating the process can lose
them. Logs rotate on the next application launch, so save the current capture
before starting another instance:

```powershell
$log = Join-Path $env:LOCALAPPDATA 'NiumaTerm\Test\logs\app.log'
Select-String -LiteralPath $log -SimpleMatch 'terminal_perf' |
    ForEach-Object { $_.Line } |
    Set-Content -LiteralPath .\target\terminal-medium-profile.log -Encoding utf8
```

If `RUST_LOG` is set, its filter must enable `terminal_perf=info`. The default
application filter already includes these reports. The ordinary build omits the
profiling module, fields, clocks, and counters. The profiling build also requires
`--enable-profiling` at runtime. Existing frame and allocation probes share this
switch, so compare timings only between builds with identical instrumentation;
use ordinary builds for final throughput confirmation.

## Reading the reports

The PTY owner keeps its counters locally and reports roughly once per second of
event-loop activity. No counter update acquires a shared statistics lock.
Reports contain counts and timings only, without terminal text or input content.

Each `pty interval` entry identifies the window, route, interval number, and grid
dimensions. A resize ends the old interval before captures at the new size.

- `bytes` counts data returned by PTY reads, after any processing performed by
  ConPTY. It can differ from the bytes vtebench originally wrote.
- `drained_batches` and `saturated_batches` count reads that reach the flush
  path. `empty_batches` is a subset that had a pending snapshot but no new bytes.
  Empty reads with no pending snapshot do not enter these batch counts.
- `commands` counts queued mutations that invoke an eager flush; resize captures
  are counted separately. It excludes queries and ordinary input writes.
- `captures` counts snapshot attempts, including a failed attempt.
  `captures_per_mib` divides that count by PTY input volume. It is absent when
  the interval has no input bytes.

The following `pty stage` entries carry the same interval identifiers. Each
reports `calls`, `total_ms`, `avg_us`, `max_us`, and, when input exists,
`ms_per_mib`.

| Stage | Work measured |
| --- | --- |
| `poll` | Waiting for readiness, including idle time and synchronization deadlines |
| `read` | Calls to the PTY reader, including empty and unsuccessful reads |
| `ingest` | Byte preprocessing, VT parsing, prompt tracking, and output observers |
| `flush` | Metadata, responses, image updates, capture, publication, and notifications |
| `capture-eager` | Historical label for a capture after draining input, a queued mutation, or exit; drained reads now use the capture interval |
| `capture-saturated` | Viewport capture at the existing saturated-input interval |
| `capture-resize` | Viewport capture after resizing; excludes the resize itself |
| `publish` | Publishing the captured buffer; excludes the subsequent UI notification |
| `query` | Owner-side row, image, text, and selection requests, including rejected requests |
| `checkpoint` | VT checkpoint generation and its completion callback |

`flush` includes `capture-eager`, `capture-saturated`, and the corresponding
publication on read and command paths. Shutdown captures are recorded without an
outer flush sample. Do not sum inclusive and nested stages as if they were
independent. The overall `publish` series also contains resize publications. All measurements
are elapsed wall time, so thread preemption can increase a stage's duration.
`poll` is a wait measurement, not CPU usage or proof of an upstream bottleneck.

For several intervals, sum counts, input bytes, and durations first, then divide.
Do not average the per-interval ratios when input volumes differ. Exclude startup,
shutdown, and idle intervals when estimating sustained benchmark throughput.

## Questions to resolve

1. If eager capture frequency increased, matched input should produce more
   captures per MiB, with increased drained-batch frequency and similar cost per
   capture. Distinguish command-triggered captures using `commands`.
2. If extraction or publication grew more expensive, capture frequency should
   stay similar while its stage's average or total time per MiB increases.
3. If queued reads consume PTY time, `query` should account for a material part of
   the benchmark interval. Its absence rules out those requests for that run.
4. If measured processing cost stays similar while readiness waits grow, inspect
   ConPTY delivery and scheduling with a system profiler. These counters alone
   cannot distinguish producer stalls from thread scheduling delays.

These are hypotheses. The supplied vtebench summaries do not establish which
stage causes the remaining difference.

## Observed capture hotspot and scheduling change

The profiling run on 2026-09-11, approximately 04:46:54-04:49:07 UTC, used
window 0, route 19, and a 138 by 33 grid. Local evidence was preserved in
`target/logs/terminal-profile-20260911-0446.log` before log rotation.

The labels below are inferred from vtebench's default benchmark order and the
consecutive measurement periods; the probes do not record benchmark names.
Counts and durations were summed over the listed intervals before calculating
rates. Percentages use the summed interval wall time as their denominator.

| Inferred benchmark | Intervals | Captures/s | Capture time | Ingest time | Poll time |
| --- | --- | ---: | ---: | ---: | ---: |
| `cursor_motion` | 7-15 | 5,602 | 80.4% | 13.8% | 4.2% |
| `dense_cells` | 17-26 | 3,156 | 78.4% | 19.9% | 0.9% |
| `light_cells` | 28-36 | 6,615 | 93.4% | 3.1% | 1.9% |
| `medium_cells` | 38-47 | 4,913 | 69.2% | 21.5% | 7.6% |
| `scrolling_fullscreen` | 82-91 | 2,793 | 38.9% | 59.5% | 0.8% |
| `sync_medium_cells` | 116-125 | 132 | 1.6% | 24.8% | 70.9% |
| `unicode` | 127-135 | 6,208 | 89.5% | 7.0% | 2.0% |

The first four sections had no saturated batches or queries. Every drained batch
captured the full viewport, bypassing the interval applied to saturated batches.
Publication consumed only about 0.07-0.16% of those intervals. This identifies a
capture-frequency bottleneck in the measured build, but does not establish how
much its frequency changed from the original shared-engine version: equivalent
instrumentation from that version has not been captured.

PTY output now uses the existing 5 ms capture interval for both drained and
saturated batches. The event-loop poll timeout carries the pending capture
deadline, so a quiet pipe still publishes its final update without repeated
self-wakes. The first output after an idle interval remains eligible immediately;
explicit commands, completed synchronized updates, and final pending output at
shutdown can publish without waiting for the interval. Protocol responses and
metadata continue to be processed while capture is deferred.

The synchronized benchmark has a different time distribution: capture is already
infrequent and readiness waits dominate. Its remaining slowdown is not explained
by the capture hotspot. The follow-up below measures the new capture rate and
reports throughput. Final-frame delivery still needs behavioral validation.

## Follow-up after capture coalescing

The next run started at 04:59:24 UTC on 2026-09-11. Its measurements are in
`app-prev1.log`, rotated from `app.log`, and preserved locally in
`target/logs/terminal-profile-20260911-0459.log`. The benchmark route is again
window 0, route 19, at 138 by 33 cells. Route 20 was opened after the benchmark
and is excluded. Benchmark labels remain inferred from execution order and
measurement periods, rather than recorded by the probes.

The 5 ms capture interval is working. These values use the same aggregation
method as the previous run:

| Inferred benchmark | New intervals | Captures/s, previous to new | Capture time, previous to new | New ingest time | New poll time |
| --- | --- | ---: | ---: | ---: | ---: |
| `cursor_motion` | 8-17 | 5,602 to 191 | 80.4% to 3.0% | 16.3% | 78.2% |
| `dense_cells` | 19-28 | 3,156 to 185 | 78.4% to 6.2% | 24.1% | 67.0% |
| `light_cells` | 30-38 | 6,615 to 192 | 93.4% to 3.1% | 5.5% | 87.1% |
| `medium_cells` | 40-49 | 4,913 to 188 | 69.2% to 3.2% | 24.3% | 69.7% |
| `scrolling_fullscreen` | 83-92 | 2,793 to 158 | 38.9% to 2.0% | 60.5% | 35.6% |
| `sync_medium_cells` | 117-126 | 132 to 104 | 1.6% to 1.3% | 25.8% | 70.0% |
| `unicode` | 128-136 | 6,208 to 191 | 89.5% to 3.2% | 9.5% | 83.5% |

Publication takes less than 0.02% of each section above. There are no queued
queries in the cell sections. Snapshot publication and owner-side queries do
not account for the remaining measured throughput difference in these runs.
The measured capture durations dropped substantially; the counters do not
measure process CPU usage or energy consumption.

### User-reported throughput

The following compares the newest supplied averages with the original
shared-engine baseline at matching grid dimensions. Positive changes mean
slower samples. These summaries are distinct from the two profiling-log
comparisons above; the original baseline has no matching stage-level capture.

| Benchmark | Original average, ms | Latest average, ms | Change |
| --- | ---: | ---: | ---: |
| `cursor_motion` | 39.97 | 39.76 | -0.5% |
| `dense_cells` | 95.71 | 99.62 | +4.1% |
| `light_cells` | 20.74 | 21.66 | +4.4% |
| `medium_cells` | 34.79 | 40.35 | +16.0% |
| `scrolling` | 122.59 | 119.01 | -2.9% |
| `scrolling_bottom_region` | 179.05 | 177.95 | -0.6% |
| `scrolling_bottom_small_region` | 529.05 | 529.63 | +0.1% |
| `scrolling_fullscreen` | 22.35 | 23.57 | +5.5% |
| `scrolling_top_region` | 804.62 | 820.31 | +1.9% |
| `scrolling_top_small_region` | 515.85 | 514.45 | -0.3% |
| `sync_medium_cells` | 36.06 | 37.73 | +4.6% |
| `unicode` | 27.85 | 27.66 | -0.7% |

`medium_cells` also increased from 37.94 ms in the immediately preceding user
summary to 40.35 ms, or 6.4%. Its reported standard deviation is 10.93 ms and
its 90th percentile is 42 ms, compared with 1.65 ms and 37 ms originally.
The one-run summaries cannot separate persistent regression from run-to-run
variation, but this case deserves priority over the nearly unchanged cases.

### What the benchmark measures

Inspection of `D:/Park/vtebench/src/bench.rs`, `Benchmark::run_sample`, shows
that the timer covers `stdout.write_all` and `stdout.flush`. Reset and setup
precede the timer. There is no terminal reply or displayed-frame acknowledgment
before the timer ends. Each duration is truncated to whole milliseconds before
the summary is calculated.

On this Windows path, the producer writes through ConPTY, whose output reaches
a blocking pipe worker, a 64 KiB SPSC buffer, the PTY owner, and finally the UI.
See `crates/platform/src/windows/pipes/mod.rs` and
`crates/terminal/src/pty_pipe/mod.rs`. The benchmark observes producer write
blocking, which depends on buffering and downstream consumption; it does not
directly measure parser speed or frame latency. Thus the large drop in capture
time need not produce the same drop in the benchmark average.

### Remaining evidence and next measurements

For `medium_cells`, capture cost fell from 26.89 to 1.30 ms per MiB, while
ingest cost rose from 8.35 to 9.76 ms per MiB. Average ingest size fell from
4,546 to 3,867 bytes, and poll calls increased from 4,963 to 6,351 per second.
`dense_cells` similarly changed from 6,447 to 3,987 bytes per ingest. More
frequent, smaller deliveries and reduced batching are visible; their cause and
contribution to the benchmark change are not established by these counters.
Ingest includes preprocessing and observers as well as the VT engine.

At 05:00:30 and 05:00:31 UTC, the medium section reported individual poll
durations of 20.50 and 15.66 ms. Throughput in those intervals dropped to 22.56
and 22.10 MiB/s. In the same intervals, maximum capture time stayed below
0.60 ms and maximum ingest time below 0.26 ms. These pauses occurred inside
the measured poll stage, but that timer includes scheduling delay. No sample
timestamps connect them directly to individual vtebench outliers.

The next useful I/O measurements are the blocking worker's pipe-read duration
and returned byte counts, time waiting for ring space, and latency from setting
readiness to owner consumption. A system scheduling trace across the producer,
ConPTY host, pipe worker, and owner can distinguish slow supply from wakeup
delay. High owner poll time alone cannot make that distinction, and does not
justify increasing buffer sizes or weakening synchronization.

The frame logs show a separate rendering problem:

- In the dense section, interval-average draw duration remains about 45-49 ms,
  with 18-19 frames/s. The earlier profiling run already had 44-48 ms draws.
- In the unicode section, steady interval-average draw duration is about
  415-797 ms, with 1.2-2.4 frames/s. The earlier run already showed similarly
  long draws despite its acceptable producer-throughput result.
- In the medium section, draws average about 6.3-7.3 ms and frame rate is
  about 68-77/s, compared with 5.9-6.9 ms and 70-76/s previously.

These are ranges of interval averages, not a combined frame average. The
reported GPU-wait average rounds to 0.00 ms throughout these sections.
`frame_stats` measures the GPUI frame construction in `Window::draw` separately
from presentation. Inspect layout, shaping, and paint preparation before
attributing these long draws to GPU execution; current logs do not identify
which operation is expensive. The new capture scheduling did not introduce
the existing long dense and unicode draws.

This follow-up used supplied benchmark results, saved logs, and source
inspection. No application, automated tests, or benchmarks were run as part
of this analysis, and no additional runtime behavior was changed.

## ConPTY ceiling probe and power-plan control

`benches/conpty-pipe-probe`, run through `scripts/conpty-pipe-probe.ps1`,
starts vtebench under the bundled ConPTY and drains conout with blocking
`ReadFile` calls only: no parser, no UI, no pipe worker thread. Its sample
time is the best any consumer can reach with this ConPTY, grid, and power
plan. The probe also selects the conout pipe kind (anonymous or named), the
kernel pipe buffer size, the read buffer size, and an optional number of busy
threads inside the consumer process.

Measurements on 2026-09-11 at 138 by 33 for `medium_cells`:

- ConPTY writes conout in fixed 4096-byte chunks. About 90% of reads returned
  exactly 4096 bytes and the average stayed at 4,070 bytes with the default
  anonymous pipe, a 256 KiB anonymous pipe, and a 256 KiB named pipe. The
  4 KiB batches seen by the PTY owner are therefore ConPTY's write size, and
  neither a larger pipe buffer nor an overlapped named pipe can enlarge them.
  A 256 KiB conout pipe in the application changed nothing either.
- The Windows power plan dominated every earlier comparison. The same probe
  configuration measured 43-54 ms per sample on the "NightSaver" scheme and
  25.6-26.8 ms on "Balanced". Same-plan repeats still spread up to 25% on
  NightSaver and about 8% on Balanced. Record `powercfg /getactivescheme`
  with every benchmark; the script prints it.
- Busy threads inside the consumer slowed vtebench (two threads added 2-4 ms,
  eight added about 5 ms), so keeping cores out of idle states does not help
  on this Ryzen 9 9950X3D.
- Named 256 KiB pipes measured 25.9, 27.9, and 32.1 ms against 25.6, 26.2,
  and 26.8 ms for the default anonymous pipe: no advantage.

### Alternating A/B on Balanced

Two ordinary release builds, alternated three times each, `medium_cells`,
138 by 33, Balanced plan:

| Build | Run 1 | Run 2 | Run 3 | Mean |
| --- | ---: | ---: | ---: | ---: |
| bee9c9ec (shared engine lock) | 29.23 | 31.53 | 30.03 | 30.26 |
| 1126f046 (exclusive owner, 5 ms coalescing) | 31.72 | 31.09 | 30.68 | 31.16 |
| Trivial consumer ceiling | | | | ~26 |

The 0.9 ms mean gap sits inside the 29.2-31.5 ms spread of the baseline
alone, so the ownership refactor caused no measurable throughput change. The
earlier 34.79 ms and 40.35 ms summaries were single NightSaver runs taken at
different times and do not describe a code difference. Both builds sit about
4-5 ms above the trivial consumer; that gap is the terminal's own cost on this
path and is unchanged by the refactor. The PTY owner is idle 70-88% of the
time in these sections, so the next candidate is UI-thread rendering (about
70 frames per second at 6.5 ms each in the medium section) competing with
ConPTY and vtebench for the same core complex; minimizing the window or
shrinking the grid during a run would test that.
