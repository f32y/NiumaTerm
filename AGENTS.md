# AGENTS.md

Repo-level guidance for AI coding agents working in this repository.

## Basic rules

- Limit edits to files needed for the user's request. Other contributors may
  have work in progress; preserve their changes when editing shared files.
- YOU ARE FORBIDDEN TO USE FOLLOWING AI SLOP WORDS: ponytail, seam, fact, parity, envelope, wire, contract
- Always write documents, specs, tests and comments in English.

## Testing application launches

Always pass `--testing` when launching NiumaTerm for manual or automated
validation, for example `target\debug\NiumaTerm.exe --testing`. Without this
flag, the launch may reuse the currently running terminal process instead of
starting an isolated test instance.

## Code comments

Write comments as self-contained explanations of the technical choice: state the
constraint, invariant, tradeoff, compatibility concern, or failure mode that makes
the implementation appropriate.

Do not use passive instruction references such as "the ADR requires this", "the
task says to do this", or "the document/spec requires this" as the rationale for
code. Replace internal ADR, task, phase, design-section, and patch identifiers with
the actual technical reason. Keep an external protocol or API reference only when
it is useful for interoperability, and make the comment understandable without
opening that reference.

## Module organization and imports

In Rust files you add or edit, anchor every `use` path at `crate` or an
external crate name. `use super::...`, `use self::...`, and bare relative
module paths are forbidden. Leave imports in untouched files unchanged.
This rule applies to import paths; visibility markers such as `pub(super)`
and `pub(in ...)` are allowed. Use the narrowest visibility a real caller
needs: private, `pub(super)`, `pub(crate)`, or `pub`.

## Technical taste

Each layer returns the outcome of its own work; callers decide how to
respond.

- Command-style functions (PTY writes, clipboard operations, state mutations)
  return what actually happened as a domain result: a `bool` for
  accepted-or-rejected, or a small result enum. UI reactions to that result
  (scrolling, focus moves, notifications, repaints) belong to the view layer
  that owns the settings and widgets involved; never bury them as hidden side
  effects inside the command path.
- When every call site already knows a condition's value, branch there
  instead of adding a boolean parameter for the helper to re-check.
- Do not extract a trivial expression (a bare `&&`, a single comparison) into
  a free function merely to unit-test it, and do not write tests that only
  exercise such a wrapper.
- Decode a result enum once with a single `match`; chained `==`/`!=`
  comparisons against the same value split the control flow.
- Append new entries (settings items, menu entries, config keys) at the end of
  the existing list unless the list has an established ordering rule or a
  specific position was requested.

## Frame scheduling

GPUI's frame pump is demand-driven. It wakes on a clean-to-dirty transition
(`cx.notify`, `window.refresh`) or when a frame ends with work still
outstanding. `window.on_next_frame` only pushes a callback onto a queue;
pushing one does not wake a parked pump.

Anything that starts an animation or defers work from *outside* a frame —
a mouse or key handler, a timer, a channel receiver — must therefore call
`cx.notify` too, not just queue a next-frame callback. A callback queued
from inside `prepaint`/`paint` is already covered by the end-of-frame
re-arm and needs nothing extra.

The failure mode is silent and easy to misread: the deferred work still
runs, but only once some unrelated repaint happens to arrive, so the input
looks dropped rather than late. It also hides during development, because
a blinking cursor or a running animation keeps the pump awake; it shows up
on a quiet window. GPUI's test harness does not model the pump, so a unit
test cannot catch this either.

## Commit message conventions

The repository uses hooks from `.githooks`. Do not bypass them with
`--no-verify`; fix the reported issue or split the commit along the required
boundary.

The pre-commit hook enforces these rules:

- Files under `.agents`, `.claude`, `.codex`, `.scratch`, `openspec`, `spec`,
  `docs/adr`, `docs/agents`, and `docs/superpowers` must not be committed with
  code files. Protected-path commits are rejected entirely on `main`.
- OpenSpec change documents under `openspec/changes` must not be committed
  until they are archived. Only paths under `openspec/changes/archive` are
  accepted; deletions from active change directories remain allowed so archive
  moves can remove the old paths. The hook reports
  `pre-commit: unfinished spec documents cannot be committed`.
- Changes under `third_party/gpui` must be committed separately from every
  other path. The same independent-commit rule applies to
  `third_party/gpui-kit`.
- Newly added content containing the repository's AI-slop marker is rejected.
- Added code comments are checked for implementation-instruction references;
  comments must explain the underlying technical rationale as described above.
- If staged files include Rust, the hook runs rustfmt on the staged Rust paths,
  `cargo clippy --all-targets --quiet` for their workspace members, and a
  first-party clippy pass with `-D clippy::absolute_paths`. This path requires
  `jq` to be available.

The commit-msg hook requires an English, printable-ASCII message and a
Conventional Commit subject in this form:

```text
<type>(optional-scope): <lowercase subject>
```

Allowed types are `feat`, `fix`, `refactor`, `docs`, `perf`, `test`, `chore`,
`build`, `ci`, `style`, `revert`, and `lint`. Examples:

- `feat(area): add new behavior`
- `fix(area): correct broken behavior`
- `refactor(area): restructure without behavior change`
- `docs(area): update documentation`

Use an unscoped typed subject for mechanical commits where a scope adds no
signal, for example `chore: apply cargo fmt`.

Follow the 60/80 rule: keep the subject at most 60 characters and wrap body
lines at 80 characters. The commit-msg hook enforces both limits; trailer
lines such as `Co-Authored-By` are exempt from the wrap.

The pre-push hook rejects pushing the local `dev` branch to `origin`; push that
branch to the `private` remote instead.

For non-trivial commits, include a body that explains the reason for the change
and the important implementation details. Keep the subject focused on the
user-visible or architectural effect.

Include a `Verification` section only when completed validation directly
exercised the changed behavior or measured its performance. Describe the
scenario, the behavior tested, and the observed result. Suitable evidence
includes manual use, an automated functional or regression test, or concrete
before-and-after performance measurements.

If no such validation was performed, omit the section. A successful build,
lint run, or test command alone does not describe which behavior was exercised.
Do not list build or lint commands in this section. The commit-msg hook also
rejects mentions of `cargo check`, `cargo test`, `cargo fmt`, `cargo clippy`,
`cargo build`, rustfmt, or compiling.

When an AI coding agent materially contributes to the change, end the
commit message with a `Co-Authored-By` trailer naming the model that
performed the work. Write the model ID the agent reports from its runtime,
followed by a noreply address on the model vendor's domain:

```text
Co-Authored-By: <MODEL_ID> <noreply@<vendor>.com>
```

Anthropic models use `noreply@anthropic.com` (e.g.
`claude-opus-5 <noreply@anthropic.com>`), Codex uses `noreply@openai.com`.

## Local agent instructions
@AGENTS.local.md
