# Transcript performance measurements

## Live application

Build the optional collectors, then use the existing runtime switch:

```powershell
.\scripts\profiling.ps1 build -p app
.\target\profiling\debug\NiumaTerm.exe --testing --enable-profiling
```

Normal agent interactions now record transcript operation costs alongside the
existing frame statistics. On Windows, testing logs normally live in
`%LOCALAPPDATA%\NiumaTerm\Test\logs\app.log`. Search for `transcript_perf`.
The default log level includes these records; a restrictive `RUST_LOG` can hide
them. Profiling does not generate synthetic conversations in the application.

NiumaTerm's collectors are compiled only with `--cfg enable_profiling`.
Without that cfg, the runtime flag is accepted but logs a warning explaining
that a profiling build is needed.

The `nmt_profiling` crate owns allocation counting, transcript operation totals,
and NiumaTerm's per-second frame statistics. It does not depend on GPUI.
GPUI re-exports the frame-statistics interface for the existing NiumaTerm probes;
both the application and those probes use the same counters. Transcript content
and its update rules remain in the existing Agent crates. The application owns
the reporting timer and shutdown callback.

Upstream task/action/window profilers, hang detection, journal collection, and
GPUI/GPUI Kit measurement helpers remain in their original libraries with their
existing feature controls. The profiling script does not enable those features.
The custom cfg only controls NiumaTerm's collectors and performance workloads.

`scripts/profiling.ps1` supplies the cfg through `scripts/profiling.toml` and
writes to `target/profiling`, keeping normal executables separate. The extra
configuration preserves the workspace's Windows static-CRT flags and Apple CPU
flags. If `RUSTFLAGS` or `CARGO_ENCODED_RUSTFLAGS` already overrides Cargo's
configuration, the script appends the cfg to that active value and restores it
afterward. Existing environment overrides remain responsible for platform flags.

`scripts/frame-stats.ps1` uses that build script. Its `-NoBuild` option expects
an executable already built in `target/profiling`. Every application launch
from this script uses `--testing --enable-profiling`.

The UI thread drains its samples once per second and once at graceful shutdown.
Only operations observed in that interval produce records. The reporting timer
does not request a repaint. Forced process termination may lose the last interval.

| Operation | Measured region |
| --- | --- |
| `append-entry` | Index insertion, content insertion, and row invalidation; timestamp formatting is outside this region |
| `replay` | Restored turn insertion, timestamp formatting, accounting, and notification; includes nested `append-entry` samples |
| `merge-completed` | Indexed completion matching, content merge, and cache invalidation |
| `append-delta` | Indexed lookup, text append, typing initialization when needed, and cache invalidation; includes unmatched attempts |
| `rows-rebuild` | Dirty suffix selection, row generation, and virtual-list update; clean cache hits are not sampled |
| `typewriter` | An active typing tick, including character counting and cache invalidation |
| `mirror-rebuild` | A changed revision's content copy, index rebuild, invalidation, and notification; excludes later row rebuilding |
| `background-snapshot` | Reading and copying the child-agent detail source, before checking the mirrored revision |
| `workflow-snapshot` | Reading the workflow detail label and copying its source, before checking the mirrored revision |

Each record contains:

- `calls`, `total_us`, `avg_us`, `max_us`: synchronous elapsed time.
- `allocation_samples`: calls with allocation tracking enabled. A zero here
  means allocation data is unavailable, not that an operation allocated nothing.
- `alloc`, `realloc`, `dealloc`: successful Rust allocation calls, successful
  reallocations, and deallocation calls within those scopes.
- `allocated_bytes`: requested sizes from successful alloc/alloc_zeroed calls.
- `reallocated_bytes`: full new requested sizes from successful realloc calls,
  including reallocations that stay in place. This is not just capacity growth.
- `deallocated_bytes`: sizes passed to dealloc, excluding realloc's old size.

Byte counts are traffic, not retained or peak heap size. They include allocations
made and freed entirely within an operation. Values released outside the measured
region do not contribute to that region's deallocation count. Native allocations
and work performed by other threads are excluded. Nested scopes are inclusive;
do not sum parent and child records. Message contents and identifiers are not
recorded.

## Instrumentation cost

Without `enable_profiling`, the executable uses its normal allocator with no
profiling wrapper. NiumaTerm's transcript and frame hooks have no timing or
counter state; allocation TLS and reporting timers are excluded. The disabled
crate has no external dependencies. Upstream instrumentation is outside this
cfg and retains its separate build controls.

