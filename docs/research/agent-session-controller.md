# Agent Session Controller

Updated: 2026-09-11

`nmt_agent::session::controller::SessionController` owns the provider runtime,
message delivery, conversation restore, naming, settings, input requests, branch
operations, command queue, child-agent state, and workflow data. `AgentPane`
owns one controller; its presentation helpers borrow the corresponding state.

The controller combines operations that must advance multiple state objects:
submission admission, startup and installation, turn boundaries, interruption,
approval and question responses, readiness, replay selection, failure cleanup,
and conversation reset. `apply_event` checks the session generation before
changing state and returns a `SessionEffect` for the view to present immediately.
Effects carry owned provider payloads without copying a complete transcript.

Readiness has two steps. The controller first selects branch or restored replay
and retires the old conversation state. The host consumes that result, then calls
`finish_ready` with profile and remembered settings. That completes question
restoration, model selection, pending naming, and runtime readiness. An ignored
event does not complete a pending conversation change.

## UI responsibilities

`AgentPane` still hosts asynchronous startup, event batching, filesystem work,
and timers through GPUI. Startup scheduling now lives in `session/startup.rs`.
Editors, focus, notifications, command discovery, branch picker drafts, workflow
visibility, and rendering remain in the UI. The controller returns domain
outcomes; the view decides which messages, caches, and widgets to update.

`TranscriptView` continues to own its existing content model once. Grouping,
folding, virtualization, height caches, typewriter presentation, and scrolling
are outside the session controller.

## Remaining work

- Give sessions a lifetime and registry independent of `AgentPane`, then move
  application-level update and background-task consumers to session handles.
- Move remaining executor scheduling and host policy out of pane methods as
  those consumers are introduced. The controller is currently an owned Rust
  value, not a separately scheduled application entity.
- Review child-agent and workflow detail transcript ownership and update paths
  before sharing the same session across multiple views.

No application launch, automated test execution, or performance measurement was
performed for this change. Existing test sources were adapted to the new owner
and event entry points and included in compilation checks.
