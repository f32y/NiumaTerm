## Context

See proposal.md — Why.

The Agent pane's transcript presentation lives in `crates/app/src/agent_pane/transcript/` and is written as `impl AgentPane`. Every renderer takes `cx: &mut Context<AgentPane>`, so its click handlers close over the pane itself. The state those renderers actually read is a narrow slice: the entry list, the three expansion sets, the virtualized-code cache, the row-spec snapshot, the list geometry (list state, measured font, measured width), the live-progress fields, the settled-turn accounting, and `cwd`/`kind` for link opening. All five click handlers do the same thing — toggle membership in an expansion set and notify.

A transcript row is `Entry { at, turn, item }`, where `item` is the backend-neutral `chat::Item`. Both providers can already produce that type for a child:

- Codex stores each descendant as its own thread. `thread/read` with `includeTurns` returns `thread.turns` in the same shape `thread/resume` returns, which the existing turn/item parser already consumes, and it does not resume or load the thread into this session.
- Claude Code emits the child's own assistant, reasoning, and tool traffic on the parent's live stream tagged with `parent_tool_use_id`, which the panel currently reduces to a single preview line before dropping. It persists that work elsewhere: not inside the parent transcript, but in one file per child under `<session-id>/subagents/`, where `agent-<id>.jsonl` holds the conversation and `agent-<id>.meta.json` names the `toolUseId` that launched it. Parent transcripts contain no sidechain records at all, so the live tag and the persisted link are different mechanisms for the same relationship.

Neither provider streams a descendant conversation to a client that has not subscribed to it, and this session subscribes only to its own thread. A child transcript is therefore a snapshot that is refreshed, not a live tail — except for Claude Code, whose linked activity already arrives on the parent's stream.

## Goals / Non-Goals

**Goals:**

- One transcript presentation used by both the parent conversation and a child detail view, so a change to either reaches both.
- Move presentation out of the Agent pane without changing how the parent conversation looks or behaves.
- Keep per-conversation view state (expansion, scroll, measured geometry) separate, so two conversations rendered by the same component do not share it.
- Load child transcripts without touching the parent session's thread, turn state, or transcript.
- Bound what a child transcript costs in memory.

**Non-Goals:**

- A live tail of a Codex descendant, which would require subscribing to that thread.
- Any child operation: input, approvals, interrupt, resume, stop, close.
- Rendering a child of a child as a nested transcript; depth remains presentational metadata.
- A second right-side column, a popout window, or a movable pane.
- Persisting detail-view state across application restarts.

## Decisions

### 1. Make the transcript its own view rather than a set of functions

`TranscriptView` becomes a gpui entity with its own `Render`, holding the entries, the three expansion sets, the virtual-code cache, the row-spec snapshot, and the list geometry. `AgentPane` holds one and renders it as a child; the `Background Tasks` detail view holds another.

The deciding constraint is the click handlers. They are `cx.listener` closures over expansion state, so whatever owns that state must be the entity the listener binds to. An entity gives that for free and also gives each conversation its own `ListState`, which caches measured row heights and cannot be shared between two conversations of different lengths.

Making the renderers generic over a host entity plus a projection function was rejected: the generic parameter spreads through every renderer and every row helper, and the host would still need to own a separate `ListState` per conversation, so the generic buys nothing the entity does not already give.

Leaving the renderers on `AgentPane` and having the detail view construct a hidden `AgentPane` was rejected because an `AgentPane` owns a backend process, a composer, and a session lifecycle, none of which a read-only child transcript should create.

### 2. Split by ownership, not by file

`AgentPane` keeps everything about conducting a conversation: the backend session, composer, approvals, queued commands, slash palette, settings, turn lifecycle, and recovery. `TranscriptView` keeps everything about displaying one: entries, row specs, expansion, virtualization, list geometry, and the live-progress row.

Fields that describe a turn's outcome — settled durations, output tokens, interrupted turns — move with the transcript, because only the fold header and interrupted row read them. `cwd` and provider kind are passed in as presentation inputs rather than owned, since they only affect link resolution and labelling.

The pane feeds the view by appending entries and setting live-progress state; the view derives its own row specs. This keeps the existing render-time diff of freshly built specs against the previous snapshot, which is what limits remeasuring to changed rows.

