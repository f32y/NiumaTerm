## Why

Agent execution already has a reusable `SessionController`, but startup, event consumption, retained conversation content, and application-level discovery still depend on `AgentPane`. Giving sessions an independent owner before adding Team discussions makes it possible to change their presentation without restarting a provider, losing local history, or introducing another sender.

## What Changes

- Introduce an application-owned `AgentSession` around the existing controller, with one provider event consumer and an explicit lifetime independent of its attached views.
- Move retained root conversation content, turn outcomes, usage, and accepted attachment ownership out of `TranscriptView`; keep drafts, editors, selection, scrolling, folding, animation, and render caches in views.
- Apply provider events and delivery bookkeeping together before publishing changes. Views observe completed state transitions and cannot be required to finish startup, confirm accepted messages, or settle recovery.
- Give the application a session registry for installation maintenance, activity, and navigation. Registry lookup and presentation subscriptions do not create another command owner or retain otherwise closed sessions.
- Adapt ordinary Agent Tabs and child/workflow detail views to the shared session and content interfaces, preserving provider-specific busy input, history, supported branching, approvals, questions, images, and display behavior.
- Preserve current launch settings, protected credential handling, workspace access, tab persistence, update choices, and shared-host cleanup. Keep provider integrations in `nmt_agent` independent of GPUI.
- Deliver this as a prerequisite for `add-agent-team-tab`. The exit condition is a session that survives presentation replacement with the same provider identity, one content owner, and one event consumer, while ordinary-tab behavior remains unchanged.

## Capabilities

### New Capabilities

- `agent-session-ownership`: Independent logical lifetime, canonical content, command bindings, resource release, scoped readers, and application discovery for ordinary Agent sessions. This records the implemented ownership behavior without adding a user-facing operation. Team creation, transfers, scheduling, durable records, and permission controls remain in `add-agent-team-tab`.

### Modified Capabilities

None. Existing requirements for ordinary Agent Tabs, transcripts, attachments, history, workspace access, child/workflow views, credentials, and provider updates retain their meaning. The added ownership capability records structural acceptance conditions alongside these unchanged requirements.

## Impact

- `crates/agent/src/session`: reuse the controller, lifecycle, delivery, input, child, workflow, and restore logic; concentrate coupled state transitions behind operations used by both ordinary views and later session owners.
- `crates/agent/src/transcript`: reuse indexed conversation storage for session-owned content and incremental updates.
- `crates/agent_ui`: separate `AgentSession` hosting from `AgentPane` presentation, including asynchronous startup, content access, attachments, and view subscriptions.
- `crates/app`: change update discovery, lifecycle/activity consumers, background detail access, and notification navigation to session handles.
- Existing regression coverage and deterministic provider support: add focused lifetime, ordering, content-sharing, and cleanup scenarios. Reuse the six controller tests already present in the working tree; their earlier results do not validate the future ownership changes.
- No new external service, storage format, provider feature, or application setting is introduced. The dependent Team change remains paused and incomplete until its own implementation is requested and verified.