With the cfg set but the runtime switch off, the profiling allocator
still checks an atomic switch and transcript probes check the shared frame switch.
This configuration is not equivalent to a build without instrumentation.

With profiling enabled, allocator callbacks update fixed-size thread-local
counters only while a measurement scope is active. They do not allocate or lock.
Probe completion accumulates results without formatting or logging; the periodic
report runs outside the synchronous measurement scopes. Live elapsed times still
include counting and any nested probe overhead.

Only the application and the relevant unit-test executables install the wrapper.
Other binaries using the UI library do not automatically replace their allocator.
They must install `ProfilingAllocator` and enable it to obtain allocation samples.

## Repeatable baseline

The ignored test uses the same update methods as the UI with synthetic data:

```powershell
.\scripts\profiling.ps1 test -p nmt_agent_ui --release --lib transcript::profiling_tests::long_transcript_profile '--' --ignored --exact --nocapture --test-threads=1
```

Quote `'--'` when passing test-runner arguments through PowerShell scripts.
Omit `--release` to record a Debug baseline too. The output identifies whether
debug assertions are enabled. Record the source revision, toolchain, profile,
machine, and CPU affinity when comparing runs. Run serially without concurrent
builds or competing workloads; compare repeated batches rather than a single
Debug percentage.

Scenarios cover reserved-capacity reasoning deltas and delta-plus-row updates
at 0, 500, and 5,000 historical turns; growing strings; a long Unicode reply with
typing ticks; completion in the middle of history; and unchanged/changed detail
revisions at 512 child entries and 10,000 workflow entries.

Each scenario has a warm-up and five fresh-state timing batches with probes and
allocation tracking disabled. It prints the median/minimum/maximum batch time
divided by the operation count, not per-operation latency percentiles. A separate
fresh-state batch enables tracking and prints allocation totals for the batch
and its instrumented operations. Fixture construction, initial row-cache fill,
report formatting, and final state destruction are outside the measured batches.
The disabled allocator check remains present during timing.

The detail scenarios reproduce the current copy-before-revision-check sequence;
they do not invoke the complete application panel renderer. These are synchronous
content and row-update measurements, not GPU rendering, text layout, image
decoding, provider I/O, or end-to-end input latency measurements. When changing
detail ownership, update these adapters together with the application callers.

The historical-completion scenario repeats the same completed payload after its
first replacement. The changed-detail scenarios advance revisions without
changing message values, isolating revision-triggered copying and rebuilding
from changes in the rendered rows. They cover redundant provider updates, not
every possible content change.

## Initial measurements

Observed on Windows x86_64, AMD Ryzen 9 9950X3D, with
`rustc 1.98.0-nightly (cced03bfd 2026-05-28)`. Source: `4268ede6` plus this
instrumentation change. No CPU affinity was imposed. Release and Debug suites
ran serially without a concurrent build. These are initial observations, not
regression thresholds or a comparison against an unmodified executable. These
numbers predate compile-time gating and retain the original runtime-only
instrumentation setup; they are not measurements of the new default build.

Times are microseconds per operation, derived from the median of five batch
times. Allocation columns are totals for one separate batch of N operations;
the Debug and Release allocation totals matched for all listed scenarios.

| Scenario | N | Debug us/op | Release us/op | alloc | realloc | allocated bytes | realloc requested bytes |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `delta-reserved/0` | 200 | 0.217 | 0.015 | 0 | 0 | 0 | 0 |
| `delta-rows/0` | 200 | 2.941 | 0.371 | 1600 | 0 | 1241600 | 0 |
| `delta-reserved/500` | 200 | 0.245 | 0.015 | 0 | 0 | 0 | 0 |
| `delta-rows/500` | 200 | 18.379 | 2.267 | 2603 | 0 | 2268672 | 0 |
| `delta-reserved/5000` | 200 | 0.257 | 0.017 | 0 | 0 | 0 | 0 |
| `delta-rows/5000` | 200 | 27.022 | 3.331 | 3003 | 0 | 2678272 | 0 |
| `delta-growth/5000` | 200 | 0.586 | 0.445 | 0 | 9 | 0 | 592249 |
| `reply-unicode-typewriter-rows/5000` | 200 | 30.981 | 8.164 | 2803 | 1 | 2671872 | 425984 |
| `middle-completion-rows/5000` | 20 | 1918.040 | 112.090 | 62 | 0 | 22496 | 0 |
| `background-snapshot-changed=false/512` | 20 | 56.495 | 22.130 | 20500 | 0 | 6080360 | 0 |
| `background-snapshot-changed=true/512` | 20 | 325.005 | 67.905 | 51240 | 0 | 12834360 | 0 |
| `workflow-snapshot-changed=false/10000` | 20 | 1291.165 | 542.590 | 400020 | 0 | 118977800 | 0 |
| `workflow-snapshot-changed=true/10000` | 20 | 9557.205 | 3158.430 | 1000040 | 0 | 251333400 | 0 |

