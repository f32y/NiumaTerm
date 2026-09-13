# Design issue repair progress

Source: `design-issues-2026-09.md`. Starting revision: `bef72482` on `dev`.
Each completed independent repair is committed separately. Existing skill,
planning, and OpenSpec changes are outside these commits.

| Issue | Repair | Commit | Validation |
| --- | --- | --- | --- |
| 1.1 | Cancel and join the remote host | `8d433e12` | Idle local WebSocket disconnect regression; 19 network unit tests |
| 1.2 | Report rejected remote input and resize | `03db8136` | Closed command receiver produces BrokenPipe; 20 network unit tests |
| 1.3 | Queue terminal replies behind pending input | `dd8fd4b6` | Partial-write continuation and ordering; 184 terminal unit tests |
| 1.4 | Keep DeepSeek startup locks local to a launch key | `15a48294` | Another launch proceeds while the first slot is busy; 65 DeepSeek tests |
| 1.5 | Return missing ConPTY library errors | `3d58d56b` | Child process with restricted DLL lookup returns NotFound; 7 console tests |
| 1.6a | Read workflow transcripts outside the cache lock | `8ad8dd82` | 24 workflow tests, including retry and revision invalidation |
| 1.6b | Persist update caches outside the state lock | `52ed772b` | Update state remains readable while persistence waits; 14 update tests |
| 1.6c | Allow Claude deadlines during blocked stdout delivery | `2b4a464e` | Blocked callback regression; 62 stream tests |
| 1.7 | Close connections when subscriptions lose output | `5a105450` | Stream-loss signal follows queued output; network tests |
| 1.8c | Wake and join subscription bridge workers | `9f643c61` | Idle worker cleanup; real console detach, reattach, and exit |
| 1.8d / 1.9 PTY spawn | Own and join terminal workers | `56f04997` | Worker resources released before close returns; 185 terminal tests |
| 1.8a,b / 1.9 drain spawn | Cancel and join remote client and drain workers | `bbf8917b` | Idle client and drain shutdown; network and protocol tests |
| 1.9 pipe errors | Tolerate a closed error receiver | `55443e2c` | Closed pipe and error receiver do not panic the reader |
| 1.9 IPC spawn | Propagate IPC startup failures | `0e94a2e5` | 8 application IPC parsing tests; startup-error path reviewed |
| 1.12 | Report rejected hook path initialization | `76dd45e2` | Repeated initialization is rejected and the first path stays in child environments |
| 1.11 reconnect | Share capped delays and retry immediately | `73c828ef` | First reconnect reaches a local listener within one second; regression failed before repair |
| 1.11 sequence | Correct the checkpoint sequence boundary | `ffd5c1a6` | Checked against output splitting and checkpoint filtering |
| 1.8e | Cancel and join DeepSeek downlinks | `b0542a3d` | Stalled handshake and idle reader close within one second and release delivery resources |
| 1.10 | Deliver background read failures | `99ca5966` | Eight failed reads report completion; discovery and child transcript error states checked |
| 1.8f | Startup output is parsed only until the address is announced | Already fixed before this task | Current stdout reader checked; no duplicate change |
| 4 graphics | Remove unused resize machinery and display dimensions | `54248425` | 184 terminal tests and 16 image conversion, placement, and lifetime tests |
| 4 cursor geometry | Remove obsolete charset and boundary movement code | `e720a6a3` | 184 terminal tests |
| 4 events | Remove 34 unused terminal events and old search state | `8529ab41` | 184 terminal tests; producers and consumers searched |
| 2.1 | Capture private frame metadata and revisions together | `ba8f79ed` | Buffer reuse updates title, cells, and revisions; terminal and application frame tests |
| 3.2 | Share key-result reactions in the view | `b27dd433` | Escape acceptance and typing scroll behavior; two view tests |
| 2.2 | Own ConPTY resize recovery in one state machine | `44eadb0b` | Scroll-up runs once; late typing is unchanged; active-screen and wrapped-row tests |
| 2.4 settings / 6.3 seeding | Retain initialization policy as one enum | `b66c8bda` | Session readiness, resumed reviewer settings, and history restoration |
| 2.4 Claude | Represent idle, pending, and running turns explicitly | `d4a86976` | 62 stream tests, including queued and adopted turns |
| 2.3 | Own conversation settings and catalogs in the controller | `5f690500` | Tier fallback, branch setup, restart cleanup; 234 Agent view tests |
| 2.5 / 6.4 input dispatch | Use one identified question path for all providers | `53475eac` | Stale and duplicate requests, response retries, secret cleanup, editor replacement |
| 6.1 | Separate activation reactions from focus restoration | `2f05344d` | 7 shell tests; activation, reorder, rename, and settings-close call sites reviewed |
| 6.7 | Reject unsupported Windows bootstrap input | `f78cf388` | Managed and unmanaged paths reject before launch; regression failed before repair |
| 2.6 directories | Keep availability marks attached to path identities | `c326556f` | Reordered and replaced directory rows retain correct marks |
| 2.6 question widgets | Key presentations and active selection by request generation | `c27179b6` | Unshown batches and reused positions do not retain stale editors; 7 question view tests |
| 2.6 image preview | Keep image, origin, and fade in explicit lifecycle states | `dc9879da` | Closing retains the image; conversation reset releases the preview; geometry checks |
| 2.8 profile dialog | Keep editable credentials in dialog-owned entities with weak input callbacks | `428ac8e8` | Independent saves and release after rendering and cancelling |
| 2.8 font picker | Scope interactive controls to rendered windows; share metadata | `f01e3584` | Two-window selection isolation, settings synchronization, and release |
| 2.8 opacity | Scope sliders and subscriptions to each rendered field | `44f0a1bf` | 27 settings tests; existing keyed-state mechanism reused |
| 2.9 | Build complete sidebar values separately from workspace metadata | `1811453a` | 42 workspace and sidebar tests |
| 3.1 | Share tab insertion and publish remote tabs immediately | `5ba9521f` | 7 launch/restore tests; snapshot participation checked |
| 3.3 | Share notification cleanup and display updates | `95828a5e` | 13 notification tests; scheduling and delivery differences checked |
| 3.4 | Use configured cancellable Codex usage queries | `13752707` | Real child protocol exchange, prompt cancellation, profile selection, and stale result rejection |
| 3.5 | Share hook registration storage and status | `9dd4124e` | 17 hook tests, including stale detection and preserved user entries |
| 3.6 | Borrow restoration state once | `77089dab` | 30 Claude task tests |
| 4 ownership | Remove the unused ownership store and transfer API | `d5364e72` | 23 team tests; production callers searched |
| 4 colors | Remove unused theme fields and built-in entries | `56e755b1` | 49 configuration tests and built-in theme checks |
| 4 constants | Remove unused render color constants | `66ddabe5` | Repository hooks |
| 4 cursor blink | Remove unread settings and snapshot data | `bb7854ba` | Cursor tests and exact serialized-default example |
| 4 read-only input | Restrict the rejection override to test support | `1db6d46a` | 102 terminal view tests; one external-relay test ignored |
| 6.6 revision | Store the PTY revision as an ordinary counter | `d8320283` | 29 PTY tests |
| 6.6 prompt | Publish the prompt-open bit atomically | `6c58c93a` | Prompt lifecycle and 11 session tests |
| 6.6 pages | Borrow the UI-owned page cache without a mutex | `af3f1adb` | 37 session tests and all application targets checked |
| 6.6 staging | Keep pending block batches in the PTY event proxy | `a7e56351` | One thousand batches flush without growth; graphics publication ordering |
| 5.2 | Move the session hub into the remote network package | `7fc1edab` | Real ConPTY detach, resized checkpoint, reconnect, and exit; controlled PTY and encrypted channel tests |
| 5.1 colors | Replace regex color parsing with checked ASCII integer parsing | `69854aa7` | RGB/RGBA, both scales, invalid sizes, and Unicode digit rejection |
| 5.1 dependencies | Extract profiles and make application storage optional | `d4f57f8d` | Encrypted storage, migration, known ciphertext, concurrent saves, and 185 terminal tests; engine dependency tree excludes profile storage |
| 5.5 normalization | Normalize appearance while loading configuration | `98d57d29` | Invalid and non-finite settings normalize before UI creation; 27 settings tests and exact defaults |

