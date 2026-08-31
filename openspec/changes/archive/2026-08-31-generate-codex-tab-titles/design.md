## Context

See `proposal.md` for motivation and `specs/agent-conversation-titles/spec.md` for observable behavior.

The Codex adapter currently sends `thread/name/set` with a shortened opening line before the first primary turn. `AgentPane` publishes the accepted name to the Shell through `TitleSuggested`, and `TabManager` already gives a user-authored title priority over provider updates. Codex sessions share one app-server process, while the Host router assigns each registered client one root thread; starting a second thread through the primary registration would replace that root.

## Goals / Non-Goals

**Goals:**

- Keep the primary Codex thread and transcript independent from title generation.
- Match the desktop Codex two-stage interaction: immediate provisional text followed by a concise generated title.
- Bound resource use and preserve a usable title on every failure path.
- Make asynchronous results safe across manual rename, reset, restore, and session replacement.

**Non-Goals:**

- Re-evaluate titles after later topic changes.
- Change Claude Code or DeepSeek title behavior.
- Add title-generation settings or expose generated descriptions.
- Require a second Codex app-server process.

## Decisions

### Use a separate Host registration for title generation

Each generation run registers a temporary client on the existing shared `CodexHost`. Its `thread/start`, `turn/start`, notifications, and unsubscribe traffic therefore have their own root ownership and cannot enter the primary session reducer. A bounded worker waits on that registration and returns one internal result to the primary session.

Using the primary registration was rejected because the Host router treats `thread/start` as a root replacement. Starting another app-server was rejected because it would duplicate process startup, credentials, and provider configuration.

### Show provisional text only in local presentation

After the primary backend accepts the first ordinary prompt, `AgentPane` emits a provisional `TitleSuggested` immediately and marks the conversation as having started its naming flow. The provisional value is built from normalized prompt text and capped at 60 characters. It is not sent through `thread/name/set` until generation settles.

Persisting the provisional value first was rejected because it adds an avoidable provider write and makes generated replacement ordering harder to reason about.

### Run a bounded read-only structured turn

The temporary thread is ephemeral, uses approval policy `never`, read-only sandboxing, low reasoning effort, disabled web search and multi-agent features, and a 30-second deadline. Standard OpenAI Codex profiles request `gpt-5.6-luna` with server fallback allowed. Custom providers retain their configured provider and model so a gateway is not asked for an unrelated built-in model.

The turn requests a structured object containing a title. The prompt asks for at most 36 characters, a short action-oriented title when appropriate, the opening prompt's language, retained issue identifiers, and no surrounding markup or trailing punctuation.

Using the primary conversation model and settings unchanged was rejected because title generation should be cheaper and must never inherit write access or interactive approvals.

### Validate the generation identity before applying its result

Every run carries a monotonically increasing generation ID, the primary thread ID, and the provisional title. The session accepts the result only while all three still describe its active naming run. Reset, restore, shutdown, or manual rename cancels the temporary registration and invalidates the ID.

This check is performed before optimistic UI publication and before `thread/name/set`, so stale work cannot rename another conversation. A user rename also persists through the existing root session after canceling generation.

### Restore the provider's stored name

Codex resume handling emits a title update from the non-empty `thread.name` returned by `thread/resume`. This marks the pane as already named and prevents the next follow-up from starting a first-prompt generation run.

## Risks / Trade-offs

- [A title consumes a small additional model turn] → Use a lightweight model, low effort, a short prompt, structured output, and one run per new conversation.
- [An older or custom app-server may reject ephemeral threads or structured output] → Keep the primary turn independent and persist the provisional title on any generation failure.
- [A temporary client can outlive a replaced pane] → Cancel it on every session replacement path, cap the wait at 30 seconds, unsubscribe its thread, and detach its Host registration.
- [A custom provider may not offer the lightweight built-in model] → Use the profile's configured provider and model for custom routes.
- [Generated output may exceed display limits or contain extra whitespace] → Normalize and validate it before publication, then fall back to the provisional title when unusable.

## Migration Plan

No stored-data migration is required. Existing named threads keep their names; new conversations use generated titles, and restored threads read the name already stored by Codex. Rolling back restores the previous first-line naming behavior without changing transcript data.