Reserved-capacity content appends allocate nothing in this workload; row-cache
updates still allocate temporary storage. An unchanged 10,000-entry workflow
snapshot requests 5,948,890 bytes across 20,001 allocations per synchronization.
That traffic is released again; it is not a claim of equivalent permanent heap
growth. The content-copy behavior remains unchanged in this instrumentation work.

## Content model extraction

`nmt_agent::transcript::TranscriptContent` now owns the entry vector and the
message-ID index previously held by `TranscriptView`. It handles insertion,
replacement, clearing, typed text deltas, completed payload merging, and content
queries for replies, errors, work steps, and task lists. Read access borrows the
stored entries; it does not make a snapshot.

`TranscriptView` owns one instance of this model, replacing its old fields. There
is no second copy in session state and no synchronization between two live
transcript collections. Each `TranscriptEntry<M>` stores its caller-defined
metadata inline. The UI supplies `EntryPresentation` containing the formatted
timestamp and image handles. The content module neither interprets this metadata
nor requires it to implement `Clone`; moving the entry transfers its contents and
metadata together. This preserves the original per-entry storage costs without a
parallel metadata vector or an extra lookup for every displayed entry.

Updates return the inserted or matched entry index. Text updates also report the
previous byte length and whether the resulting text is non-blank. `TextField`
limits updates to reply text, reasoning summaries, and command output, preventing
callers from changing indexed IDs through an arbitrary mutable-item callback.
Duplicate IDs retain insertion order, and only the first compatible item is
updated. Completed payloads retain the existing rules for omitted fields.

The UI still controls row invalidation, grouping, disclosure state, virtual-list
heights, scrolling, typing, and code/image presentation. A new typed reply counts
characters only in the prefix that existed before its delta; subsequent deltas
do not recount that prefix or restart the animation. Replay timestamp formatting
and turn display accounting also remain in the UI; the resulting entries move
into the model through the same insertion operation as live messages.

The profiling regions and workloads are unchanged apart from calling the typed
update interface. Detail panels still copy their source before checking its
revision, and changed mirrors still rebuild their owned entries. Those ownership
and refresh changes are deliberately separate from this extraction.

### Extraction comparison method

The comparison uses saved executables from `1668ddc9` and executables built from
the extraction working tree, both with `enable_profiling`. It does not reuse the
older runtime-only numbers above. Both versions use the toolchain and machine
listed above. The benchmark process and its children run on logical CPU 2
(affinity mask `0x4`); the application process is not changed. Builds and other
validation commands finish before the comparisons run.

Debug uses five before/after pairs and Release uses nine. Execution order
alternates between before/after and after/before. Each executable still runs the
same five timing batches per scenario. The table below reports the median of
those per-execution medians, in microseconds per operation. Separate allocation
batches use the same operation count as before; all six counters, including
deallocations and their byte counts, are compared rather than only allocation
counts. These runs measure the instrumented build with active sampling off for
timing, not the instrumentation-free build.

The extraction initially added measurable Debug overhead through read and update
helpers. The final read interface directly borrows the stored slice and is
inlined; row traversals retain that borrow instead of repeatedly converting the
vector. The streamed update method is also inlined in Debug to avoid an additional
call and result transfer for each chunk. Optimized builds retain the compiler's
normal inlining decisions. Replacement carries an ordinary inline hint
so optimized callers can combine collection and index rebuilding. No workload,
row-grouping rule, or allocation-counting region was reduced for these changes.

### Extraction measurements