All listed commits passed the repository hooks. Windows validation was used;
Unix source changes have not been executed on a Unix host. Tests requiring an
external relay remain ignored.

## Additional completed repairs

| Issue | Repair | Commit | Validation |
| --- | --- | --- | --- |
| 6.2 usage | Always show usage totals | `9b814239` | Nine usage tests |
| 6.2 completion | Remove unused completion flag | `5c565a64` | Hooks |
| 6.2 confirmation | Evaluate close preference at caller | `219dea39` | Hooks |
| 6.2 export | Fix selection export options | `744f6b82` | Three block-copy tests |
| 6.3 selection | Match selection kinds once | `ff2582b9` | Wrapped-word and line selection regressions |
| 6.3 / 6.4 team | Exhaustive team dispatch | `19158217` | Claude child approval delivery |
| 6.4 approval | Return explicit approval outcomes | `b597e6fc` | Thirteen input lifecycle tests |
| 6.3 status | Match team member runtime status once | `fc71af42` | Four team view regressions |
| 5.4 launch | Launch from the shared profile | `c0d3936f` | Ten launch tests |
| 5.4 identity | Share profile, session, and task identity | `43023109` | Profile/session storage labels, task restoration, 236 Agent view tests |
| 2.7 save | Return save errors without editing global state | `58777fd7` | Invalid TOML and blocked destination retry |
| 2.7 editing | Keep temporary state with its settings window | `ea655e83` | Two rendered windows; close releases editor and rejects late updates |

