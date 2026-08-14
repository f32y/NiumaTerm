# Agent Harness Integration Requirements

| Field | Value |
| --- | --- |
| Status | Current implementation baseline |
| Date | 2026-08-14 |
| Scope | Agent Tabs and future local CLI harness adapters |

## 1. Purpose

This document defines the user-facing behavior that a new agent harness should
provide when it is added to NiumaTerm. It records the common behavior already
available in the Codex and Claude Tabs, identifies provider-specific additions,
and separates the minimum usable integration from optional capability groups.

The requirements describe observable behavior. They do not require a new shared
trait or a rewrite of the existing adapters before another harness is added.

## 2. Goals

- Preserve one consistent Agent Tab experience across harnesses.
- Keep provider-specific behavior available without presenting unsupported UI.
- Reuse the existing profile, session, transcript, history, and notification
  paths.
- Give future harness work a clear minimum scope and a repeatable acceptance
  checklist.
- Keep credentials and session data within the existing storage boundaries.

## 3. Non-goals

- Defining the CLI protocol of a third-party harness.
- Requiring every harness to implement every Codex or Claude addition.
- Redesigning the Agent Tab layout.
- Moving app-wide usage or update UI into each Agent Tab.
- Building a general plug-in system before a third harness needs one.

## 4. Terms and requirement levels

- **Harness**: a local agent CLI process controlled by NiumaTerm.
- **Adapter**: provider-specific code that launches the harness, translates its
  output into shared chat events, and sends user or UI actions back to it.
- **Core**: required for every new harness.
- **Conditional**: required when the harness exposes the named ability.
- **App-owned**: supplied by NiumaTerm and expected to work without provider
  support.

A new adapter shall expose only behavior it can perform. Unsupported controls
shall be absent or disabled with a clear reason; the UI shall not invent model,
usage, history, approval, or progress data.

## 5. Current feature inventory

### 5.1 Shared Codex and Claude Tab behavior

| Area | Current behavior |
| --- | --- |
| Profiles | Name, command, model, effort, environment, custom endpoint, API key, working directory, and saved tab state |
| Composer | Multiline editing, send, stop, Escape handling, steering during a running turn, and restoration of interrupted input |
| Input history | Up/down navigation, provider and working-directory scope, persisted entries, 100-entry limit, and adjacent duplicate removal |
| Transcript | User prompts, Markdown responses, reasoning, command output, file changes, web or other tool activity, errors, and compaction markers |
| Transcript interaction | Streaming updates, text selection and copy, timestamps, collapsible groups, local file and URL opening, and large-output virtualization |
| Run status | Running state, elapsed time, output tokens, step label, first-output delay, interruption, cache use, and Git branch when available |
| Context usage | Used, remaining, and maximum tokens plus provider token categories and cumulative values when reported |
| Session history | Recent sessions for the current working directory, title, branch, time, resume, `/resume`, `/new`, and `/clear` |
| Slash menu | Filtering, ranking, keyboard and mouse use, argument checks, result feedback, and queue policy |
| Thread settings | Model, permission mode, and effort selection, remembered per profile |
| Approvals | Approve once, approve for the session, decline, and cancel |
| Child work | Child-agent status, read-only child transcript, tab running/unread state, and native completion notifications |
| Recovery | Context compaction and in-place recovery after a provider CLI update |

The shared local commands are `/new`, `/clear`, `/resume`, `/model`,
`/permissions`, and `/status`.

### 5.2 Codex additions

| Capability | Current behavior |
| --- | --- |
| Runtime settings | Approval policy, approval reviewer, sandbox mode, reasoning effort, and service tier |
| Skills | Skills appear directly in the slash menu and under `/skills`; invocation carries the selected name and path |
| Review | `/review` starts the Codex review flow |
| Compaction | `/compact` compacts context and updates usage accounting; the generated summary is not expanded as readable transcript content |
| History | Paged session history and descendant-thread loading |
| Commands | `/compact`, `/review`, and `/skills` in addition to the shared local commands |

### 5.3 Claude additions

| Capability | Current behavior |
| --- | --- |
| Rewind | `/rewind` can restore files, the conversation, or both while preserving the original session history |
| Questions | Structured question cards for harness requests that need user choices |
| Workflows | A right-side view for runs, phases, agents, models, token use, and read-only conversations |
| Commands | `/compact [instructions]`, `/rewind`, and commands or aliases discovered from the running CLI |
| Context details | Composition segments and a readable compaction summary when supplied by the CLI |
| Model handling | Model-dependent effort options and profile submodel replacement |