| Scenario | Debug before | Debug after | Change | Release before | Release after | Change |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `delta-reserved/0` | 0.236 | 0.220 | -6.8% | 0.015 | 0.015 | 0.0% |
| `delta-rows/0` | 2.971 | 3.014 | +1.4% | 0.388 | 0.374 | -3.6% |
| `delta-reserved/500` | 0.259 | 0.229 | -11.6% | 0.015 | 0.015 | 0.0% |
| `delta-rows/500` | 18.848 | 18.774 | -0.4% | 2.314 | 2.339 | +1.1% |
| `delta-reserved/5000` | 0.258 | 0.239 | -7.4% | 0.015 | 0.015 | 0.0% |
| `delta-rows/5000` | 26.723 | 26.670 | -0.2% | 3.368 | 3.370 | +0.1% |
| `delta-growth/5000` | 0.505 | 0.487 | -3.6% | 0.242 | 0.206 | -14.9% |
| `reply-unicode-typewriter-rows/5000` | 31.503 | 31.477 | -0.1% | 8.259 | 8.246 | -0.2% |
| `middle-completion-rows/5000` | 1958.730 | 1953.075 | -0.3% | 114.555 | 114.110 | -0.4% |
| `background-snapshot-changed=false/512` | 58.100 | 57.645 | -0.8% | 22.050 | 21.765 | -1.3% |
| `background-snapshot-changed=true/512` | 332.215 | 326.775 | -1.6% | 69.340 | 69.805 | +0.7% |
| `workflow-snapshot-changed=false/10000` | 1227.965 | 1222.355 | -0.5% | 515.245 | 512.195 | -0.6% |
| `workflow-snapshot-changed=true/10000` | 7199.850 | 7164.195 | -0.5% | 1980.845 | 1930.245 | -2.6% |

All six allocation counters match before and after for every scenario in both
profiles and every retained pair. Reserved-capacity deltas still allocate
nothing; string growth performs the same reallocations, and row refreshes and
mirrors request the same allocation sizes as before. The x86_64 UI entry still
occupies 192 bytes, with unchanged content and turn offsets. This is an observed
layout for this toolchain, not a fixed representation promised by the API.

The 5,000-turn delta-plus-row path is effectively unchanged in these runs:
26.723 to 26.670 us in Debug and 3.368 to 3.370 us in Release. This is not a claim
that every scenario became faster. Short-history Debug delta-plus-row updating is 1.4%
higher (2.971 to 3.014 us), and the 500-turn Release case is 1.1% higher (2.314
to 2.339 us). The short Debug per-execution medians range from 2.955 to 3.004 us
before and 3.007 to 3.142 us after; the Release 500-turn ranges are 2.300 to
2.329 us and 2.324 to 2.371 us. These small positive results remain recorded
rather than being discarded as noise. Strict zero slowdown across all measured
paths has not been established.

Raw results are retained locally under the ignored `target/profiling` directory
in `transcript-comparison-debug.json` and `transcript-comparison-release.json`.
Saved pre-change executables are in `transcript-before-1668ddc9`. The ignored
`compare-transcript.ps1` helper alternates execution order and records affinity;
the maintained benchmark entry point remains the command documented above.

### Remaining timing increases

This note expands the final extraction comparison against `1668ddc9`; it does
not introduce another sampling run. Four scenario/profile combinations have a
higher final median. The values below are whole-scenario times, not measurements
attributing the increase to a particular function. All other scenario/profile
medians in the retained results are equal to or lower than the baseline.

| Profile and scenario | Before us/op | After us/op | Increase us/op | Change | Slower pairs |
| --- | ---: | ---: | ---: | ---: | ---: |
| Debug `delta-rows/0` | 2.971 | 3.014 | +0.043 | +1.4% | 5/5 |
| Release `delta-rows/500` | 2.314 | 2.339 | +0.025 | +1.1% | 8/9 |
| Release `background-snapshot-changed=true/512` | 69.340 | 69.805 | +0.465 | +0.7% | 8/9 |
| Release `delta-rows/5000` | 3.368 | 3.370 | +0.002 | +0.1% | 5/9 |

The absolute increases are 43 ns, 25 ns, 465 ns, and 2 ns per operation,
respectively. "Slower pairs" counts paired executions whose after median exceeds
their before median; it is not a statistical significance test. The increase
column subtracts the two overall medians, not the median of paired differences.

#### Text append plus row refresh

The three `delta-rows` cases each time 200 repetitions of appending one `x` to a
live reasoning summary and refreshing rows with `WorkAndToolCalls` folding.
String capacity is reserved before timing, and no reply typing tick runs in
these cases. The suffix is the number of historical turns, not entries: the
fixtures contain 1, 1,001, and 10,001 entries for 0, 500, and 5,000 historical
turns, respectively. Initial content construction and row-cache filling are
outside the measurement.

The measured sequence contains:

1. `TranscriptView::append_delta` in [view.rs](../../crates/agent_ui/src/transcript/view.rs):
   `TranscriptContent::append_delta` looks up the message ID, selects the reasoning
   field, appends text, and checks whether the resulting text is non-blank. The
   view then invalidates the affected code and row caches.
