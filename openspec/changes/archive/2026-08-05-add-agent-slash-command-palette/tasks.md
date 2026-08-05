# Tasks: add-agent-slash-command-palette

## 1. Command model and pure logic

- [x] 1.1 Add backend-neutral `SlashCommandInfo`, source, argument shape, execution policy, catalog event, and execution-result types to `nmt_agent_utils::chat`.
- [x] 1.2 Create a pure-logic slash-command module for the Agent UI that parses a leading command and arguments, distinguishes ordinary text containing slashes, and preserves the original argument text.
- [x] 1.3 Normalize and merge local, adapter, and dynamic Claude catalogs by priority, and sort filtered entries by exact, prefix, and substring matches.
- [x] 1.4 Add window-independent unit tests for parsing, catalog merging, filter sorting, unknown commands, and enum-argument matching.

## 2. Claude Code dynamic catalog and command execution

- [x] 2.1 Parse `system/init.slash_commands` in `stream_json.rs::process_system`, remove the leading `/`, discard empty values, and replace the current dynamic catalog through `Event::Commands`.
- [x] 2.2 If the initialize control response has the same field, publish it early with the same parser. Remain compatible when absent and do not create a warm-up turn.
- [x] 2.3 Add a dedicated `execute_slash_command` path to Claude Session that sends provider commands without creating a user-message bubble and maps the provider lifecycle to command results.
- [x] 2.4 Add Claude protocol tests for catalog replacement, duplicate and invalid names, dynamic-command submission, provider errors, and init without a catalog.

## 3. Codex command RPCs

- [x] 3.1 Add a `thread/compact/start { threadId }` request to `app_server.rs` and track command context by request id.
- [x] 3.2 Add inline `review/start`, initially fixed to `target.type = uncommittedChanges`, and keep review turns and items in the existing event stream.
- [x] 3.3 Map successful command RPC responses and non-fatal errors to backend-neutral execution results, clear the pending map, and keep the current session usable.
- [x] 3.4 Ensure `contextCompaction`, `enteredReviewMode`, and `exitedReviewMode` remain visible through fallback tools or items, and add matching JSON protocol tests.

## 4. AgentPane command state and local actions

- [x] 4.1 Extend `Backend` dispatch with the current adapter command catalog and `execute_slash_command`, while keeping ordinary `send_user_message` limited to non-slash input.
- [x] 4.2 Merge local, adapter, and provider catalogs in AgentPane and maintain the palette query, selected row, argument stage, error or notice, and dynamic-catalog readiness.
- [x] 4.3 Invoke the slash parser before `send_user_message`. Clear input after a successful command; retain input and show a local error for unknown or unavailable commands; never let a slash branch reach ordinary turn or steer behavior.
- [x] 4.4 Implement second-stage selection and explicit argument validation for `/model` and `/permissions`, reusing the existing model and permission sources and updating the same `ThreadSettings`.
- [x] 4.5 Implement a local `/status` notice that shows only known backend, state, and settings fields without creating a turn, working timer, or unsupported token-usage value.
- [x] 4.6 Implement equivalent `/new` and `/clear` actions available only while idle. Replace the backend, clear session UI state, retain provider history, and preserve `history_dismissed` for the current tab.
- [x] 4.7 Implement Immediate, QueueUntilIdle, and IdleOnly policies with FIFO execution. Run commands serially after TurnCompleted and clear the queue with a reason on fatal, exited, or new.

## 5. Slash-command palette UI

- [x] 5.1 Render a palette overlay above the composer without changing layout. Include command name, description, argument hint, disabled reason, empty result, and a notice when the Claude dynamic catalog is not ready.
- [x] 5.2 Use fixed row height, a maximum height of about 8 to 10 rows, scrolling, and highlight visibility. Selecting an available row with the mouse is equivalent to Enter.
- [x] 5.3 In the Input wrapper's action-capture phase, handle Up, Down, Enter, Tab, and Escape only while the palette is visible. Escape closes the palette and prevents agent interruption; other keys return to InputState.
- [x] 5.4 Add second-stage option palettes for `/model ` and `/permissions `, and make Tab complete the command token without executing it.
- [x] 5.5 Give local notices, errors, and queued feedback a presentation distinct from User and Agent items, and exclude them from completed-turn grouping.

## 6. Regression coverage and validation preparation

- [x] 6.1 Add command-state tests showing that local commands do not advance a turn, provider commands enter working state only after a real TurnStarted, and a slash command during active work does not become steer input.
- [x] 6.2 Add `/clear` reset tests covering backend epoch, approval and queue cleanup, retained history files, and preserved `history_dismissed`.
- [x] 6.3 Write a manual checklist for both Agent Tab variants: `/` overlay, keyboard, mouse, IME, Escape, core commands, Claude dynamic catalog, Codex RPCs, FIFO behavior during active work, unknown commands, and ordinary text containing slashes.
- [x] 6.4 Run targeted unit tests only with explicit user authorization, and use `target\debug\NiumaTerm.exe --testing` for manual validation. Record commands, results, and unvalidated items.

## 7. Keyboard navigation correction

- [x] 7.1 Follow GPUI's action-before-raw-key dispatch order by moving palette keyboard handling from `capture_key_down` to parent-level `capture_action`.
- [x] 7.2 Add pure-logic regression tests for wrapping Up and Down selection, correcting an index after catalog shrinkage, and an empty catalog.
