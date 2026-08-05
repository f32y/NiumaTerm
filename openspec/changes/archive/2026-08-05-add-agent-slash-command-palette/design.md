# Design: add-agent-slash-command-palette

## Context

The Agent Tab UI lives in `crates/app/src/ui/agent_pane.rs`. Its composer uses `gpui_component::input::InputState` and currently reads all text and calls `send_user_message` only on ordinary Enter; text submitted during active work becomes a steer message. The `Backend` enum dispatches the same UI to Claude Code stream-json and Codex app-server, but their slash-command behavior differs:

- Claude Code in stream-json or SDK mode publishes provider commands available to the current session through `slash_commands` in `system/init`. Published commands are still sent to the CLI as user messages beginning with `/`. The catalog can depend on the version, configuration, skills, plugins, and MCP, and is usually incomplete until the first `system/init`.
- Codex app-server is a rich-Client protocol and does not reuse the local slash dispatcher from the Codex TUI. Ordinary input goes directly to `turn/start` or `turn/steer`; compaction, review, and similar actions use separate RPCs such as `thread/compact/start` and `review/start`.
- The settings row below the composer already maintains model, permission or approval, sandbox, and effort. Slash commands must reuse the same `ThreadSettings` and model catalog rather than create a second state source that can drift.
- InputState's LSP completion menu targets code completion. Its position, row semantics, and disabled state do not meet command-palette needs. GPUI resolves Up, Down, Enter, Tab, and Escape into Input actions before raw key events, so AgentPane must intercept palette navigation during the parent node's action-capture phase; `capture_key_down` alone runs too late.

## Goals / Non-Goals

**Goals:**

- Provide one consistent, keyboard-first slash-command discovery and execution experience in Claude and Codex Agent Tabs.
- Make the catalog reflect the current backend, session state, and real provider support so unknown or unavailable commands never enter a model turn or steer action.
- Separate UI interaction, local session actions, and provider routing so new commands do not duplicate composer behavior.
- Initially support `/compact`, `/new`, `/clear`, `/model`, `/permissions`, `/status`, and `/review`, plus additional commands published dynamically by Claude.
- Preserve existing behavior for ordinary messages, history restoration, approvals, the settings row, and turn grouping.

**Non-Goals:**

- Reproduce native Claude or Codex TUI commands that require multi-level terminal screens, including login, themes, plugin installation, feedback upload, or terminal configuration.
- Build full Codex browsers for `/skills`, `/apps`, or `/plugins` initially. They can use the same catalog and execution interfaces later when matching app-server RPCs exist.
- Read or imitate the Codex TUI's internal command table. NiumaTerm promises to show only Codex commands that it actually implements.
- Force Claude skills and Codex `$skill-name` into one underlying invocation syntax.
- Intercept `/` in ordinary terminal panes. Native TUI behavior remains responsible when Claude or Codex runs directly in a terminal.

## Decisions

### D1: Use a backend-neutral catalog with explicit execution routes

Add backend-neutral `SlashCommandInfo` to `chat.rs` with normalized name, description, argument hint, source, and argument shape, plus catalog events. AgentPane merges three entry classes by name:

1. NiumaTerm local commands: `new`, `clear`, `model`, `permissions`, and `status`.
2. Commands explicitly implemented by a backend adapter, including `compact` and `review`.
3. The provider dynamic catalog, including the remaining commands from Claude `system/init.slash_commands`.

Resolve collisions as local command before explicit adapter implementation before dynamic provider entry. Dynamic metadata can add a description but cannot replace the execution route. Compare names without ASCII case sensitivity and display canonical lowercase `/name`.

Do not add a generic pass-through branch. AgentPane parses and dispatches local commands first. Other recognized entries call `Backend::execute_slash_command`, and the Claude or Codex adapter returns a result such as `Started`, `Completed`, `Queued`, `Rejected`, or `NotReady`. Ordinary `send_user_message` handles only non-slash input.

