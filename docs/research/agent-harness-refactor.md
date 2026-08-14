# Agent Harness Refactor Research

| Field | Value |
| --- | --- |
| Status | Research; no implementation decision recorded yet |
| Date | 2026-08-14 |
| Scope | Reducing the cost of adding a third and later agent harness |
| Companions | [`agent-harness-integration-requirements.md`](../agent-harness-integration-requirements.md), [`deepseek-harness-integration.md`](./deepseek-harness-integration.md) |

## 1. Summary

Adding a third harness today touches **93 logical branch sites**. The compiler
forces an update at 52 of them. The other **41 stay silent**, and a new harness
inherits Codex behavior at some of them and Claude behavior at others,
incoherently.

The count is not the problem worth fixing. The silence is.

Two findings shape the recommendation:

- **A trait for `Backend` is not worth it.** Only 6 of its 18 methods have
  identical signatures on the underlying types, 9 already carry a stub arm, one
  is not object-safe and has a caller with no session at all, and construction —
  the highest-friction axis — cannot be expressed as a trait method. Details in
  section 4.
- **The real duplication is one layer up**, in identity and capability checks
  scattered across the pane, composer, and settings UI. That is where the
  silence lives and where a data-driven replacement pays for itself.

Recommended order: close the silent sites and the enum leaks first (R1–R3),
land the third harness, then decide whether the identity enums still hurt (R4).

## 2. Where the 93 sites are

| Bucket | Sites | Compiler catches | Silent |
| --- | --- | --- | --- |
| Capability — "does this harness support X?" | 28 | 14 | 14 |
| Identity and serialization | 21 | 11 | 10 |
| Presentation — icons, labels, ordering | 16 | 10 | 6 |
| Launch and configuration | 11 | 5 | 6 |
| Protocol dispatch — calling a different session type | 12 | 12 | 0 |
| Other — hardcoded defaults and two-element lists | 5 | 0 | 5 |
| **Total** | **93** | **52** | **41** |

Of 247 raw occurrences, 83 are in tests and 17 are `#[cfg(test)]` arms inside
`backend.rs`; the rest collapse into the 93 logical sites above.

Only the 12 protocol-dispatch sites are irreducible. Those genuinely call a
different concrete session type, and they are all exhaustive matches, so a third
harness cannot slip past them.

### 2.1 There are five identity enums, not three

| Enum | Definition | Spelling |
| --- | --- | --- |
| `AgentKind` | `crates/app/src/agent_pane/profile.rs:6` | `codex`, `claude` |
| `AgentProfileKind` | `crates/config/src/profile.rs:52` | `codex`, `claude-code` |
| `ProviderKind` | `crates/agent_utils/src/update.rs:41` | `codex`, `claude` |
| `BackgroundTaskProvider` | `crates/agent_utils/src/background_task/mod.rs:19` | — |
| `BackgroundTaskRefs` | `crates/agent_utils/src/background_task/mod.rs:63` | — |

Three spellings of one concept, bridged by four conversion functions. The last
two carry different payloads per harness, so they are a separate case and are
not part of the collapse proposal below.

### 2.2 The worst individual sites

- `backend.rs:129` — `match (self, key.provider)` with a `_ => Vec::new()`
  catch-all. Two enums can both grow and this will never warn.
- `session/mod.rs:123` and `:773` — `provider_commands_ready` is seeded from
  "is this Codex". A new harness gets `false`, so the composer waits for
  provider commands that may never arrive; `palette.rs:246` then declines to
  show the loading hint because that is Claude-gated, so the palette simply
  looks empty with no explanation.
- `ui/settings/state.rs:266` and `ui/settings/agent_profile_dialog.rs:428,433` —
  hand-written two-element lists. A third harness would exist in the type system
  and be unreachable from the UI.
- `AgentKind::from_id` (`profile.rs:30`) is deliberately lossy so that a newer
  snapshot degrades to a plain terminal tab. That is correct, but it means a
  forgotten registration silently downgrades an Agent Tab on restore with no
  error, via `tab_surface.rs:30,41` and `persistence.rs:262,335`.

### 2.3 Twenty distinct capabilities are encoded as identity checks