Identity decisions: the update service's two-provider type remains separate
because DeepSeek does not use that service. Background task reference variants
retain their provider-specific data. The input crate remains an independent
encoding and test boundary, as permitted by 5.3.

## Further completed repairs

| Issue | Repair | Commit | Validation |
| --- | --- | --- | --- |
| 2.7 / 5.5 | Own Config and validate edits through named operations | `6b19e540` | 38 settings tests, including stale indices and invalid appearance edits |
| 2.2 decision | Record resize recovery ownership | `2da601ea` | Document-only commit |
| 5.4 decision | Record shared identity choices | `37e1b41e` | Document-only commit |
| 2.7 decision | Record settings ownership | `e79398ab` | Document-only commit |
| 4 annotations | Keep response annotations with the composer | `432302a9` | Fresh and restored annotation parsing |
| 3 commands | Share adapter command catalog | `ba6d8373` | Composer and successful-action history tests |
| 3 versions | Build unsupported version states once | `fc9b0f03` | Four provider update tests |
| 3 panel equality | Compare optional targets directly | `85e81faf` | Hooks |
| 3 descriptions | Share close confirmation descriptions | `41fb6f10` | Six shell tests |
| 4 keyboard modes | Use application cursor and keypad flags | `ecae989d` | 22 terminal input and interaction tests |
| 4 keyboard events | Preserve text release and repeat rules | `9abd575b` | 31 encoding tests |
| 4 keyboard integration | Connect requested reporting modes, key phases, and IME text | `c6660994` | 104 terminal view tests, one external-relay test ignored; 186 terminal library tests |
| 4 keyboard policy | Remove unused release policy and wrapper tests | `eb24726d` | 29 encoding tests |
| 6.3 profile controls | Iterate launchers and match exclusive control groups | `a1f891f9` | Rendered profile dialog lifecycle test |
| 6.5 updates | Notify update observers instead of refreshing every window | `79ff5d66` | Two observers receive check and update transitions; 16 update tests |
| 3 panel targets | Retarget workflows and background tasks together | `9c292428` | Leaving an Agent tab clears both targets; repeated sync is idle |

## Verified limits and retained designs

- 3.6 provider-specific root reset behavior remains distinct; a generic optional registry wrapper would add forwarding without consolidating those transitions.
- 2.4 update readiness needs no change: `ConversationWork.empty` describes transcript content and is independent of approval, branching, and compaction.
- 2.6 keyboard builder needs no change: its private derived flags are initialized once in a local immutable value and never mutated. Event reporting also depends on repeat/release state.
- Question editor positions are local to one immutable request generation; replacing a request replaces its keyed presentation as a whole.
- The workflow and background-task views retain separate visibility and detail-reset logic. Their optional target equality is shared language behavior, and their active-tab synchronization now has one owner.
- Theme selection changes global colors and native translucency, so it still refreshes all windows. Remote device revocation and host removal change shared disk-backed lists that can be visible in multiple settings windows; their global refresh is retained.
- Keyboard encoding uses the event information GPUI exposes. Its keystrokes do not identify physical key location or modifier-only keys; this repair does not invent left/right or keypad identities.


## Final completed repairs

