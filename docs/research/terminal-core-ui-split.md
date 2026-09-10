# Terminal Core and UI Separation

Date: 2026-09-10

Status: Implemented in the working tree; automated validation completed.
Manual window checks, relay-backed remote validation, and broader performance
measurements remain outstanding.

## Objective

Move terminal runtime and interaction rules from `nmt_terminal_ui` into
`nmt_terminal`, while retaining presentation, GPUI integration, and window
resources in `nmt_terminal_ui`. The core must remain usable without a window.

The unit of migration is a responsibility, not a directory or any function
that happens to have no GPUI imports. Layout calculations, display ordering,
animation timing, and rendering caches remain UI responsibilities.

This document covers the terminal split only. Agent separation is a distinct
change. The design discussion below records the original plan; the
implementation results describe the decisions now present in the code.

## Implementation Results

### Ownership and interfaces

- `nmt_terminal::session::TerminalSession` now owns local PTY startup,
  process management, shutdown-on-drop, input acceptance, engine scrolling,
  selection, alternate-screen state, and host-event publication.
- `TerminalSession::from_pty` accepts an existing `EventedPty` and
  `SessionOptions`. Event routing uses the options' route ID so there is
  no separate ID that can disagree with the worker.
- `SessionObserver` receives graphics, block lifecycle, clipboard requests,
  and content/host-event notifications synchronously. UI image caches
  implement it; the core has no presentation image store.
- Selection is invalidated directly during command completion and exit.
  Exit and alternate-screen state use atomics. Working-directory restoration
  queries the engine immediately, without requiring host-event consumption.
  Windows drive-qualified OSC 7 paths retain their filesystem form.
- `write_input`, `write_text`, `paste_text`, and `resize` report local
  queue acceptance. Input is rejected after exit or in read-only mode.
  Acceptance does not imply delivery to a remote peer.
- `with_render_buffer` and `with_screen_reader` batch read-only access
  under separate locks. Callbacks cannot reenter the session. Finished block
  references are acquired briefly and then read independently.
- `TerminalSurface` remains a small UI coordinator for frame extraction,
  graphics, clipboard policy, initial/restored tab settings, and pixel-to-grid
  resize. It no longer forwards all terminal operations.
- `NetPty` is now in `nmt_remote_net::net_pty`. Remote clients still use
  terminal responses and do not use local engine-block mode. The remote host
  continues using the lower-level driver with its existing options.
- `nmt_terminal_ui::session` re-exports the core module for existing app
  imports. This does not introduce a core dependency on the UI.

### Image release and update order

The PTY path stages block events, delivers graphics updates, updates the UI
frozen-image cache, publishes blocks, and then sends the final content
notification. The earlier graphics content notification remains unchanged.
Image references are cloned before acquiring the render-buffer lock.

The first pane render attaches its image-release queue to a task associated
with the original window. That task holds neither the pane nor image
generations. It releases atlas images independently of content rendering,
including when the last live image is removed, frozen history is evicted,
or a displayed frame outlives its pane. A closed window ends the task.
No-image frame extraction still skips the live generation store.

### Automated validation

On Windows, the following completed successfully:

- `nmt_terminal`: 160 tests passed, including real local PTY output/title,
  shell integration, alternate-screen recovery and post-reset output.
- `nmt_terminal_ui`: 102 tests passed; the relay-dependent remote test and
  two manual performance profiles were excluded from the default run. Both
  profiles passed when run explicitly in Release as described below.
- `nmt_remote_net`: 18 tests passed, including the moved adapter's partial
  read, readiness reset, and bounded-buffer scenarios.
- Strict first-party Clippy groups passed for all targets of these three
  crates, including absolute paths, complexity, performance, and style.
- Dependency inspection found no GPUI, terminal UI, or remote networking
  dependency in the terminal core's normal dependency tree.
- The application's all-targets check passed with the new crate boundaries.

New scenarios verify input rejection after exit/read-only/queue closure,
runtime updates before event draining, normalized working-directory reads,
callback publication order, final-image removal, frozen eviction, and delayed
release after pane-owned stores are gone. Existing selection, paste, mouse,
frame-cache, and wake-coalescing tests remain with their owning layer.

