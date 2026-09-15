# Codex Subtask Prompt Display

Updated: 2026-09-15

Status: investigation recorded; implementation deferred. No approach has been
selected.

## Reported behavior

Opening a Codex Tab subtask in Background Tasks can show a tool call as its
first transcript entry, without the task instructions sent by the parent
agent. The reporter confirmed that the task instructions were absent and was
unsure whether an opening assistant reply was also missing.

The official ChatGPT App appeared to display task instructions, but it was not
confirmed whether this was the same local Codex conversation or a subtask in a
ChatGPT conversation. Those views use different data sources.

## Findings

### Local Codex task records

Three NiumaTerm-created subtasks from 2026-09-14 were inspected:

| Task | Thread ID |
| --- | --- |
| `/root/review_reuse` | `01a09f3a-36e2-7b51-92f5-f873be6a2cf7` |
| `/root/review_errors` | `01a09f3a-5d93-7e62-9780-23332b8240b1` |
| `/root/review_platform` | `01a09f3a-7c81-7aa2-9d51-8baf7b4ffe10` |

Their logs are under `%USERPROFILE%\.codex\sessions\2026\09\14`, in
`rollout-*.jsonl` files whose names end with the thread ID.

Each initial `response_item` of type `agent_message` contained:

- An `input_text` block identifying the message type, task, and sender.
- An `encrypted_content` block containing the task body.

The readable header alone did not contain the task instructions. The first
visible execution entry in each sampled task was a command execution; no
opening assistant reply preceding that command was found in these samples.

### Existing NiumaTerm handling

The Codex adapter already preserves two available sources of readable input:

- A legacy `collabAgentToolCall` with `tool: "spawnAgent"` supplies `prompt`.
  The adapter records that value as the task objective and launch message.
- A live `rawResponseItem/completed` notification can supply a plaintext
  `agent_message`. The adapter retains its first message for the child.

When loading child history, a retained launch message is prepended unless a
matching user message already exists. Mixed plaintext and encrypted messages
are deliberately excluded, since displaying only the header would present
incomplete instructions as the task body.

`experimentalRawEvents` is already enabled for new threads. It exposes the
original records; it does not turn encrypted content into readable text.

Relevant implementation:

- [Launch message retention](../../crates/agent/src/codex/app_server/background_tasks/launch_messages.rs)
- [Child tracking and spawn prompt capture](../../crates/agent/src/codex/app_server/background_tasks/mod.rs)
- [Child history response handling](../../crates/agent/src/codex/app_server/mod.rs)
- [Thread request and history parsing](../../crates/agent/src/codex/app_server/protocol.rs)

### Official App inspection

Inspected package: `OpenAI.Codex_26.908.4834.0_x64__2p2nqsd0c76g0`.
Its bundled executable reported `codex-cli 0.154.0-alpha.6.2`.

The JavaScript in `app/resources/app.asar` showed these distinct paths:

| View | Task instruction source |
| --- | --- |
| ChatGPT conversation subtask | Parent message metadata: `codex_collab_agent_tool_call.prompt` |
| Local Codex subtask | Local thread history, including `thread/read` and paginated history requests |

The ChatGPT subtask panel renders a "Delegated task" section only when its
`prompt` is present. This establishes a readable metadata source for that
view; it does not establish client-side decryption of local task records.

The App also converts certain `functionCallOutput` entries into user messages.
That conversion recognizes `codex_app` task delegation and automation output,
including `<codex_delegation>` content. It is not a general decoder for native
`collaboration.spawn_agent` messages.

Relevant files inside the archive:

- `webview/assets/chatgpt-subagents-panel-d1e42d39d920.js`
- `webview/assets/local-conversation-subagents-panel-tab-5d9af437ca4d.js`
- `webview/assets/app-initial-d9bed9d614d8.js`
- `webview/assets/src-996ff3571e1f.js`

### Backend verification

The bundled official executable was used for read-only requests against the
same `review_reuse` thread:

| Request | Turns | Items | User messages | First item |
| --- | ---: | ---: | ---: | --- |
| `thread/read`, `includeTurns: true` | 5 | 211 | 0 | `commandExecution` |
| `thread/turns/list`, ascending, `itemsView: "full"` | 5 | 211 | 0 | `commandExecution` |

The turns listing had no remaining cursor. These results confirm that the
official bundled backend did not return a readable task prompt through either
complete history request for this sample. The official UI was not tested
against the same thread.

The existing 25 tests in `codex::app_server::background_tasks::tests` passed,
including plaintext launch message retention, duplicate avoidance, and
rejection of partial encrypted instructions. These tests verify adapter
behavior; they do not prove that every backend supplies a readable prompt.

## Upstream behavior to revisit

The local Codex source checkout at `D:\Park\codex` provided further context:

- `codex-rs/core/src/tools/handlers/multi_agents_spec.rs` marks the V2
  `message` parameter with `.with_encrypted()`.
- `codex-rs/core/src/tools/handlers/multi_agents_v2/spawn.rs` supplies
  `prompt: None` in its collaboration tracking data and emits
  `subAgentActivity` for the visible lifecycle.
- `codex-rs/core/src/tools/handlers/multi_agents_v2.rs` preserves encrypted
  messaging unless the call is explicitly classified as plaintext.
- `codex-rs/core/src/tools/router.rs` recognizes that plaintext classification
  from the response's `encrypted_function_args` metadata.

No supported app-server display option was identified that supplies the
missing plaintext for these encrypted records. Recheck this against the
installed version before implementation; the local source checkout and the
bundled executable are separate references.

## Possible future approaches

### A. Report a user-visible description alongside native tasks

Register an application dynamic tool such as:

```text
report_task_description(task_path, description)
```

The parent agent reports a readable description when creating a native child.
NiumaTerm associates it with the child thread, persists it, and displays it
above the transcript as "Task description".

This preserves native Codex task scheduling and requires less integration
work. The description is separately generated text, not a guaranteed verbatim
copy of the encrypted prompt. Calling the reporting tool is model-dependent;
missing descriptions must remain an explicit supported state.

Before implementation, define how descriptions arriving before child discovery
are matched, how nested task paths are scoped to a parent session, and how
follow-up descriptions are distinguished from the initial one. Ordinary
conversation text must not be guessed into a task description.

### B. Let NiumaTerm create tasks and retain their submitted prompts

Provide an application dynamic tool such as `spawn_task(prompt)`. Its handler:

1. Retains the readable input supplied to the tool.
2. Creates a thread with `thread/start` and submits the input with `turn/start`.
3. Persists the relationship between parent, child, and submitted prompt.
4. Shows the retained prompt as the initial task instructions after both live
   creation and later restoration.

This can display the exact input that NiumaTerm submits. It also makes
NiumaTerm responsible for task orchestration: context inheritance, parent and
child tracking, completion delivery, follow-up messages, cancellation, failed
starts, and restoration. App-created threads must not be assumed to have the
same native descendant metadata as Codex-created children.

The project already uses Codex dynamic tools for team moderation; see
[the registration and callback handling](../../crates/agent/src/codex/app_server/team.rs).
That provides an existing integration point, not an implementation of task
creation.

### Selection criteria

- Prefer A if the requirement is to explain what each native subtask is doing.
- Consider B if displaying the exact submitted prompt is essential and owning
  task orchestration is acceptable.
- Revisit upstream support for a readable display field before choosing either.
- Neither approach recovers original text from previously recorded
  encrypted-only task bodies using the data inspected here.

Implementation remains deferred; neither approach has been selected.