Skills; skills-under-slash compatibility; file rewind; workflows; structured
questions; per-tier sub-model overrides; expandable compaction rows; paged
backend history; filesystem transcript history; resume mechanism; resume
preserves thread settings; resume preserves approval reviewer; repeated `Ready`
during initialization; synchronous provider commands; asynchronous
command-discovery loading state; live background-task query; background-task
transcript fetch; background-task restoration from history; stable session id;
exit-derived events.

Several collapse — the three workflow methods are one capability, the three
background-task ones are arguably two — but the list is the real shape of what
differs between harnesses, and none of it is written down anywhere today.

## 3. Recommended refactors

### R1 — A capability table (highest value)

Replace the capability checks with a struct returned per harness kind:

```rust
pub(crate) struct Capabilities {
    pub skills: bool,
    pub file_rewind: bool,
    pub workflows: bool,
    pub structured_questions: bool,
    pub paged_backend_history: bool,
    pub live_thread_resume: bool,
    // ... one field per capability in section 2.3
}
```

Give it **no `Default` implementation**. A new harness then cannot compile until
every capability has been answered deliberately, which converts all 14 silent
capability checks into a single reviewable struct literal.

Call sites change from `self.kind == AgentKind::Claude` to
`self.caps.file_rewind`, which also reads as the question the code was actually
asking.

Cost: 28 call sites edited, one new file. No behavior change, and the existing
Codex and Claude tests cover it.

### R2 — Close the `Backend` enum leaks

Five production sites pattern-match the `Backend` enum outside `backend.rs`, in
each case to reach a method the enum does not expose:

| Site | Reaches |
| --- | --- |
| `session/mod.rs:653` | Claude `session_id()` as a guard |
| `session/mod.rs:664` | Claude `begin_task_restoration()` |
| `session/mod.rs:683` | Claude `finish_task_restoration()` |
| `session/mod.rs:954` | Codex `resume_thread()` |
| `view/history.rs:90` | Codex `request_more_history()` |

Adding these four methods to `Backend` with stub arms makes `Backend::Codex` and
`Backend::Claude` disappear from everything except `backend.rs` and the spawn
path. The doc comment at `backend.rs:12-14` already claims the pane is
protocol-agnostic; this makes the claim true.

While there, fix `backend.rs:129` so the background-task match is exhaustive
rather than falling through a catch-all.

Cost: about 40 lines, one file plus five call sites.

### R3 — Extract the spawn factory

`session/mod.rs:387-413` builds the backend inline, and the per-harness
preparation around it is spread over `:342-347` (seed flags) and `:368-373`
(pre-resolving `launch.model`, which Claude needs because its CLI bakes the
model into the system prompt). A third harness otherwise wedges itself into
unrelated pane logic.

Move it into `backend.rs` as roughly:

```rust
Backend::spawn(kind, &LaunchConfig, cwd, Option<RecoveryIdentity>, deliver, on_stderr)
```

This is the one place a trait genuinely cannot help — the two spawn functions
take generic `impl Fn` sinks, and resume is a launch flag for Claude but a
protocol request for Codex — so an explicit factory is the right shape.

While there, collapse `RecoveryIdentity` (`backend.rs:6`) from one variant per
harness to `{ kind, id }`. It currently needs a new variant per harness for no
reason beyond spelling.

### R4 — Collapse the identity enums (bigger, decide later)

One `HarnessKind` replacing `AgentKind`, `AgentProfileKind`, and `ProviderKind`
would remove 21 identity sites and four conversion functions.

The obstacle is crate layout: `nmt_agent_utils` and `nmt_config` are both
first-party leaves with no dependency between them, so a shared enum needs a
home. Putting it in `nmt_agent_utils` and having `nmt_config` depend on that
crate is acyclic and puts the harness domain in the crate that already owns it.

Two details to preserve: `AgentProfileKind` serializes `claude-code` into
`config.toml`, so the unified enum needs that spelling as a serde alias; and
`AgentKind::from_id` must stay lossy so an unknown kind from a newer snapshot
still degrades to a terminal tab.

This is the invasive one and the one to skip if the schedule is tight. R1–R3 do
not depend on it.

### R5 — Data-drive the UI lists