### Text pipeline measurements

The initial Debug comparison reported a 12.5% increase in single-row
extraction (28.457 to 32.009 us). That observation is withdrawn as evidence
of a code regression. This profile was documented for Release use, but
previously did not reject Debug execution.

Re-running the same unmodified executables reversed the result: four
alternating runs produced 28.350-29.067 us before and 21.959-22.523 us after.
Copying each executable to two files, checking equal SHA-256 values within
each version, and pinning execution to logical CPU 0 also changed the
timings: the old copies measured 28.488 and 24.824 us, while the new copies
measured 23.196 and 28.247 us. Subsequent inspection confirmed different
image load addresses for the identical copies. This demonstrates that
these short Debug timings depend on process placement/execution conditions;
it does not identify a specific CPU-cache mechanism.

The frame extraction, render-buffer, and engine algorithms did not change
in the split. The original micro-profile also bypasses the session observer,
so its percentage could not by itself establish observer overhead.

#### Controlled Release comparison

Both versions were built with the same repository Release profile and
Rust compiler. The pre-split source was extracted from HEAD into a separate
diagnostic directory, preserving the active working tree. Both received
identical benchmark changes:

- Reject Debug execution with an explicit error.
- Warm up 256 incremental frames before recording measurements.
- Keep inputs and outputs observable through `black_box`.
- Verify that every incremental update replaces row 0 and reuses all other
  23 line allocations, outside the timed interval.
- Add a real-PTY surface profile: a `cmd.exe` process emits 1,000 lines and
  a marker, then exits; the profile waits for exit and verifies the marker
  before timing `TerminalSurface::frame`.
- Verify that the quiet-surface path retains all 24 line allocations.

The benchmarks ran serially on logical CPU 0, alternating version order,
with five samples per version and no concurrent build. Results are medians:

| Measurement | Before | After |
| --- | --- | --- |
| Viewport capture per frame | 38.446 us | 38.892 us |
| Full frame extraction | 44.040 us | 42.934 us |
| Single-row incremental extraction | 2.767 us | 2.722 us |
| Surface full extraction | 43.356 us | 42.169 us |
| Surface clean-frame reuse | 0.741 us | 0.741 us |

The single-row samples ranged from 2.673 to 2.880 us before and 2.696 to
2.745 us after. No increase was reproduced in incremental extraction or
the directly measured surface paths. These measurements do not establish
performance for image workloads, GPU cleanup, or remote transport.

Reproduce the guarded benchmarks with:

```text
cargo test --release -p nmt_terminal_ui profile_ -- --ignored --nocapture --test-threads=1
```

Use matched before/after builds, pin both processes to the same CPU, and
alternate multiple runs; do not compare a single Debug sample or run the
two profiles concurrently.

### Remaining validation

- Exercise quiet windows and inactive tabs with cursor blinking disabled;
  the GPUI unit harness does not model the actual frame pump.
- Run the relay-backed remote session test with its required service.
  Adapter tests alone do not establish remote end-to-end behavior.
- Collect before/after throughput, frame time, memory, and lock-wait data for
  sustained text, long history, image replacement, and frozen eviction.
  Passing functional tests does not establish unchanged performance.
- Manually exercise window atlas cleanup. The new queue tests establish
  delivery and ownership behavior, not GPU memory reclamation measurements.

## Pre-migration Structure

| Location | Current responsibilities |
| --- | --- |
| `nmt_terminal::pty_pipe` | PTY worker, VT parsing, engine state, viewport snapshot publication |
| `nmt_terminal_ui::session` | Startup, process management, host events, command lifecycle, GPUI image stores |
| `nmt_terminal_ui::surface` | Input, paste, mouse reports, selection, scrolling, frame construction, restored tab state |
| `nmt_terminal_ui::view` | GPUI integration, presentation, and some runtime state transitions |

Relevant source:

- [PTY construction](../../crates/terminal/src/pty_pipe/session.rs)
- [PTY worker](../../crates/terminal/src/pty_pipe/mod.rs)
- [Terminal session](../../crates/terminal/src/session/mod.rs)
- [Session event proxy](../../crates/terminal/src/session/proxy.rs)
- [Terminal surface](../../crates/terminal_ui/src/surface/mod.rs)
- [UI host-event handling](../../crates/terminal_ui/src/view/events.rs)

