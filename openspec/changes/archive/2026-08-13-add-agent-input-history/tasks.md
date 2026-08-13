## 1. Shared History Storage

- [x] 1.1 Add the `agent_pane/input_history` module with a serializable scope key for target,
  backend, and normalized effective working directory.
- [x] 1.2 Implement oldest-to-newest entry storage, adjacent duplicate collapse, the 100-entry
  limit, and immutable snapshots for pane navigation.
- [x] 1.3 Implement versioned JSON loading and atomic saving at
  `agent-input-history.json`, including missing or invalid input handling, diagnostics, and
  injected process-specific paths for tests and `--testing`.
- [x] 1.4 Add the application-global history service, ordered background save processing, startup
  initialization, and a final clean-shutdown flush.
- [x] 1.5 Add storage tests for file round trips, invalid JSON, backend, target, and working-directory
  isolation, adjacent duplicates, the retention limit, and save failures that preserve memory.

## 2. Composer History Navigation

- [x] 2.1 Add the normalized scope and per-pane `InputHistoryNavigation` snapshot state to
  `AgentPane`, and reset active browsing when the user edits the input.
- [x] 2.2 Implement handled-or-declined Up and Down transitions for empty entry, older and newer
  movement, oldest-item clamping, newest-item clearing, unchanged text checks, collapsed
  selection checks, and whole-buffer cursor boundaries.
- [x] 2.3 Route history navigation after command, skill, rewind, and recent-session handling but
  before input-editor fallback, stopping propagation only when history handles the action.
- [x] 2.4 Restore recalled text with the cursor at the UTF-8 end, clear structured skill binding,
  and dismiss automatic slash palette display until the recalled text is edited.
- [x] 2.5 Add pane and navigation tests for multi-line entries, cursor placement, non-empty drafts,
  selections, interior cursor positions, no wrapping, slash text, palette priority,
  recent-session priority, and editor fallback.

## 3. Accepted Input Recording

- [x] 3.1 Record trimmed ordinary composer text only after `send_text_with_skill` accepts a new
  turn or steering input, before clearing the composer.
- [x] 3.2 Record submitted slash text only after `submit_slash_input` reports a successful local,
  queued, or provider action, without recording lower-level UI-generated messages.
- [x] 3.3 Add regression tests for accepted new turns, accepted steering, successful slash actions,
  validation failures, unavailable sessions, failed dispatch, and current-process recall after
  a durable save error.

## 4. Validation

- [x] 4.1 Run formatting, focused Agent pane and history tests, affected workspace checks, and
  clippy for all affected targets.
- [x] 4.2 Launch `target\debug\NiumaTerm.exe --testing` and manually verify Up and Down recall,
  draft preservation, palette priority, multi-line text, and shared history across matching
  Agent Tabs; use the storage round-trip test to verify restart persistence.
