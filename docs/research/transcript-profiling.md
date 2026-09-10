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