Before migration, some runtime transitions depended on the UI draining host events:

- Exit marks the surface read-only.
- Alternate-screen events update a separate surface flag.
- Command completion clears the active-screen selection.
- Working-directory events update a surface cache used for restoration.

The proposed core owns these transitions. Host events describe changes that
have already happened; consuming them must not be necessary to make the
terminal state correct. UI consumers still update titles, notifications,
display caches, and widgets.

## Proposed Ownership

```text
nmt_terminal
  session/       Runtime, lifecycle, state, events, launch options
  selection/     Terminal-coordinate selection and text extraction
  pty_pipe/      Existing worker and engine driver
  block_store    Existing command-block history
  ghostty/       Existing engine interface

nmt_terminal_ui
  view/          GPUI input conversion, focus, window interaction
  frame/         Display lines, styles, incremental frame cache
  block_list/    Block layout, visibility, scroll anchoring
  graphics/      RenderImage, atlas upload and release
  wake/          Repaint coalescing and GPUI scheduling

nmt_remote_net
  net_pty/       Remote connection adapted to EventedPty
```

| Existing code | Proposed disposition |
| --- | --- |
| Session creation, shutdown, process counts, writes, resize | Move into the core session |
| Shell arguments and prompt integration | Move into core launch preparation |
| Conversion to `TabState`, retrying a restored tab without its cwd | Keep in application/UI assembly |
| Read-only checks and paste text preparation | Move into the core |
| Terminal mouse modes and report selection | Move into the core; reuse `nmt_input` encoding |
| Terminal-coordinate selection and text extraction | Extend the existing core selection module |
| Engine viewport scrolling | Move into the core |
| Pixel-to-cell conversion, scroll animation, block-list scrolling | Keep in UI |
| Engine and block reads | Expose focused core read operations |
| Display-line construction and GPUI image conversion | Keep in UI |
| Frame styling, shaping, and incremental display caches | Keep in UI |
| Remote PTY adapter | Move into `nmt_remote_net` |

Do not move `TerminalSurface` wholesale. Absorb its runtime responsibilities
into the core session, then remove it or retain only a cohesive presentation
role. Do not preserve two independently updated copies of runtime state.

## Image Ownership and Publication

### Existing behavior

The [graphics module](../../crates/terminal_ui/src/graphics/mod.rs) owns:

- CPU-side `RenderImage` construction from decoded pixels.
- Replacement of the live image generation under an image ID.
- Retention of earlier generations by frames through `Arc`.
- Deferred atlas release when the last uploaded generation reference drops.
- Lazy frozen-block image conversion and caching.

These are presentation responsibilities. The core already owns image data
through Ghostty; a second generic pixel cache is not automatically needed.

### Alternatives

| Approach | Tradeoff | Recommendation |
| --- | --- | --- |
| Add a core pixel cache and a separate UI image cache | Adds identity, cleanup, and possibly copying across two caches | Do not introduce in the initial split |
| Queue all image and block updates for the UI thread | Changes timing and execution cost; inactive panes may accumulate work | Do not use for the initial split |
| Synchronously notify a UI-owned image observer | Preserves worker-side processing and current resource ownership | Preferred direction |

The core observer interface should carry framework-independent updates:

- Image additions, replacements, and removals.
- Committed block-history changes, eviction, and clearing.
- Content-change and host-event availability notifications.

The UI implementation owns its caches. A consumer without graphical output
can ignore presentation updates. Do not expose windows, `RenderImage`, atlas
operations, or widgets in the core interface.

Before implementation, select the smallest observer shape that fits existing
`EventListener` usage. Do not add a general event bus or introduce an async
runtime solely for this split.

### Thread and ordering requirements

Observer calls can run synchronously on the PTY worker. They must not access
windows, wait for the UI thread, call back into the session, or acquire the
engine lock. Some existing terminal events originate while the engine is
locked; reentrant session access would be unsafe.

The current publication sequence includes:

