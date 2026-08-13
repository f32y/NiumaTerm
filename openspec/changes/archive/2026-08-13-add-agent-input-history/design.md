## Context

See `proposal.md` for motivation and `specs/agent-input-history/spec.md` for observable behavior.

Agent composer submission currently has two accepted paths. Ordinary input calls
`send_text_with_skill`, whose boolean result covers both a newly started turn and accepted
steering. Slash input returns `true` only when a local action, queued command, or provider
command was accepted. Both paths clear the input only after that result, so the same boundary
can record history without changing provider adapters.

Up and Down already enter `AgentPane::handle_palette_control`. That handler gives visible
command, skill, rewind, and recent-session views first choice, then propagates the action to
the input editor. Input history must fit between those two stages. `InputState` exposes the
current text, selection, and UTF-8 cursor offset, plus full-text replacement and explicit
selection placement, so this change does not need a component-library edit.

NiumaTerm currently saves window and Agent defaults in `local_state.toml` mainly during
shutdown. Prompt history changes after each accepted input and contains a different kind of
user data, so it needs independent storage and an in-process owner shared by all Agent Tabs.
Agent Tabs currently run on the local machine, but the persisted key must include a stable
target identifier so a later remote Agent path cannot mix entries with local history.

## Goals / Non-Goals

**Goals:**

- Keep history mutation and retention rules in one reusable model that can be tested without a
  running provider.
- Share newly accepted entries immediately across Agent Tabs in the same scope.
- Keep per-pane browsing state small and independent, so navigating one composer cannot move
  another composer's position.
- Preserve existing editor movement whenever input history declines an action.
- Keep accepted Agent actions responsive when loading or saving history fails.

**Non-Goals:**

- Reading or updating Claude or Codex private history files.
- Reconstructing attachments, skill paths, provider handles, or other submission metadata.
- Adding a history browser, search, deletion control, or configurable retention limit.
- Adding history to ordinary terminal panes.
- Adding remote Agent execution as part of this change.

## Decisions

### 1. Use a dedicated application-global history service

Add an `AgentInputHistory` GPUI global initialized before windows open. It owns the loaded
history model and the save queue. Every Agent Tab derives its scope and reads or records
through this global, while the pane owns only its current browsing cursor.

This avoids duplicate file loads and lost updates when two tabs submit close together. Storing
the data on each pane was rejected because panes in the same scope would diverge. Extending
`LocalState` was rejected because that file follows window-lifecycle writes and would combine
prompt data with session restoration data.

The service will live in a new `agent_pane/input_history` module. Pure storage and navigation
types stay separate from GPUI calls, with tests in the module's child test file.

### 2. Key entries by target, backend, and normalized working directory

Define a serializable scope with three fields:

- `target`: a stable string identifier; current local Agent Tabs use `local`.
- `backend`: the stable Claude or Codex identifier, not a profile display name.
- `cwd`: the tab's effective working directory normalized once when the pane is created.

When a tab has no explicit directory, normalization resolves the process working directory.
The implementation first makes the path absolute, applies platform path normalization, and
uses filesystem canonicalization when it succeeds. A lexical absolute fallback keeps history
usable when the directory no longer exists or cannot be inspected.

Profile name and executable path are excluded. Users commonly create multiple profiles for
one backend and expect their prompts to remain available within the same workspace. A single
global list was rejected because it would expose unrelated prompts across projects and
backends.

The model API accepts a target identifier even though the first caller always passes `local`.
This lets isolation tests cover the required key now and prevents a data-format change when
remote Agent execution is introduced.

### 3. Record at the composer acceptance boundary

Ordinary input is recorded only when `send_text_with_skill` returns `true`. This includes both
new turns and accepted steering. Slash input is recorded only when `submit_slash_input` returns
`true`. Recording stays in `send_user_message` and `submit_current_slash`, after the accepted
result and before the input is cleared.

The stored string is the same trimmed text accepted by the composer. Calls to lower-level
`send_text` from UI-generated actions are not recorded, because they were not typed and
accepted from the composer.

Keeping history at this boundary avoids provider-specific behavior and naturally excludes
validation errors, unavailable sessions, and failed slash dispatch. Recording on Enter before
dispatch was rejected because it would retain input that remained visible for correction.

### 4. Keep shared entries and per-pane navigation state separate