### 5.4 App-wide provider behavior

These features are related to a provider but do not belong to one Agent Tab:

- Account usage presentation.
- Provider CLI update detection, coordinated process suspension, update, and
  session recovery.
- Provider-specific daily usage presentation when usage data is available.

They are not blockers for a core adapter unless explicitly included in the new
harness scope.

## 6. Functional requirements

### HR-001: Harness identity and profile launch

**Level:** Core

The integration shall add a stable harness identity that can be stored in an
agent profile and restored with a saved tab. A profile shall be able to provide:

- A display name and harness type.
- The executable command and supported launch arguments.
- A working directory.
- Environment values required by the process.
- Initial model, effort, and permission values when supported.
- A custom endpoint and credential reference when supported.

Credentials shall use the existing protected credential storage path. Secrets
shall not be copied into tab state, transcript items, process diagnostics, or
ordinary logs.

If the executable cannot be found or launched, the tab shall remain open and
show a useful recovery message.

### HR-002: Session lifecycle

**Level:** Core

The adapter shall support these lifecycle operations:

1. Start a new session in the profile working directory.
2. Accept an initial prompt and later prompts.
3. Stream output without blocking the UI.
4. Report successful completion, interruption, and failure as distinct results.
5. Stop a running turn without closing the tab.
6. Shut down the process when its owning tab or application session ends.

Steering a running turn is conditional. If unsupported, input shall follow the
existing queue policy or remain unavailable until the turn completes.

### HR-003: Shared chat event mapping

**Level:** Core

Provider output shall be translated into the shared chat event and transcript
item types in `agent_utils::chat`. The adapter shall preserve stable IDs and
ordering so streaming updates amend the intended item instead of creating
duplicates.

At minimum, the adapter shall report:

- Session start and terminal state.
- User message acceptance.
- Assistant text deltas or completed text.
- Provider errors with a user-readable message.
- Turn completion or interruption.

When available, it shall also report reasoning, commands, command output, file
changes, tool activity, usage, context limits, approvals, questions, child work,
and progress.

Unknown provider events shall not terminate a healthy session. They may be
logged without secrets and ignored until explicit UI support is added.

### HR-004: Transcript behavior

**Level:** Core with conditional item types

The new harness shall use the existing transcript renderer. It shall support:

- Incremental updates during a turn.
- Selection and copy without moving composer focus unexpectedly.
- Markdown presentation for assistant text.
- Expand/collapse behavior for verbose reasoning and tool groups.
- Timestamps and completion state.
- Local path and URL activation through the existing link handler.
- Virtualized presentation for large command or tool output.

The adapter shall not place raw provider payloads directly in user-facing
transcript text when a shared item type exists.

### HR-005: Composer and input history

**Level:** App-owned

The existing multiline composer, keyboard actions, stop action, and persistent
input history shall work for the new harness. History shall remain scoped by
harness and working directory and retain the current size and duplicate rules.

If a prompt is rejected before the harness accepts it, the text shall remain
available for retry. If a running turn is interrupted, unsent composer text
shall be preserved.

### HR-006: Run status

**Level:** Core with conditional fields

The tab shall always distinguish idle, starting, running, stopping, completed,
interrupted, and failed states. The tab shall show elapsed time while running.

The adapter shall publish output token count, current step, first-output delay,
cache use, and Git branch only when reliable data is available. Missing values
shall not be displayed as zero.

### HR-007: Models, permissions, and effort

**Level:** Conditional

If the harness exposes runtime choices, the adapter shall provide valid models,
permission modes, and effort values to the existing settings row. It shall:

- Mark the active value.
- Reject unsupported values before sending them to the process.
- Persist the chosen value per profile.
- Refresh the available list when the CLI publishes a change.
- Make clear whether a change affects the current turn, next turn, or a new
  session.

If a category is unsupported, its control and related slash command shall be
omitted.

### HR-008: Slash commands

**Level:** Core for applicable local commands; conditional for provider commands

The slash menu shall combine applicable local commands with commands supplied by
the adapter. Each command shall include a name, description, argument hint when
needed, and source.