1. Stage command-block events produced during parsing.
2. Extract a viewport snapshot and image changes.
3. Install image changes after releasing the engine lock.
4. Publish the render buffer.
5. On terminal damage, prune frozen presentation resources and commit staged
   block history before issuing the corresponding content notification.

Image updates also issue an earlier content notification today. Therefore,
the existing behavior does **not** guarantee that every notification marks
a fully committed batch. Preserve that distinction during migration. Removing
the earlier notification is a separate optimization requiring validation.

The essential guarantee is that image processing and staged block publication
have completed before the final damage notification. Do not move per-read
image or block traffic into the host-event queue.

### Release scheduling concern

The [render path](../../crates/terminal_ui/src/view/mod.rs) currently drains
atlas releases only when live images exist. After the final live image is
removed, pending releases may still exist. This is a static-review concern,
not a reproduced failure.

The proposed design should distinguish live-image availability from pending
resource releases. Validate final-image removal, frozen-only images, inactive
panes, and pane teardown before deciding how release work wakes the UI.

## Core Session Interface

Build on the existing `start_session` and PTY worker rather than adding a
second engine loop. Organize the public surface around four responsibilities:

| Responsibility | Operations |
| --- | --- |
| Construction and lifetime | Local launch, construction from `EventedPty`, shutdown, process state |
| Commands | Write, paste, resize, terminal mouse input, viewport scroll |
| Reads | Runtime state, viewport data, acquired blocks, selection, extracted text |
| Notifications | Host-event draining and content-change notification |

### Command results

`TerminalSession::write_input` currently discards the channel send result,
while the surface can subsequently report success. The proposed interface
must return what happened: accepted into the local queue, rejected by input
policy, exited, or channel closed, as appropriate.

Local queue acceptance does not prove receipt by a remote process. The remote
adapter currently forwards input without delivery acknowledgement. Do not
invent stronger delivery guarantees in the core result type.

UI feedback, scrolling, focus changes, and repaint requests follow these
results in the UI layer.

### Reads and locking

Expose focused batch reads such as render-buffer access, acquired blocks,
and row ranges. Avoid exporting every internal lock or adding a call per cell.

Preserve existing constraints:

- Engine and render-buffer locks remain separate.
- Clone live image references before locking the render buffer.
- Never acquire the engine while holding the block-store lock; the PTY path
  can acquire them in the opposite order.
- Acquire a finished block briefly under the engine lock, then use the
  existing block reference for subsequent reads.
- Observer callbacks must not reenter session reads or commands.

Do not combine all session state under a single large mutex merely to make
ownership look uniform.

### Selection and input

Keep viewport, absolute screen, and frozen-block coordinates distinct. UI
maps pixels through the currently displayed frame; the core interprets
terminal coordinates and extracts text.

The [frame cache](../../crates/terminal_ui/src/frame/cache.rs) deliberately
retains a stale displayed frame for pointer and IME mapping until its
replacement is rendered. This behavior must remain in UI.

When command completion changes active-screen ownership, the core invalidates
the corresponding selection independently of UI event draining. A content
generation attached to selection state is one candidate; its implementation
must be checked against existing lock order and command publication timing.

Reuse `nmt_input` rather than duplicating terminal encoding. Moving current
input rules into the core may add a direct `nmt_input` dependency; it has no
dependency on `nmt_terminal` today.

### Clipboard and restoration

Core paste accepts text. Core selection exposes text. The UI owns desktop
clipboard access and copy-first keyboard policy. Terminal-originated clipboard
requests should use an explicit host operation that does not require a window.
Choose its dispatch timing deliberately; do not silently move blocking desktop
work into a new thread context.

Core launch options should not contain `TabState` or GPUI metrics. The caller
supplies initial dimensions, colors, and runtime settings. The application
preserves the original user launch command for restoration before shell
integration adds arguments, environment entries, or bootstrap input.

## Remote PTY Placement

Current Windows dependencies include:

```text
nmt_remote_net -> nmt_remote_session_hub -> nmt_terminal
```

Moving `NetPty` into `nmt_terminal` while retaining its dependency on
`nmt_remote_net` would create a cycle. Move the adapter into `nmt_remote_net`
and pass its existing `EventedPty` implementation to the core constructor.

Preserve the distinction between:

- Remote client: parses the received VT stream and answers terminal queries.
- Remote host: owns the shell and output stream; terminal query responses are
  disabled so host and client do not send duplicate answers.

See [host construction](../../crates/remote_session_hub/src/windows.rs).
Do not flatten these differences into one set of default creation options.

The remote hub also owns subscriptions, checkpoints, and output broadcasting.
It may continue to use the existing lower-level `start_session` during the
initial migration. Adopting the new higher-level session there is not required
to separate the GUI code.

## Migration Plan

| Batch | Change | Validation focus |
| --- | --- | --- |
| 1 | Separate image-cache ownership through the synchronous observer | Replacement, frozen cleanup, ordering, no-image path |
| 2 | Move session startup, lifetime, event interpretation, and runtime state | Exit and input policy without UI event consumption |
| 3 | Move terminal input, viewport scrolling, selection, and focused reads | Paste, mouse modes, soft wraps, scroll-relative copy |
| 4 | Move `NetPty` and reconnect remote creation | Initial snapshot, live output, input, resize, disconnect, idle CPU |
| 5 | Remove redundant UI forwarding and runtime state | Window behavior, inactive panes, quiet-window wakeups |

Each batch should preserve a working application. Use `git mv` for cohesive
module moves, keep imports rooted at the owning crate, and move implementation
tests with their responsibilities. Do not widen all internal visibility merely
to preserve old UI access patterns.

## Validation Plan

Existing tests provide a starting point:

- [Session tests](../../crates/terminal/src/session/tests.rs): launch
  integration, host-event mapping, command state, block metadata, image routing,
  sustained output, and remote PTY operation.
- [Image tests](../../crates/terminal_ui/src/graphics/tests.rs): replacement,
  invalid data, ID reuse, release behavior, and per-session isolation.
- [Selection tests](../../crates/terminal/src/session/selection/tests.rs).
- [Wake tests](../../crates/terminal_ui/src/wake/tests.rs).

Strengthen coverage at the new public behavior rather than duplicating tests
for forwarding methods. Explicit scenarios include:

- Exit rejects subsequent input without a UI event drain.
- Alternate-screen state and selection invalidation update without a render.
- Writes distinguish local acceptance from a closed queue.
- Final damage notification observes committed image and block updates.
- Sustained output does not turn graphics or block changes into an unbounded
  UI-facing queue.
- Final live-image deletion and frozen-history eviction release uploaded
  resources exactly once.
- Clipboard paste preserves newline and bracketed-paste handling.
- Selection remains correct across soft wraps, scrolling, and block changes.
- Remote initial snapshot precedes subsequent output; disconnect ends the
  session without an idle busy loop.

Compare before and after measurements for no-image sustained output, long
scrollback, image replacement/removal, and frozen image eviction. Examine
throughput, frame time, memory, lock waits, and pixel copies. Do not assume
unchanged performance from passing functional tests.

Manual launches must pass `--testing`. Validate quiet windows without an
unrelated animation or cursor blink keeping the frame pump awake. GPUI unit
tests do not model that pump. Content wake coalescing must not suppress host
events for inactive panes, and notifications from outside a frame must still
cause the required GPUI repaint request.

## Completion Criteria

- `nmt_terminal` has no new GPUI dependency and no dependency on UI or remote
  networking crates.
- Runtime state is correct without a window or UI event consumption.
- Core operations return domain results; UI owns user-visible reactions.
- Image presentation resources stay in UI without adding per-frame copies or
  a second generic image store.
- Local and remote PTYs share the existing engine driver while retaining
  their different terminal-response settings.
- The UI no longer owns duplicate terminal lifecycle and input-policy state.
- Existing behavior and relevant performance paths have been validated.

## Decisions Still Requiring Implementation-Level Review

- Exact observer shape and how much existing `EventListener` machinery to reuse.
- Selection invalidation mechanism and its synchronization with block publication.
- Runtime-state snapshot shape and event-consumer ownership.
- Host clipboard dispatch and pending atlas-release wake scheduling.
- Whether any small presentation coordinator remains after dismantling the
  current `TerminalSurface`.

These details should be settled against concrete call sites during each
batch, without expanding the effort into a replacement terminal engine or
a new application event framework.