2. `TranscriptView::refresh_rows` in [incremental/mod.rs](../../crates/agent_ui/src/transcript/incremental/mod.rs):
   locate the earliest changed turn, preserve the unchanged prefix, reconsider
   the preceding row's spacing, and regenerate the affected row specifications.
3. Row generation and `sync_transcript_tail` in [rows.rs](../../crates/agent_ui/src/transcript/rows.rs):
   classify entries, calculate row fingerprints and spacing, compare the changed
   suffix, and update the virtual list's cached measurement state as needed.
   Actual text layout and painting are not part of this timing.

The append-only controls (`delta-reserved`) became faster in Debug and stayed
equal at the reported precision in Release. This makes the combined update path
the next place to investigate; it does not prove that `refresh_rows`, a content
accessor, or a particular list method accounts for the difference. Subtracting
the append-only median from the combined median would not provide a reliable
stage time: they come from separate fixtures and timing batches.

#### Changed background detail snapshot

`background-snapshot-changed=true/512` times 20 synchronizations of a 512-entry
background-agent detail conversation. Each synchronization advances the source
revision while leaving the message values unchanged. Its measured sequence in
[profiling/tests.rs](../../crates/agent_ui/src/transcript/profiling/tests.rs) is:

1. Clone the source entries into a temporary snapshot, before any revision check.
2. Call `TranscriptView::show_items`: accept the new revision, invalidate display
   caches, copy items into the view's entries, replace the model contents, rebuild
   the message-ID index, mark rows dirty, and notify the view.
3. Call `refresh_rows`: regenerate the mirrored rows and compare them with the
   existing rows. Identical message values do not imply this regeneration is
   skipped.
4. Drop the temporary snapshot within the timed iteration.

Thus the +0.465 us belongs to the complete synchronization workload, not solely
to `source.clone()`, `TranscriptContent::replace`, or the `background-snapshot`
probe. The old and new versions perform the same two content-copy stages here;
this extraction did not add the second copy. No provider request, complete detail
panel render, image decoding, or GPU work is included.

#### Strength and limits of the evidence

| Profile and scenario | Before range us/op | After range us/op | Interpretation |
| --- | ---: | ---: | --- |
| Debug `delta-rows/0` | 2.955-3.004 | 3.007-3.142 | All five pairs are slower and these observed ranges do not overlap; keep as a follow-up candidate. |
| Release `delta-rows/500` | 2.300-2.329 | 2.324-2.371 | Ranges overlap, but eight pairs are slower; overlap alone does not dismiss the increase. |
| Release changed background snapshot | 68.315-70.470 | 69.085-71.755 | Ranges overlap, but eight pairs are slower; the costly stage is still unknown. |
| Release `delta-rows/5000` | 3.337-3.390 | 3.328-3.391 | Paired differences change sign; the +2 ns median difference does not establish a stable slowdown. |

Ranges describe per-execution medians, not confidence intervals. Allocation,
reallocation, deallocation, and their requested byte totals are identical before
and after in all four cases. Extra counted allocations therefore do not explain
these increases, but equal counts do not guarantee equal allocation latency.

The saved results do not contain separate stage timings under the same timing
conditions. Inlining, generated code layout, cache behavior, and measurement
variation remain possible explanations, not established causes. Earlier changes
to borrowing and inlining addressed intermediate results; they do not establish
the cause of these remaining differences. No whole-application or default-build
slowdown percentage can be inferred from this profiling-build benchmark.

The next diagnostic step, if pursued, is to isolate content updating, row
generation, list synchronization, and the snapshot copy/index-rebuild stages in
separate diagnostic runs, then confirm any proposed improvement against the
unchanged combined workloads. This note adds no new probes and makes no code
changes; the strict zero-slowdown check remains open.

### Behavioral coverage

Six new core tests cover compatible duplicate-ID selection, ignored updates,
empty and Unicode whitespace, completed payloads retaining streamed optional
fields, replacement and clear removing stale IDs, turn-scoped queries, and moving
non-cloneable metadata with an existing text allocation. The UI typing regression
now starts with a Unicode prefix, checks its character edge, and verifies that
later deltas neither reset it nor accept an incompatible field.

The core suite passes all 550 tests. The UI suite passes all 227 active tests in
both ordinary and profiling builds, including incremental row output compared
with a full rebuild, untouched-prefix reuse, mirror revisions, and allocation
sampling. One ordinary-build test and two profiling-build tests remain ignored
by default; the long-transcript benchmark is run explicitly for this comparison.
No application window is launched by this validation, so these results do not
claim a manual visual check or an end-to-end rendering speedup.
