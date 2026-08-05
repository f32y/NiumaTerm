# Manual validation: agent slash command palette

> Status: prepared, not executed. Repository guidance requires explicit user
> authorization before compiling, testing, or launching NiumaTerm.

## Launch

1. Build only after explicit authorization.
2. Launch the isolated instance as `target\debug\NiumaTerm.exe --testing`.
3. Open one Codex Agent Tab and one Claude Agent Tab in a disposable working tree.

## Palette and input behavior

- Type `/` in each Agent Tab. Confirm the palette overlays above the composer
  without moving the transcript or composer.
- Confirm rows show command, description, optional argument hint, selection,
  disabled reason, and no-results text.
- Type partial exact, prefix, and substring queries and confirm ordering.
- Move with Up/Down through more than nine rows; confirm scrolling keeps the
  selected row visible.
- Confirm Enter and mouse click behave identically, while Tab completes the
  token without executing it.
- Press Escape once with the palette open during a running turn. Confirm only
  the palette closes. Press Escape again and confirm the running turn stops.
- Exercise an IME composition near `/`; confirm composition/Enter is handled
  by the editor and no partial command executes.
- Move the caret after the first token and confirm palette navigation no longer
  captures editor keys.

## Local commands

- `/model` opens the current backend's model choices. Select by keyboard and
  mouse, then confirm the existing model picker reflects the same value.
- `/permissions` opens the backend's existing approval/permission choices and
  updates the existing picker.
- Enter an unknown, ambiguous, and invalid model/permission value. Confirm the
  input remains and an ERROR message appears.
- Run `/status` while idle and running. Confirm only known backend, status, and
  settings fields appear; no user bubble, turn, timer, or token estimate is made.
- Run `/clear` and `/new` while idle. Confirm the backend is replaced, transcript,
  approval and queued work are cleared, persisted history remains, and a hidden
  history list does not reappear.
- Attempt `/clear` and `/new` while running. Confirm they remain in the input and
  report that the agent must be idle.

## Backend commands

- In Codex, run `/compact`; confirm a `thread/compact/start` operation completes
  and any `contextCompaction` item is visible.
- In Codex, run `/review`; confirm an inline review of uncommitted changes starts
  and review-mode lifecycle items remain visible.
- In Claude, confirm dynamically announced commands replace the dynamic portion
  of the palette without duplicating local/adapter commands.
- In Claude, run `/compact` and one discovered command. Confirm neither creates
  a user bubble and working UI begins only after a real TurnStarted event.
- With a provider version that omits `slash_commands`, confirm local and adapter
  commands remain usable and no warm-up turn is created.

## Queueing and routing regressions

- During a model turn, submit `/compact`, `/review`, then another queued command.
  Confirm QUEUED feedback reports FIFO order and commands execute one at a time
  after TurnCompleted.
- Kill or fail the backend with commands queued. Confirm the queue is cleared and
  the cancellation reason is visible.
- Submit an unknown leading slash command. Confirm it never becomes a model turn
  or steer and the original input remains editable.
- Submit ordinary prose containing a slash after the first character. Confirm it
  follows the normal message/steer path.

## Verification record

- Static formatting/parser check: `rustfmt --edition 2024` completed successfully
  for all changed Rust files.
- OpenSpec check: `openspec validate add-agent-slash-command-palette --strict`
  completed successfully.
- Build command: `cargo check` passed for the workspace on 2026-08-05.
- Unit-test command: not run.
- Manual launch command: not run.
- Remaining unverified items: all runtime and protocol integration behavior above.
