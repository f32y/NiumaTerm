## 1. Extract the Transcript View

- [x] 1.1 Add a `TranscriptView` entity holding entries, the three expansion sets, the virtual-code cache, the row-spec snapshot, and list geometry, with presentation inputs for working directory and provider kind.
- [x] 1.2 Move the row renderers, disclosure rows, formatting helpers, and virtualized-code handling onto that entity so its click handlers bind to it rather than to the Agent pane.
- [x] 1.3 Move settled-turn durations, turn output tokens, and interrupted turns onto the view, and render fold headers only when that accounting exists.
- [x] 1.4 Give the Agent pane a `TranscriptView` and delegate appending entries, live-progress state, replay, clearing, and scrolling to it.
- [x] 1.5 Move the existing transcript tests onto the extracted view and confirm they pass unchanged.
- [x] 1.6 Add tests proving two views keep separate expansion, scroll, and measured geometry.

## 2. Common Child Transcript Model

- [x] 2.1 Add a per-child transcript record to `agent_utils` holding backend-neutral items, a load state of not loaded, loading, ready, or unavailable, and a marker for content dropped at the retention bound.
- [x] 2.2 Add a bounded append that merges an item by id and drops oldest items first when the bound is reached.
- [x] 2.3 Add a session event carrying a child transcript update, keyed by provider-qualified child key.
- [x] 2.4 Add unit tests for merging a completed item into a streamed one, bounded eviction, the dropped-content marker, and load-state transitions.

## 3. Codex Child Transcripts

- [x] 3.1 Add a dynamically tracked `thread/read` request with `includeTurns` for a confirmed descendant, parsing its turns with the existing turn and item parser.
- [x] 3.2 Route the response into the child's transcript record without changing the parent thread ID, turn state, approval state, or transcript.
- [x] 3.3 Trigger a read when a child's detail view opens and when that child's lifecycle state changes, and guard against overlapping reads for the same child.
- [x] 3.4 Report unavailable on a failed read while keeping the child's existing summary and any previously loaded items.
- [x] 3.5 Add tests for descendant reading, parent isolation during a read, refresh on lifecycle change, and read failure.

## 4. Claude Code Child Transcripts

- [x] 4.1 Parse linked sidechain assistant, reasoning, and tool activity into transcript items instead of only a preview line, keeping that content out of the parent transcript.
- [x] 4.2 Append parsed items to the owning task's bounded transcript as they arrive live.
- [x] 4.3 Extend the task-history pass to reconstruct a child's items from the sidechain records linked to a selected launch.
- [x] 4.4 Merge restored child items with live ones behind a starting sequence so older history cannot replace newer live content.
- [x] 4.5 Add tests for live linked activity, restored child conversations, unlinked sidechains creating nothing, retention bounds, and continued parent transcript isolation.

## 5. Agent Pane Projection

- [x] 5.1 Store child transcripts and their load states in the Agent pane session beside the existing task snapshot.
- [x] 5.2 Apply child transcript events without changing parent composer, transcript, approval, queued-command, or running-state behavior.
- [x] 5.3 Expose read-only access to one child's transcript and a request to load it, scoped to the active parent session.
- [x] 5.4 Add Agent pane tests for transcript replacement, retained items after a failed load, and clearing when the pane starts or restores a different session.

## 6. Panel Detail View

- [x] 6.1 Make task rows activatable and add a list-or-detail mode to the `Background Tasks` view keyed by child key.
- [x] 6.2 Add a detail header showing provider, name, lifecycle state, and timing from the live summary, plus a back control that restores the list's scroll position and section expansion.
- [x] 6.3 Render the child transcript through the extracted transcript view, with its own expansion and scroll state per child.
- [x] 6.4 Show loading and unavailable transcript states without hiding the child's status details, and show the dropped-content marker when retention truncated the beginning.
- [x] 6.5 Return to the list when the shown child leaves the active session's snapshot, and close the view when the active pane stops being a supported provider session.
- [x] 6.6 Replace the hover-only row summary with an activatable row presentation, keeping accessible labels for state, timing, and truncated text.
- [x] 6.7 Add view tests for opening and returning, per-child state separation, both providers, loading and unavailable states, session switching, and the absence of child operation controls.

## 7. Validation

- [x] 7.1 Run formatting, workspace lint checks, and focused Rust tests for the transcript view, common transcript model, both provider adapters, Agent pane, and the panel.
- [x] 7.2 Launch `target\debug\NiumaTerm.exe --testing` and confirm the parent Agent transcript is unchanged after the extraction, covering folding, disclosure expansion, virtualized output, selection, copying, and jump-to-latest.
- [x] 7.3 Manually validate opening a Codex child, its transcript matching Agent pane presentation, refresh on completion, and read failure.
- [x] 7.4 Manually validate opening a Claude Code child, live linked activity extending its transcript, a restored child from history, and parent transcript isolation.
- [x] 7.5 Manually validate back navigation, per-child state, parent-session switching, Git replacement, and that no child operation control is reachable.
