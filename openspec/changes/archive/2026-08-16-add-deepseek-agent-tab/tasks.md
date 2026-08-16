## 1. Harness identity

- [x] 1.1 Add the DeepSeek variant to `AgentKind`, `AgentProfileKind`, and `ProviderKind`, with their serialized names and the conversions between them
- [x] 1.2 Add its `Capabilities` constant in `crates/app/src/agent_pane/capabilities.rs`, with every capability this slice does not provide set false
- [x] 1.3 Add the `Backend` variant and its `RecoveryIdentity` case, leaving unsupported methods as explicit stub arms
- [x] 1.4 Data-drive the two hand-written UI harness-kind lists so a registered kind appears without a third hand-edited list (R5)
- [x] 1.5 Exclude harnesses with no vendor-managed installation from the Agent General update rows and the shared Check action

## 2. Host process

- [x] 2.1 Resolve the `dsh` executable, and report a missing installation as its own distinct failure
- [x] 2.2 Spawn `dsh web --port 0` under the existing Windows Job Object process-tree containment, capturing stdout and stderr
- [x] 2.3 Parse the address line the host prints on stdout, with a bounded wait whose expiry is the start-failure signal and which surfaces the host's own output
- [x] 2.4 Read the installed `dsh` package version and compare it against this build's supported range, reporting an out-of-range version without blocking the tab
- [x] 2.5 Hold the host as one application-wide resource: start it with the first DeepSeek tab, keep it across tabs, terminate it and its process tree with the last
- [x] 2.6 Detect host exit while tabs are open and notify every affected tab

## 3. `/api` client

- [x] 3.1 Unary call helper: `POST /api/<method>` with the request message and `application/json`, decoding the response into an ok value or a typed error
- [x] 3.2 Implement the four methods this slice needs: `host.describe`, `session.create`, `session.prompt`, `session.cancel`
- [x] 3.3 Open both WebSocket downlinks with `tokio-tungstenite` and decode their frames
- [x] 3.4 Reconnect a dropped downlink, using the per-session sequence baseline the stream emits on open
- [x] 3.5 Route frames to the owning tab by session id, ignoring events for sessions no tab owns
- [x] 3.6 Ignore unknown event and frame types rather than failing, so a harness release that adds one does not break the tab
- [x] 3.7 Check the carrier receipt on any answer to an answerable frame instead of firing and forgetting

## 4. Transcript

- [x] 4.1 Create one session per tab through `session.create` and hold its id
- [x] 4.2 Submit prompts and render the user's own message, filtering out the messages the harness injects alongside it
- [x] 4.3 Stream assistant text and reasoning as separate transcript content, keyed by a synthetic id derived from turn, step, and block index
- [x] 4.4 Fold the harness's completed message into the streamed entry through the existing merge path, without discarding streamed fields
- [x] 4.5 Record agent errors and stream failures as transcript error entries
- [x] 4.6 Show tool calls as transcript rows, dispatching on the host-computed render card rather than the tool name
- [x] 4.7 Track calls by id so a result completes the row its call opened, and ignore a result whose call this tab never saw
- [x] 4.8 Complete a failed call with the text the model received, which is all a failure leaves behind
- [x] 4.9 Live test: a real turn's commands, file diff, and other calls all reach the transcript and close

## 5. Approvals

- [x] 5a.1 Recognize `approval/requested` for this session and raise it as a transcript approval, holding the correlation and audit identities an answer needs
- [x] 5a.2 Answer with the harness's own outcome vocabulary, mapping the card's decisions onto it and stopping the turn when the user cancels
- [x] 5a.3 Clear the pending request when the harness reports it resolved, and when the turn ends under it
- [x] 5a.4 Hide the session-scoped allow control for a harness that cannot express it, through a named capability rather than a kind check
- [x] 5a.5 Live test: an approval is raised, answered, the turn continues, and the approved command's effect actually lands

## 6. Turn control

- [x] 5.1 Drive run status and step progress from the harness turn and step lifecycle
- [x] 5.2 Stop a running turn through `session.cancel`, offering the action only while a turn runs
- [x] 5.3 Present a stopped turn as interrupted while keeping its partial answer, and distinguish it from a completed and from a failed turn

## 7. Verification

- [x] 6.1 Unit tests for the frame-to-transcript mapping over the captured frames: streamed text and reasoning, completed-message folding, injected-message filtering, and a cancelled turn that carries no completed message
- [x] 6.2 Unit tests for session-id routing and for ignoring unknown event types
- [x] 6.3 Exercise the adapter end to end against a real host: prompt, watch text and reasoning stream, stop mid-turn and confirm the partial answer survives, then confirm it accepts another prompt
  - Covered by `crates/agent_utils/tests/deepseek_live.rs` (ignored by default;
    needs an installed `dsh` and a credential), and confirmed in the running
    application: a manually added DeepSeek profile opens a tab that prompts,
    streams, and stops.
- [x] 6.4 Confirm the failure paths: `dsh` absent, a host that cannot start, and a second tab reusing the running host
- [x] 6.5 Run one shell-using prompt against the web profile to determine whether the Windows sandbox failure seen in earlier measurements reproduces here, and record the result
  - It does not. The web profile composes `dsh-tool-pwsh`, so the model reached
    for PowerShell and the command succeeded first try; the earlier failure
    belonged to `dsh-bash-sandbox` in a composition this integration never
    loads.