### 3. Read Codex descendants without resuming them

Opening a Codex child issues `thread/read` with `includeTurns` for that child's thread id and parses `thread.turns` with the same parser used for parent replay, so a restored child card cannot lose output or status relative to the parent.

`thread/resume` was rejected because it moves this session onto the child thread. Subscribing to the descendant was rejected because subscription is a side effect of starting, forking, or resuming a thread, all of which change what this session is pointed at.

Because there is no descendant stream, the detail view reloads when it opens and when that child's lifecycle state changes. It does not poll.

### 4. Retain Claude Code linked activity instead of discarding it

The Claude reducer already recognizes activity linked to a known launch through `parent_tool_use_id` and currently keeps only the latest preview. It will instead append the parsed item to that task's retained transcript, bounded per task, while continuing to keep that content out of the parent transcript.

History supplies the rest, but from the child's own file rather than the parent's. The task-history pass reads the session's `subagents/` directory, accepts a conversation only when its metadata names a launch the pass already collected, and replays that file with the same parser the parent transcript uses. A child's file is entirely sidechain records, so that parser takes a flag selecting which side to keep rather than growing a second implementation. Live and restored items merge by item id the same way the parent transcript merges a completed payload into a streamed one.

The CLI also writes each child's status back into the parent conversation as a synthesized `user` turn carrying a `<task-notification>` block, marked `origin.kind`. Those turns are addressed to the model, so transcript replay excludes them alongside the compaction summaries it already excludes; replaying one shows the user something they never typed.

Retention is bounded because a long-running child can emit an unbounded number of items. When the bound is reached the oldest items are dropped and the view reports that the beginning is not shown, rather than silently presenting a partial conversation as complete.

### 5. Keep detail navigation inside the panel

`RightPanelKind` stays a two-way selection. The `Background Tasks` view gains its own mode — list or detail for one child key — so the shared right-side host never learns what a task is. Opening a child records the list's scroll position and section expansion so returning restores them.

Adding a third `RightPanelKind` was rejected: the host would then need to know which child is selected and when that selection becomes invalid, which is exactly the state the panel already owns.

### 6. Track transcript loading separately from row data

Child transcript loading gets its own not-loaded/loading/ready/unavailable state on the common task model, beside the existing discovery state. A failure leaves the row's status details visible and reports only that the conversation could not be read, so a provider problem never blanks a child that the panel otherwise knows about.

### 7. Reuse the existing parent-session scoping

The detail view is keyed by parent session and child key. The panel already hides a snapshot that does not describe the pane's current session; the detail view uses the same check and falls back to the list when the child is no longer part of the active session's snapshot.

## Risks / Trade-offs

- [Moving presentation regresses the parent transcript] → Move it with no behavior change and keep the existing transcript tests passing against the extracted view before any child rendering is added.
- [A child transcript grows without bound] → Bound retained items per task, drop oldest first, and state in the view that earlier content is not shown.
- [Codex child content is stale while the child works] → Reload on open and on lifecycle change, and show the child's lifecycle state and timing from the live summary so the header is never stale even when the body is.
- [Retaining Claude linked activity leaks child content into the parent] → Retention happens in the reducer that already runs before parent filtering; the parent path continues to drop those records unchanged, and the existing isolation tests stay in place.
- [Two conversations share expansion or scroll state] → State lives in the per-conversation view, and switching the detail target replaces the view's state rather than reusing it.
- [Fold headers depend on turn accounting a child does not report] → A child transcript without settled-turn accounting renders its rows without fold headers rather than fabricating durations.

## Migration Plan

1. Extract `TranscriptView` and move transcript state and renderers into it, with the Agent pane delegating and its existing tests unchanged.
2. Add transcript loading state and per-task retained items to the common task model.
3. Add Codex descendant reading and Claude Code linked-activity retention plus child history reconstruction.
4. Add row activation, the detail mode, and back navigation to the panel, rendering the child through the extracted view.
5. Validate both providers in isolated `--testing` launches.

Rollback removes the detail mode and the provider transcript loading; the extracted transcript view can stay, since it changes no behavior on its own.
