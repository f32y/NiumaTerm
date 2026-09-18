**Tokio migration record — 2026-09-18**

This document records how waiting work moved onto the shared Tokio runtime, the design decisions that kept existing ordering and cleanup guarantees, and the work that deliberately stays synchronous. It supersedes the 2026-09-17 assessment of revision `56623482`, whose candidate list is now either implemented or classified below. The remote-session crate was removed in `b27d5cfa`, so its former candidates no longer apply.

## Runtime

[`nmt_runtime`](../../crates/runtime/src/lib.rs) owns one lazily started multi-thread runtime with Tokio's default of one worker per core. Terminal parsing and frame capture share it with network waits, so the pool size matters for tail latency rather than throughput. A headless probe ran N ConPTY sessions streaming colored output while a separate thread measured how long an empty task waited before running:

| Sessions | Workers | Wait p50 | p99 | max |
| --- | --- | --- | --- | --- |
| 8 | 32 (default) | 6.9 µs | 23 µs | 50 µs |
| 8 | 4 | 6.8 µs | 27 µs | 132 µs |
| 16 | 32 (default) | 6.6 µs | 36 µs | 110 µs |
| 16 | 4 | 7.0 µs | 103 µs | 210 µs |
| 32 | 32 (default) | 9.5 µs | 190 µs | 665 µs |
| 32 | 4 | 10.6 µs | 188 µs | 784 µs |

ConPTY delivers output in small pieces (about 77 bytes per batch), so one read/parse/flush batch averaged 2–5 µs. A frame capture averaged 280 µs and runs at most once per capture interval per session. From the measured parse cost, a full 64 KiB batch bounds a neighbor's wait at roughly 1.3 ms. At 32 sessions the machine is saturated mostly by the OpenConsole processes, and the pool size stops mattering.

Blocking work uses the runtime's blocking pool, and only for finite operations. The GPUI side reaches the runtime through [`on_runtime`](../../crates/app/src/utils.rs), which spawns a task and resumes its panic in the caller. GPUI's test scheduler accepts wakes only from its own thread, so under `cfg(test)` that helper blocks on the work in place. Tests that drive runtime work through other paths call `allow_parking` and wait for the result.

## Terminal PTY

`Termio::run_event_loop` is an ordinary runtime task. It owns the Ghostty engine and polls the PTY through [`AsyncPty`](../../crates/platform/src/async_pty.rs), which replaced the mio-based `ProcessReadWrite`/`EventedPty` traits. Commands arrive on a Tokio channel. One reusable timer tracks the earliest capture or delayed-input deadline and moves only when that deadline changes. An idle terminal has no timer and no thread.

On Windows, output reads use Tokio's named-pipe IOCP registration. Input keeps the overlapped writer because ordering requires observable native completion: Tokio's named-pipe `poll_flush` succeeds while a write is still in flight, which would let a resize overtake the input submitted before it. The writer's completion callback wakes the task through `SoftReady`, whose waker is installed before each completion check. The sequence stays:

```text
submit input A -> await native completion of A -> submit resize -> submit input B
```

A resize moves the console owner into a blocking task while output keeps draining. Shutdown waits for that resize, then closes the console asynchronously: `ClosePseudoConsole` runs on the blocking pool under a two-second timeout, after which the job object ends the shell tree. The owner task keeps reading output during the close, which the console host needs to finish its writes. The final frame and the close notification are published before this teardown so a closed tab is not delayed by it. Dropping an unawaited `Pty` starts the same close detached.

On Unix, the PTY descriptor is an `AsyncFd`, and child exit uses a Tokio SIGCHLD stream created before the first `waitpid` check. The signal-hook self-pipe and the mio dependency are gone.

Diagnostic VT tracing writes through one dedicated thread with a bounded queue. Tracing can emit a record per PTY chunk, and a persistent blocking sink is cheaper on a thread of its own than as a blocking-pool hop per record.

## Agent processes

[`spawn_piped`](../../crates/platform/src/windows/process.rs) starts a child with all three standard streams piped to the runtime. On Windows, Tokio's own child stdio wraps anonymous pipes in blocking-pool reads, which keeps a pooled thread parked on every pending read. The helper instead hands the child the same overlapped named-pipe pairs the ConPTY path uses: the parent end is registered with the runtime's IOCP and the child end stays synchronous. On Unix, Tokio's child pipes are nonblocking descriptors and are used directly. `KillOnCloseJob::attach_spawned_or_kill` contains the child in its job object or process group, and the platform `output` helper runs short commands such as Git to completion on the same pipes.

