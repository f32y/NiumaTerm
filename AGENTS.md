# AGENTS.md

Repo-level guidance for AI coding agents working in this repository.

## Basic rules

- You are not the only agent that works in this repo. Do not touch files that you don't need to modify. Do not restore changes that are not made by you.
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

Split source files by responsibility, and keep the production code of a file
under roughly 800 lines. A cohesive state machine or hot loop may exceed the
guideline; splitting one merely to satisfy a line count hides its control
flow. Inline `#[cfg(test)]` test modules belong in their own child file
(`#[cfg(test)] mod tests;` resolving to `<module>/tests.rs`).

Multi-file modules use the directory form with `mod.rs` as the module root.
Never keep `foo.rs` next to a `foo/` directory; when splitting an existing
file, `git mv foo.rs foo/mod.rs` first so file history stays traceable.
`mod.rs` declares the child modules and re-exports moved public items so
existing import paths keep compiling.

Anchor every `use` line at the crate root: `use crate::...` or an external
crate name. `use super::...`, `use self::...`, and bare relative module
paths are forbidden in new or edited code; import lines in files a change
does not otherwise touch stay as they are. This rule governs import paths
only; visibility markers such as `pub(super)` and `pub(in ...)` remain the
correct tools. Widen visibility one step at a time (private, `pub(super)`,
`pub(crate)`, `pub`) and never further than a real caller requires.

## Technical taste

Each layer answers its own questions and returns honest results; callers
decide how to react.

- Command-style functions (PTY writes, clipboard operations, state mutations)
  return what actually happened as a domain result: a `bool` for
  accepted-or-rejected, or a small result enum. UI reactions to that result
  (scrolling, focus moves, notifications, repaints) belong to the view layer
  that owns the settings and widgets involved; never bury them as hidden side
  effects inside the command path.
- Do not add a boolean parameter that a helper re-checks when every call site
  already knows the answer; branch at the call site instead. A literal `true`
  or `false` argument in a call is the tell.
- Do not extract a trivial expression (a bare `&&`, a single comparison) into
  a free function merely to unit-test it, and do not write tests that only
  exercise such a wrapper.
- Decode a result enum once with a single `match`; chained `==`/`!=`
  comparisons against the same value split the control flow.
- Append new entries (settings items, menu entries, config keys) at the end of
  the existing list unless the list has an established ordering rule or a
  specific position was requested.

## Commit message conventions

The repository uses hooks from `.githooks`. Do not bypass them with
`--no-verify`; fix the reported issue or split the commit along the required
boundary.

The pre-commit hook enforces these commit boundaries:

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
  `third_party/gpui-component`.
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
and the important implementation details. Bullet lists are common. Keep the
subject focused on the user-visible or architectural effect, not just the files
touched.

Do not add a `Verification` section merely to list routine development checks
such as `cargo check`, `cargo fmt`, `cargo clippy`, compilation, or analogous
formatting and lint commands. These checks are mandatory parts of modifying the
codebase; by themselves they do not verify that the changed behavior works.

Include a `Verification` section only when the change itself was meaningfully
validated, such as by manually exercising the affected behavior, running an
automated functional or regression test that directly covers it, or collecting
concrete before-and-after performance data. If no such validation was performed,
omit the section instead of substituting routine tool output as boilerplate.

When an AI coding agent materially contributes to the change, end the
commit message with a `Co-Authored-By` trailer naming the model that
performed the work. Write the model ID the agent reports from its runtime,
followed by a noreply address on the model vendor's domain:

```text
Co-Authored-By: <MODEL_ID> <noreply@<vendor>.com>
```

Anthropic models use `noreply@anthropic.com` (e.g.
`claude-opus-5 <noreply@anthropic.com>`), Codex uses `noreply@openai.com`.