`ui/settings/state.rs:266` (seeded built-in profiles) and
`ui/settings/agent_profile_dialog.rs:428,433` (the two "Base Agent" buttons) are
hand-written two-element lists. Iterate over the kinds instead, so a new harness
appears in the UI by existing.

### R6 — De-duplicate the leaked launch constants

- The credential environment variable name is written twice:
  `agent_pane/profile.rs:102-103` and `ui/settings/agent_profile_dialog.rs:368-369`.
- The default executable name is written three times:
  `agent_utils/src/update.rs:57-59`, `ui/settings/state.rs:250-251`, and as bare
  strings at `agent_profile_dialog.rs:172`.

### R7 — Deferred: the updates registry

`AgentUpdates` (`agent_pane/updates/mod.rs:38-43`) has named `claude` and
`codex` fields and a two-element tuple destructure at `:66`. It should be a map
keyed by harness kind, but only a harness with managed CLI updates forces the
issue, and the DeepSeek integration explicitly omits those. Defer until a
harness needs it.

## 4. Why not a trait

The obvious alternative — replace the `Backend` enum with
`Box<dyn AgentSession>` — does not survive the numbers.

- **Only 6 of 18 methods have identical signatures** on the underlying types
  (`process`, `execute_slash_command`, `has_active_operation`, `shutdown`,
  `interrupt`, `respond_approval`). Three have real mismatches: Codex takes an
  extra skill argument on send, `load_background_task_transcript` takes a
  different parameter type per harness, and `session_id` is named `thread_id` on
  the Codex side.
- **`adapter_commands` is not object-safe and has a caller with no session.**
  Both are associated functions with no receiver, and `composer/mod.rs:492-500`
  calls them statically, keyed on `AgentKind`, before any session exists. That
  call site needs a kind-to-value function regardless of what `Backend` becomes.
  The same applies to the per-kind option constants at `composer/mod.rs:517-521`.
- **Nine of 18 methods already carry a stub arm**, and going dynamic would add
  the four downcast-only methods from R2, reaching 22 methods of which 13 are
  defaulted no-ops. A trait whose majority is defaulted no-ops is a capability
  table wearing a trait's clothes, and it hides the question that the `match`
  currently makes greppable.
- **Construction cannot be a trait method.** Both spawn functions take generic
  `impl Fn` sinks, resume is a launch flag for one harness and a protocol
  request for the other, Codex additionally supports resuming an already-running
  session while Claude must respawn, and per-kind launch preparation happens in
  the caller.
- **Three Claude-Code types leak into the shared method signatures**
  (`WorkflowRefreshRequest`, `WorkflowRefreshResult`, `RestoredWorkflowRun`, all
  from `claude_code/workflows/disk.rs`). A neutral trait would have to re-home
  them or make a third harness depend on Claude-Code types.

The genuine shared boundary already exists without a trait, and it is clean:
`JsonLineProcess` framing below, `Vec<chat::Event>` above, everything between
harness-private. Neither concrete session type exposes anything from the middle
layer except the three workflow types noted above.

There is also no async and no `self`-by-value anywhere in `Backend`, so the
enum costs nothing that a trait would save.

## 5. Expected payoff

Today, adding a harness means roughly 50 non-test arms, of which 41 sites can go
wrong silently.

After R1, R2, R3, and R5, it means:

- one `Capabilities` struct literal, which will not compile until every
  capability is answered,
- about 12 protocol-dispatch arms, all exhaustive,
- about 11 launch and environment lines, which are genuinely per-harness,
- a handful of presentation entries that appear in the UI automatically.

Roughly 30 sites instead of 50, and — the part that matters — none of them
silent.

R4 removes about 20 more, at the cost of a new crate dependency edge and a serde
alias.

## 6. Sequencing

The requirements document advises against a broad adapter refactor before a
third integration proves what is genuinely duplicated. R1 through R3 do not
conflict with that advice: they are justified by evidence already visible in the
two existing harnesses, they change no behavior, and the existing Codex and
Claude tests cover them. R4 is the one that benefits from waiting.

Suggested order:

1. R2 and R3 — mechanical, self-contained, make the pane genuinely
   harness-agnostic.
2. R1 — the capability table, which is where the silent-behavior risk is.
3. Land the third harness against the refactored shape.
4. Re-evaluate R4, R5, R6, and R7 with three harnesses in hand.