`JsonLineProcess` runs its writer, stdout reader, stderr reader, and exit wait as tasks. The input queue wakes its writer through `Notify`, and a started batch is written in one piece, so cancellation still cannot split it. `shutdown` returns a future that owns everything it waits on, so a caller holds neither the process nor a lock across the wait. The Claude and Codex sessions, the shared Codex host, and `Backend` expose the same shape, and `Drop` starts the forced shutdown detached.

Converted on top of this:

- **Deadlines:** request deadlines wait with `sleep_until` and a `Notify`. Timeout and cancellation stay distinct.
- **Backend startup:** `Backend::spawn` is async. The shared Codex host slot waits through `Notify`, and a guard releases waiters if a startup is cancelled.
- **DeepSeek host:** its reader threads became tasks. The per-launch slot lock is a Tokio mutex held across startup, so tabs sharing a launch wait for one start, and the API login is awaited.
- **Title generation:** it runs as a task. Cancellation arrives as a message, so the interrupt, unsubscribe, and detach cleanup still runs.
- **Maintenance commands:** `run_bounded` keeps capped, redacted output and kills the owned tree on timeout. `ProviderMaintenance`, the update coordinator, and the app's update transaction await it.
- **Git:** queries await the platform `output` helper. The branch cache never holds its lock across a process.
- **Usage fetches:** they race their body against `FetchCancellation`, so a cancelled fetch stops at once rather than at the next polling tick. The Claude usage-panel fallback sleeps until the next scheduled keystroke or new output.
- **Progress monitor:** the Claude progress monitor runs as a task. Each 750 ms pass reads and parses the log as one blocking batch.
- **Plan restoration:** Codex plan restoration reads the log on the blocking pool.
- **DeepSeek teardown:** closing the downlink no longer waits for the reader task. Each delivery runs under a gate's lock, and closing takes the callback out under that lock, so no frame reaches the tab after `Drop` returns and the callback is already released.

## HTTP, IPC, and background writers

The updater's release check and package download, the Claude release check, and the Claude OAuth usage request use async `reqwest`, with a streamed byte cap on downloads. The `reqwest` blocking feature is no longer enabled. Hashing, unpacking, and installation stay blocking stages on the blocking pool, and Restart Manager queries stay on GPUI background work.

The single-instance IPC server accepts on a Tokio named pipe (Windows) or Unix listener. The sending side stays a short synchronous retry loop: the hook helper and secondary-instance startup send one bounded message and have no runtime.

Native notification calls go through one runtime task that runs them in submission order on the blocking pool, so a removal cannot overtake the show it withdraws. The input history writer keeps latest-snapshot coalescing and flush acknowledgment through a `Notify` and per-flush `oneshot`. Each save is one blocking batch, and `on_app_quit` awaits the exit-time flush.

## Deliberately synchronous

| Area | Reason |
| --- | --- |
| Session and PTY creation | `CreateProcess`, `forkpty`, and the Flatpak shell lookup run synchronously when a tab opens, so a launch failure is still a synchronous `Result`. Making creation async would let a tab appear before its shell exists. |
| Startup-only waits | Process-exit waits and macOS login-shell capture are bounded startup sequencing, not long-lived waits. |
| IPC client and Explorer extension | Standalone or native-host entry points that send one message and have no runtime. |
| VT trace writer | A persistent, high-rate blocking sink; see above. |
| GPUI UI work, rendering, and CPU preparation | They belong to GPUI's executors; Tokio tasks would not make them asynchronous. |
| Durable settings and team storage | Each write is a serialized lock/merge/replace unit; UI callers offload it whole when needed. |
| Build scripts | Build-time only. |

## Validation

The platform, terminal, agent, updater, and app test suites pass, including the real-ConPTY passthrough tests, job-object teardown awaited through `poll_shutdown`, Agent provider update and recovery with real child processes, Git branch reads, and an end-to-end IPC round trip through the synchronous client. The `conpty_typing` scenarios, which type during shrink/grow resizes and past the right edge, pass when launched with PowerShell 7. Under Windows PowerShell 5.1 they fail at setup because its PSReadLine lacks the options they configure.