The menu shall retain existing filtering, ranking, keyboard and mouse use, and
argument checks. The adapter shall return a clear result for accepted, rejected,
queued, and unsupported invocations.

`/new`, `/clear`, and `/status` shall work for every harness. `/resume`,
`/model`, and `/permissions` shall appear only when their underlying abilities
exist. Provider-only commands shall never appear in another harness tab.

### HR-009: Session history and resume

**Level:** Conditional

If the harness has durable session IDs, the adapter shall list recent sessions
for the current working directory and resume a selected session. History entries
should include title, branch, and update time when available.

Resume shall restore enough state for new prompts and later events to continue
in the same provider session. Paged history and descendant sessions are optional
extensions.

If durable resume is unavailable, the tab may keep only its in-memory session;
`/resume` and recent-history UI shall be omitted.

### HR-010: Context usage and compaction

**Level:** Conditional

When the harness reports token usage or a context limit, the adapter shall map
the values to the existing context usage model without estimating missing
categories. Used, remaining, and maximum values shall remain internally
consistent.

If compaction is supported, the adapter shall expose a command and show a
transcript marker. Optional instructions and a readable generated summary shall
be shown only when the harness supports them.

### HR-011: Approvals and structured questions

**Level:** Conditional

Approval requests shall pause the affected operation and present only responses
the harness can honor. Supported outcomes may include approve once, approve for
the session, decline, and cancel.

Structured questions shall preserve prompt text, available choices, multi-select
rules, validation, and cancellation. A plain-text fallback is acceptable only
when the harness itself sends a plain-text question.

Closing or replacing a session shall resolve pending UI state without sending a
response to the wrong session.

### HR-012: Child agents and workflows

**Level:** Conditional

If the harness reports child work, the adapter shall publish stable child IDs,
status transitions, labels, and read-only transcript content. Parent and child
events shall remain associated with the correct session.

If the harness reports workflow structure, the existing workflow view may show
runs, phases, agents, model data, token use, and read-only conversations. A
harness without workflow data shall not show an empty workflow view.

### HR-013: Tab state and notifications

**Level:** App-owned

The existing tab indicators shall reflect running and unread activity for the
new harness. A background completion may raise the existing native notification
when the application is not focused.

Opening the relevant tab shall clear unread state according to existing tab
behavior. Notifications shall identify the harness and session without exposing
prompt or credential content that the current notification policy omits.

### HR-014: Provider CLI updates and recovery

**Level:** Conditional

If NiumaTerm manages updates for the harness CLI, update handling shall:

1. Detect an available version without interrupting active work.
2. Coordinate all sessions that use the same executable.
3. Stop or suspend them only after the user accepts the update.
4. Run the update once.
5. Relaunch and resume recoverable sessions in place.
6. Leave a clear error and retry path when update or recovery fails.

The core adapter may omit managed updates and rely on the user to update the CLI.

### HR-015: Rewind or rollback

**Level:** Conditional

If the harness supports rewind, the UI shall expose only the scopes it can
restore, such as files, conversation state, or both. The operation shall require
an explicit target turn and confirmation when local files may change.

The original session history shall remain inspectable. A harness without rewind
support shall not show the command or related controls.

### HR-016: Failure isolation and graceful reduction

**Level:** Core

A malformed optional event, unavailable usage endpoint, failed history request,
or unsupported command shall not close a usable chat session. The smallest
affected feature shall fail independently and present a retry or clear status
where useful.

Process exit, parse failure, and transport failure shall be distinguishable in
diagnostics. User-facing errors shall include the next useful action without
exposing secrets or dumping unbounded provider output.

## 7. Minimum integration scope

A harness is ready for an initial release when it provides all of the following:

- Profile identity, launch command, environment, and working directory.
- New session, prompt send, streamed assistant text, stop, completion, and error
  reporting.
- Shared event mapping with stable item ordering.
- Existing composer, transcript, input history, run state, tab indicators, and
  notifications.
- `/new`, `/clear`, and `/status`.
- Secret-safe logging and clean process shutdown.
- Clear omission of unsupported optional controls.

The following may be added later as independent capability groups:

- Runtime model, permission, and effort changes.
- Durable history and resume.
- Usage and context limits.
- Compaction.
- Approvals and structured questions.
- Dynamic slash commands and skills.
- Child agents and workflows.
- Rewind.
- Managed CLI updates and account usage UI.

