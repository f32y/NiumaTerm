## 1. Establish canonical session state

- [x] 1.1 Record the current startup, controller/content mutation, attachment cleanup, and application discovery paths in `docs/research/agent-session-controller.md`; verify every production path has an identified session or presentation responsibility, and rerun the existing controller and ordinary-session scenarios to establish the implementation baseline.
- [x] 1.2 Introduce `ConversationState` in `nmt_agent` using the existing indexed transcript storage for entries, turns, observed outcomes, usage, compaction, and attachment references; adapt the current pane to this single data owner and verify replay, duplicate item IDs, turn boundaries, and missing usage retain their existing interpretation.
- [x] 1.3 Concentrate coupled controller and conversation changes behind session operations, preserving bounded batches, adjacent-delta merging, and generation checks before each event; verify a mixed text/interaction batch publishes consistent content and delivery state, and events from an obsolete backend cannot mutate either.
- [x] 1.4 Move readiness completion, replay ordering, accepted-message confirmation, and unanswered-prompt recovery out of renderer callbacks; verify repeated readiness preserves an active turn and selected settings, queued messages are confirmed once, and recovery uses admitted output without requiring paint or animation progress.

## 2. Introduce independent session execution and ownership

- [x] 2.1 Add `AgentSession` in `nmt_agent_ui` to host the existing controller, canonical content, effective launch/workspace state, and asynchronous startup/inbox work; migrate the ordinary creation path and retire its pane-owned workers, verifying one provider start and one event consumer per session while `nmt_agent` remains independent of GPUI.
- [x] 2.2 Add runtime `SessionId`, logical ownership, weak or borrowed read access, and a command binding generation; verify multiple readers cannot acquire send access, detached composer callbacks are rejected, and presentation replacement preserves provider identity and backend generation.
- [x] 2.3 Separate creation from attachment in the ordinary tab and pane paths, with the logical tab retaining the session; verify detaching all views leaves the session processing events, reattachment reads the same conversation, and attachment does not send, resume, or start provider work.
- [x] 2.4 Move explicit close, pending-task invalidation, and backend release into session lifetime management; verify owner close prevents later commands and maintenance restart, stale observers cannot keep execution alive, and closing one shared-host user leaves the other running.
- [x] 2.5 Add weak session registration before startup and removal on logical close, keeping application route identity separate from presentation entities; verify lookup follows a replacement view, closed sessions disappear, and registry entries do not retain their resources.

## 3. Adapt presentation and attachment resources

- [x] 3.1 Convert `TranscriptView` to borrowed canonical content with generation/revision and bounded range invalidation, retaining its own layout, folding, selection, scroll, and animation state; verify two readers maintain independent presentation, missed revisions recover from current content, and streaming does not copy the full conversation for each delta.
- [x] 3.2 Route ordinary composer and provider controls through the current command binding and apply draft clearing, focus, scrolling, and feedback from returned outcomes in the view; verify rejected submissions preserve their draft, supported busy-input actions retain their existing behavior, and delayed callbacks cannot target a replacement binding.
- [x] 3.3 Transfer accepted image resources and required scratch-file lifetime to the session while leaving unsent resources and GPUI previews in views; verify accepted images survive presentation replacement and delayed provider reads, rejection preserves pending images, and closing a session removes only its own scratch resources.
- [x] 3.4 Adapt child/workflow details to scoped session content and session-owned refresh work with reader visibility interest; verify parent identity and late-result generation checks, separate root/detail content, existing unavailable states, and unchanged refresh cadence without overlapping loops when two readers attach.

## 4. Migrate application consumers

- [x] 4.1 Move lifecycle/activity subscriptions and notification navigation to session identity with application-owned preferences and routes; verify a completion generates one notification with zero or multiple attached readers, attaching a view does not repeat it, and navigation reaches the current ordinary tab.
- [x] 4.2 Discover installation-maintenance participants from the registry and compute readiness from canonical session/backend state; verify sessions without renderers participate, multiple readers produce one participant, and active work, compaction, interactions, and pending operations preserve current readiness decisions.
- [x] 4.3 Preserve wait-until-idle, approved stop-now, recovery identity checks, and independent restoration through session operations; verify mixed readiness across participants, explicit close during maintenance prevents restart, temporary participant handles are released, and compatible shared-host replacement retains the required host reference.

## 5. Verify behavior across the ownership transition

- [x] 5.1 Add a deterministic lifecycle scenario that starts once, detaches every renderer while retaining the logical owner, processes output and an interaction, attaches a new renderer, and closes the owner; verify unchanged provider identity, retained content, one accepted response, one event consumer, and final resource release.
- [x] 5.2 Exercise ordinary conversation creation, switching, history/resume, supported branching, approval/question recovery, and provider-specific busy input after the migration; verify launch-setting precedence, configured versus active workspace, input-history scope, protected credential handling, and existing saved-tab interpretation remain unchanged.
- [x] 5.3 Validate an isolated NiumaTerm launch with `--testing`, covering a quiet window receiving asynchronous output, a long mixed transcript, images, child/workflow details, and view replacement; record observed repaint, scrolling, folding, selection, and cleanup outcomes, including evidence that each streaming update avoids a full-content copy.
- [x] 5.4 Run focused tests for the changed session, transcript, and application integration paths, then the repository-required formatting and clippy checks for affected workspace members; verify they pass and inspect the production code for remaining pane-owned execution workers, mutable content duplicates, and application discovery through panes.

## 6. Document acceptance and the Team prerequisite

- [x] 6.1 Update `docs/research/agent-session-controller.md` with the implemented ownership model, public operations, lifetime rules, and observed verification results; verify its paths and responsibilities match the final code and distinguish unexecuted scenarios from completed validation.
- [x] 6.2 After the preceding acceptance tasks pass, reconcile `add-agent-team-tab` planning documents to reference this prerequisite and reuse only verified session-extraction work; verify Team-specific implementation and validation tasks remain incomplete, no Team behavior is claimed, and both changes pass OpenSpec validation.

Validation note (2026-09-11): all implementation and acceptance tasks passed. The dependent plan was reconciled, and both changes passed strict OpenSpec validation after adding the requested `agent-session-ownership` delta and removing `skip_specs: true`. Team-specific work remains paused. See the research document for observed results.