Each pane adds an `InputHistoryNavigation` value containing an optional entry snapshot, entry
index, and last recalled text. The shared global holds entries oldest to newest. Starting
browsing captures the latest entries for the pane's scope, so submissions from another tab are
available the next time an empty composer enters browsing. An active traversal keeps its
snapshot until it ends, preventing retention or a concurrent submission from shifting its
index midway through navigation.

The handler accepts Up or Down only when:

1. the higher-priority palette and recent-session handlers declined it;
2. the selection is collapsed;
3. input is empty when starting browsing, or exactly matches the active recalled text; and
4. while browsing, the cursor is at byte offset zero or the full text length.

An accepted recall clears the pane's structured skill binding, marks automatic palette display
as dismissed, uses `InputState::set_value`, then selects `len..len` so multi-line input also
places the cursor at the end. `set_value` intentionally avoids restoring the prior undo stack.
Dismissing automatic palette display lets consecutive history navigation continue when a
recalled entry starts with `/`; editing the recalled text runs the existing change path and
allows the relevant palette to appear again. Moving Down past the newest item clears the input
and resets navigation. Up at the oldest item consumes the action without changing text. If the
input changes, the existing `InputEvent::Change` path resets navigation before later Up or
Down actions are evaluated.

History navigation returns a small handled-or-declined result. The view stops propagation
only for handled actions; otherwise the input editor keeps its current cursor movement. A
keybinding directly on `InputState` was rejected because it would run without the pane's
palette priority and scope state.

### 5. Persist a bounded, versioned JSON file

Store history at `nmt_config::config_dir_path()/agent-input-history.json`. The top-level value
contains a format version and a list of scope records. Each scope stores at most 100 text
entries in oldest-to-newest order. Recording text equal to the current newest entry is a
no-op; otherwise it appends and removes oldest entries beyond the limit.

The global updates memory before requesting a save. A single background save worker processes
snapshots in order and may merge queued requests by retaining the newest snapshot. Each save
uses a temporary file in the same directory followed by atomic replacement. On clean shutdown,
the latest in-memory snapshot is flushed once so a pending background save is not lost.

Loading a missing file produces empty history. Invalid or unsupported JSON is logged and also
produces empty in-memory history; the next accepted entry writes a valid current-version file.
A save error is logged but does not roll back memory or alter the accepted Agent action.

For `--testing`, initialization uses a process-specific temporary path. Unit tests inject their
own temporary path and never read a developer's normal history file.

Synchronous writes on every submission were rejected because a slow profile directory would
block the UI thread. A database was rejected because the retained dataset and access pattern
are small enough for a versioned JSON snapshot.

### 6. Validate behavior at model and pane routing levels

Pure tests cover scope isolation, adjacent duplicate handling, the 100-entry limit, file
round trips, invalid input files, and all navigation transitions. Agent pane tests cover the
acceptance boundary, palette and recent-session priority, draft preservation, editor fallback,
cursor placement, and clearing after the newest item. Test helpers inject the history global
and storage path so provider processes and the user's own data are not involved.

## Risks / Trade-offs

- [Prompt text is sensitive local data] -> Keep it in the existing per-user configuration
  directory, never send it to providers as history metadata, and document the dedicated file
  location in user-facing release notes.
- [A forced process exit can occur before the background worker saves] -> Save after every
  accepted entry, coalesce only pending snapshots, and flush the newest snapshot during clean
  shutdown. A hard exit may still lose only the most recent unsaved additions.
- [Path normalization can fail for deleted or inaccessible directories] -> Use a deterministic
  lexical absolute fallback and test both paths.
- [The number of distinct scopes can grow over time] -> Bound entries within every scope now;
  defer whole-file aging until real usage shows a need, since silent scope removal would add a
  new user-visible retention rule.
- [Invalid JSON is replaced after the next accepted input] -> Log the parse error before using
  empty memory so diagnosis remains possible without preventing future saves.

## Migration Plan

1. Add the new module, versioned file model, global initialization, and process-specific test
   path. A missing file requires no migration and loads as empty history.
2. Add per-pane scope and browsing state, then place history handling after existing palette
   and recent-session routing.
3. Record successful ordinary and slash input at their current acceptance boundaries.
4. Add model and pane-level regression tests before enabling the behavior in normal builds.

Rollback removes the global and pane integration. The standalone JSON file can remain on disk
without affecting older builds; a later build with this feature can read it again.