| Issue | Repair | Commit | Validation |
| --- | --- | --- | --- |
| 4 Windows helpers | Remove unused launch wrappers | `663044e2` | Managed child ownership and four real PowerShell scenarios |
| 4 Unix helpers | Keep unused native helpers private | `337b15a8` | Hooks; Unix execution unavailable |
| 4 package launchers | Keep package command details internal | `c3233cf2` | Hooks |
| 4 version probes | Keep DeepSeek release probes in tests | `c14b086f` | Pinned release classification; live probe ignored |
| 4 process probes | Restrict bounded process execution | `8a1de3b7` | Four launcher tests |
| 4 Claude history | Restrict backend history loaders | `5cb5c9c7` | 38 history tests |
| 4 backend methods | Restrict backend-only operations | `a8ad8162` | 97 session tests |
| 4 monitor | Restrict monitor bookkeeping | `83158602` | 597 core tests; one live probe ignored |
| 4 update clock | Keep clock injection private | `3889609f` | Hooks |
| 4 saved indices | Remove unused index wrappers | `0852bd83` | Hooks |
| 4 placeholders | Remove unused placeholder codecs | `475e724c` | 22 placement tests and 28 frame extraction tests |
| 4 input builders | Keep encoding builders private | `042b9e75` | 29 encoding tests |
| 3.7 environment | Share native environment override lookup | `b7f35dd8` | Last override and Windows name comparison |
| 3 IPC | Share bounded message reading | `15d06c2c` | Exact limit accepted; excess, endless, and failed streams rejected |
| 6.2 PTY management | Pass the actual job and reject failed containment | `68c96ec1` | Managed tree ownership and release; four PowerShell scenarios |
| 3 pipe workers | Share lifetime ownership and cancel blocked writers | `26dd2d50` | Full-pipe close failed before repair and passes afterward; readiness tests and four PowerShell scenarios |
| 3 process lifetime | Share cleanup and descendant counting | `cb25ca32` | 49 platform tests; three platform cases ignored |
| 3 window order | Use one last-active-first ordering | `efc1b1e7` | Hooks |
| 3 IPC routing | Share registry snapshot and accepted-action dispatch | `4dfde1a0` | Hooks |
| 4 session transitions | Restrict internal session transitions | `9e54da5a` | 97 session tests |
| 4 team scheduling | Restrict context and dispatch bookkeeping | `ec3fec04` | Hooks |
| 4 team test support | Remove unused wrappers and gate test-only operations | `e7672844` | 23 team scenarios |
| 4 transcript updates | Restrict task and transcript bookkeeping | `75b8bd7a` | Hooks |
| 4 usage helpers | Keep usage collection and merging internal | `375e58a1` | Hooks |
| 4 provider diagnostics | Keep diagnostic parsers private | `954c3501` | Hooks |
| 4 command normalization | Keep normalization private | `00701b30` | Hooks |
| 4 process state | Restrict process bookkeeping | `f9a30924` | Hooks |
| 4 history identity | Keep history partition keys internal | `1dbacf97` | Hooks |

## Final scope decisions

- The remote list and kill operations are exercised by `remote_net/tests/host_e2e.rs`; they remain supported. `Detach` remains a recognized protocol message. Removing or reordering serialized enum variants would change the stored numeric identifiers used by existing peers.
- `CodexProviderConfig` is used by `LaunchConfig` and the provider recovery integration tests. Public types reached through public fields or return values remain public even when consumers rely on inferred types rather than spell their names.
- The remote client now has one runtime creation path; the three original copies were removed while giving its worker explicit ownership.
- Unix APIs remain available. Shared environment, IPC, and process code was relocated; unused Unix implementations were retained with narrower visibility.
- 5.6 `Colors`: unused chrome fields were removed. The remaining fields deserialize existing theme keys; splitting the palette would add nested access and format mapping without changing validation or ownership.
- 5.6 `AppearanceConfig`: normalization and named settings mutations now enforce values at their owner. Existing flat configuration keys are retained; font, tab, and window preferences have independent consumers and do not require a new persisted nesting format.
- 5.6 `PtyPipe`: resize recovery now owns its state machine and block publication stays on the PTY thread. The remaining prompt sniffer, engine mode, launch directory, and alternate-screen tracking are private and updated in that loop. A field-only grouping would not transfer an additional invariant.
- 5.6 `Shell`: render-deferred focus, resume, close, and root observation have separate triggers and completion rules. Settings, panels, activation, and notification reactions now have explicit owners; a bag of pending fields would add access paths without unifying those rules.
- 5.6 `TranscriptView`: preview lifetime is now explicit. Viewport width, height, and origin are measured together; font changes trigger row remeasurement, and picker reading position is saved and restored separately. These distinct update rules stay with the view.
- 5.6 DeepSeek session: tool calls and usage projections already have their own trackers. Inbox identities can outlive a turn, and approval/question requests are independently resolved. They must not all be cleared by a generic turn reset.

All confirmed behavior and ownership repairs are committed. The grouping-only
suggestions above were evaluated and retained as described. No application
window was launched for this task; validation used unit, GPUI, protocol, and
isolated child-process tests. Unix execution and tests requiring external
services remain outside the completed validation.

## Final validation

- Joint library run: app 344 passed / 2 ignored; agent 597 passed / 1 ignored; configuration 43 passed; input 29 passed; profiles 9 passed; remote network 23 passed.
- Platform run: 49 passed / 3 ignored across unit and integration targets.
- Terminal integration: four real PowerShell input and resize scenarios passed after the final pipe-worker changes.
- Every implementation and decision-record commit passed the repository hooks without bypasses.
- Application entry-point suite: 245 passed. Remote integration targets: 7 passed / 9 external-service cases ignored.