A hard-coded complete command table in the UI would not follow Claude skills, plugins, MCP, or version changes and would expose Codex TUI commands app-server cannot execute, so it is not used.

### D2: Render a dedicated AgentPane palette without changing general input completion

AgentPane renders the palette as an overlay above the composer, outside vertical layout so it does not move the transcript or composer. Use fixed row height, a maximum of roughly 8 to 10 rows, and scrolling beyond that. Each row includes `/name`, description, argument hint, and a disabled reason with subdued styling.

On the input wrapper, use `capture_action` for `MoveUp`, `MoveDown`, `Enter`, `IndentInline`, and `Escape`, stopping propagation only while the palette is visible. When it is hidden, pass actions to InputState:

- Up and Down move the highlight and keep the selected row visible.
- Enter executes the selected entry or enters an argument-selection stage.
- Tab completes only the canonical command token and enters argument input without executing.
- Escape closes only the palette and stops propagation so it does not invoke the existing running-agent interruption.
- Other keys continue to InputState, retaining current IME and multiline editing behavior.

Mouse selection is equivalent to Enter. This dedicated implementation adds a small amount of local state compared with the LSP completion menu, but it supports disabled rows, argument stages, upward positioning, and command execution without expanding the public API of `third_party/gpui-component`.

### D3: Parse only a command token at the start of the message

The command parser activates only when the raw text's first character is `/`, with no character before it. The command name ends at the first whitespace, and the remaining text is retained as its argument after removing separator whitespace. URLs, Windows and Unix paths, Markdown, and ordinary sentences containing `/` therefore do not trigger commands.

Show the palette for `/query` only while the cursor remains inside the first token. After whitespace, continue showing second-stage options only for commands with declared enum arguments, such as `/model ` and `/permissions `. Sort filter matches by exact, prefix, then substring, retaining merged catalog order within each group.

For Codex, an unknown slash command displays a local error below the composer and retains input; it never invokes `turn/start` or `turn/steer`. For Claude, only commands published by the dynamic catalog or declared by the adapter execute. Before the dynamic catalog is ready, core commands remain visible and the palette explains that provider commands appear after initialization. The system does not create a hidden turn or consume a model request to discover commands.

### D4: Reuse existing settings for enum arguments

Second-stage `/model ` options come from the existing `models: Vec<ModelInfo>`, and `/permissions ` options come from permission or approval constants already used by the backend. Selection updates the same `ThreadSettings`, clears the composer, and shows a lightweight confirmation, so the settings row updates immediately without additional protocol state.

Executing `/model` or `/permissions` without an argument opens the same second-stage list. An explicit argument must match a catalog protocol value exactly or an unambiguous display name. Otherwise the input remains and an error lists available values. Claude applies the change through its existing control request before the next message, while Codex sends thread overrides on the next `turn/start`.

Opening the existing DropdownMenu programmatically is unsuitable because AgentPane does not own a controllable open state for that picker and it cannot provide argument completion inside the composer. Reuse the same data rather than the menu instance.

### D5: Map core commands explicitly

- `/compact`: Claude sends `/compact` through a dedicated command-submission path in stream-json. Codex calls `thread/compact/start { threadId }`. Existing transcript and working lifecycles receive resulting activity, but no user-message bubble is added.
- `/review`: Claude sends `/review` only when the dynamic catalog or adapter baseline supports it. Codex calls `review/start`, initially fixed to `delivery: inline` and `target: { type: "uncommittedChanges" }`.
- `/new` and `/clear`: equivalent local actions that discard the current backend process, clear transcript, turn, pending approval, and queue state, then start a session with the same agent kind and cwd. The provider keeps the old persisted session for restoration from a future tab. The current tab retains `history_dismissed`, preserving the rule that hidden history stays hidden for the tab lifetime.
- `/status`: create a local notice outside any turn with backend, connection or running state, current model, permission or approval, and Codex sandbox, effort, and tier. Do not invent token usage that is not tracked.
- `/model` and `/permissions`: update settings as described in D4 without creating a provider turn.