## 8. Current integration points

Future work should extend the existing paths before adding new parallel UI or
storage code:

| Responsibility | Current path |
| --- | --- |
| Shared chat events, items, settings, history, and usage | [`crates/agent_utils/src/chat.rs`](../crates/agent_utils/src/chat.rs) |
| Agent profile launch and restoration | [`crates/app/src/agent_pane/profile.rs`](../crates/app/src/agent_pane/profile.rs) |
| Backend selection and provider actions | [`crates/app/src/agent_pane/session/backend.rs`](../crates/app/src/agent_pane/session/backend.rs) |
| Local slash command definitions | [`crates/app/src/agent_pane/commands.rs`](../crates/app/src/agent_pane/commands.rs) |
| Slash routing and composer behavior | [`crates/app/src/agent_pane/composer/mod.rs`](../crates/app/src/agent_pane/composer/mod.rs) |
| Rewind UI | [`crates/app/src/agent_pane/composer/rewind.rs`](../crates/app/src/agent_pane/composer/rewind.rs) |
| Settings row | [`crates/app/src/agent_pane/view/settings_row.rs`](../crates/app/src/agent_pane/view/settings_row.rs) |
| Approval, question, update, and status views | [`crates/app/src/agent_pane/view/banners.rs`](../crates/app/src/agent_pane/view/banners.rs) |
| Session history view | [`crates/app/src/agent_pane/view/history.rs`](../crates/app/src/agent_pane/view/history.rs) |
| Context usage | [`crates/app/src/agent_pane/context_usage.rs`](../crates/app/src/agent_pane/context_usage.rs) |
| Transcript presentation | [`crates/app/src/agent_pane/transcript/render.rs`](../crates/app/src/agent_pane/transcript/render.rs) |
| Local path and URL handling | [`crates/app/src/agent_pane/links.rs`](../crates/app/src/agent_pane/links.rs) |
| Child-agent detail view | [`crates/app/src/ui/background_tasks/mod.rs`](../crates/app/src/ui/background_tasks/mod.rs) |
| Workflow view | [`crates/app/src/ui/workflows/mod.rs`](../crates/app/src/ui/workflows/mod.rs) |
| Tab running and unread state | [`crates/app/src/ui/tab_bar.rs`](../crates/app/src/ui/tab_bar.rs) |
| Native agent notifications | [`crates/app/src/ui/shell/agent_notifications.rs`](../crates/app/src/ui/shell/agent_notifications.rs) |

The smallest implementation should add the new harness identity and adapter,
then extend existing provider branches only where behavior differs. A broader
adapter refactor should wait until the third integration demonstrates repeated
code that can be removed safely.

## 9. Acceptance checklist

### Core behavior

- [ ] A profile opens a new tab in the selected working directory.
- [ ] A prompt is accepted once and appears once in the transcript.
- [ ] Streamed response updates amend one assistant item in order.
- [ ] Stop produces an interrupted state and leaves the tab usable.
- [ ] Process failure produces a readable error and allows a new session.
- [ ] Closing the tab ends its owned process without leaving descendants.
- [ ] Input history is isolated by harness and working directory.
- [ ] `/new`, `/clear`, and `/status` work from keyboard and slash menu.
- [ ] Running and unread tab indicators update correctly.
- [ ] A background completion follows the existing notification policy.
- [ ] Unsupported controls and commands are not offered.
- [ ] Credentials and raw secret values do not appear in saved tab data or logs.

### Conditional behavior

- [ ] Model, permission, and effort choices show only valid values.
- [ ] Resume continues the selected provider session in the correct directory.
- [ ] Usage totals and context limits update without guessed values.
- [ ] Compaction, approvals, questions, child work, workflows, and rewind each
      degrade independently when unavailable.
- [ ] Dynamic commands disappear when the CLI no longer publishes them.
- [ ] Managed update recovery preserves every recoverable open session.

### Regression coverage

- [ ] Existing Codex profiles, sessions, commands, skills, review, history, and
      settings retain their behavior.
- [ ] Existing Claude profiles, sessions, dynamic commands, questions, rewind,
      workflows, and settings retain their behavior.
- [ ] Shared transcript selection, copy, folding, links, and large-output
      rendering remain unchanged for both existing harnesses.
