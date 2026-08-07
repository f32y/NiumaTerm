## 1. Active Claude conversation chain

- [x] 1.1 Add JSONL fixtures for a linear session, multiple `parentUuid` branches, missing parents, compaction metadata, tool results, meta records, and sidechain records.
- [x] 1.2 Implement a structured Claude transcript index that selects the last valid leaf and reconstructs only its `parentUuid` chain without following `logicalParentUuid`.
- [x] 1.3 Route Claude history replay through the active-chain index while preserving tool-use/result association, reasoning, diffs, and existing record filtering.
- [x] 1.4 Extract ordered `ClaudeCheckpoint` values from human user prompts on the active chain, including message UUID, parent identity, prompt text, timestamp, and conservative file-restore availability.
- [x] 1.5 Add unit tests proving abandoned branches never appear in replay or checkpoint lists and broken parents never cause cross-branch splicing.

## 2. File checkpoint control protocol

- [x] 2.1 Enable `CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING=true` for every newly spawned Claude stream-json process and cover the process configuration with a test.
- [x] 2.2 Generalize Claude control-request tracking to correlate request ids with pending operation kinds without changing initialize behavior.
- [x] 2.3 Implement the `rewind_files` request using the selected user message UUID and publish backend-neutral success/failure events for the matching response.
- [x] 2.4 Add protocol tests for success, provider rejection, expired/missing checkpoints, unrelated response ids, process exit, and malformed control responses.

## 3. Recoverable conversation forks

- [x] 3.1 Implement a Claude session fork helper that slices the active chain at a parent message, assigns a new session id, remaps copied UUID references, excludes file undo history, writes atomically, and never mutates the source transcript.
- [x] 3.2 Handle rewind before the first prompt as a fresh empty session and later prompts as resumable prefix forks.
- [x] 3.3 Add fork fixtures/tests covering preserved source files, exact prefix replay, UUID remapping, unknown JSONL fields, compaction records, and absent targets.
- [x] 3.4 Expose the fork result and replay data to Agent Pane without introducing a Python SDK or other runtime dependency.

## 4. Slash command and rewind UI

- [x] 4.1 Add Claude-only `/rewind` catalog metadata and route it as a NiumaTerm-owned `IdleOnly` action instead of a provider slash message; keep it absent from Codex.
- [x] 4.2 Build the first-stage checkpoint picker with recent-first prompt rows, timestamps, disabled/empty states, keyboard navigation, mouse selection, and cancellation.
- [x] 4.3 Build the second-stage action picker for file-only, conversation-only, combined restore, and cancel, including scope text and unavailable-file reasons.
- [x] 4.4 Add a rewind operation state machine that blocks duplicate actions and message submission while preserving local, non-turn feedback semantics.
- [x] 4.5 Wire file-only restore to the correlated backend result without changing transcript, composer, session id, working timer, or turn counters.

## 5. Conversation replacement and combined behavior

- [x] 5.1 Replace the active Claude backend with a successful prefix fork while preserving cwd and settings, replaying the prefix, and restoring the selected prompt into the composer.
- [x] 5.2 Advance the session epoch and clear old approvals, queued commands, active-turn/compaction/context state, and provider catalogs so stale backend events cannot affect the fork.
- [x] 5.3 Implement combined restore as awaited file rewind followed by conversation fork, with no fork after a file-phase failure.
- [x] 5.4 Report file-success/fork-failure as an explicit partial result and retain the original session as a history recovery path.
- [x] 5.5 Add state-transition tests for cancellation, each successful action, each failure boundary, duplicate submission, stale events, and continued messaging under the new session id.

## 6. Validation and compatibility

- [x] 6.1 Add fake stream-json integration coverage proving `/rewind` never becomes a user/provider slash turn and control responses complete only their matching operation.
- [x] 6.2 Verify legacy sessions without file snapshots still support conversation rewind and show honest file-action availability or provider errors.
- [x] 6.3 Manually validate all three actions in an isolated disposable repository by launching NiumaTerm with `--testing`, including cancel, old-session, expired-checkpoint, and partial-failure presentation.
- [x] 6.4 Run `cargo fmt --all --check`, targeted agent/session/UI tests, and the repository-required workspace clippy checks; record any environment-limited validation explicitly.