Represent local notices and errors through a dedicated `ChatItem` variant or composer state, not as an Agent response, and do not start a working timer. If a provider command creates a real turn, reuse existing `TurnStarted`, `TurnCompleted`, and item events.

### D6: Treat the Claude catalog as replaceable dynamic state

When `stream_json.rs::process_system` handles `subtype == "init"`, read `slash_commands`, normalize entries by removing leading `/`, discard empty values, and emit `Event::Commands`. If the current CLI version also exposes the field in its initialize control response, publish it early through the same parser without depending on that optional field.

Each init catalog replaces the previous provider dynamic catalog rather than appending, so resume, configuration changes, or CLI upgrades cannot leave stale entries. Missing catalog data does not block the composer; local and adapter core commands remain available.

### D7: Track Codex command RPCs by request context

In `app_server.rs`, allocate a normal dynamic request id for command RPCs and store pending `request id → command` context. Convert error responses to non-fatal `Event::Error` values and remove pending entries. A successful response confirms only the request; actual activity and output continue through standard app-server turn and item notifications.

Even without dedicated cards, `contextCompaction`, `enteredReviewMode`, and `exitedReviewMode` must remain visible through the existing fallback item presentation. Dedicated rendering can be added later without changing slash dispatch.

### D8: Give every command an explicit policy during active work

Each command declares `Immediate`, `QueueUntilIdle`, or `IdleOnly`:

- `/status`, `/model`, and `/permissions` are Immediate.
- `/compact` and `/review` are QueueUntilIdle by default. Dynamic Claude commands also default to QueueUntilIdle so the existing message path cannot treat them as steer input.
- `/new` and `/clear` are IdleOnly and show a disabled reason while work is active. The user can first use the existing Stop or Escape action, preventing silent destruction of an active process.

AgentPane stores queued commands in FIFO order and executes them after `TurnCompleted`. If one command starts a new turn, wait for that turn to complete before continuing. Clear the queue with an explanation on fatal session state, process exit, or `/new`. Queue feedback stays in composer state and does not create a synthetic user turn.

## Risks / Trade-offs

- [Claude project skills, plugins, and MCP prompts are unavailable before the first `system/init`] Show core commands immediately, replace the dynamic catalog after init, and display a short not-ready hint without a hidden warm-up request.
- [Provider upgrades add, remove, or rename commands] Replace the Claude catalog dynamically. Show only Codex commands implemented by the adapter and confirmed through current app-server responses, treating RPC errors as non-fatal.
- [A slash command during active work can become steer input] Parse before the ordinary message path and require every slash command to follow an execution policy. Queue dynamic Claude commands locally by default.
- [The palette can conflict with IME, multiline input, or Escape interruption] Capture only the required navigation actions while visible, stop Escape propagation explicitly, and leave all other events to InputState.
- [Local model or permission state can briefly differ from provider state] Keep the existing ThreadSettings application path; later Claude init or Ready events can correct values from provider state.
- [Reproducing every native-Client command would expand scope without limit] Treat the menu as a contract for capabilities executable by NiumaTerm and hide TUI-only commands until a matching UI or RPC exists.

## Migration Plan

This is an additive UI and protocol feature with no persisted-data migration:

1. Add the parser, catalog model, and read-only palette while leaving the ordinary message path unchanged.
2. Add local commands and the settings argument stage.
3. Add Claude dynamic discovery and execution plus Codex compaction and review RPCs.
4. Enable the active-work queue and error or notice presentation.

Rollback removes the palette and command dispatch. Ordinary `send_user_message`, session history, and provider persistence formats remain unchanged.

## Open Questions

- Claude `slash_commands` currently guarantees only command names. It is unclear whether a future protocol version will publish structured descriptions and argument hints. Use local metadata for known core commands and a generic description for unknown dynamic commands initially.
- The initial Codex `/review` reviews only uncommitted changes. Decide from initial usage whether base-branch, commit, and custom targets should become later second-stage arguments.
