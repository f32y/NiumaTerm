# AGENTS.md

Repo-level guidance for AI coding agents working in this repository.

## Basic rules

- YOU ARE FORBIDDEN TO USE FOLLOWING AI SLOP WORDS: ponytail, seam, fact, parity, envelope, wire
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

## Commit message conventions

The repository uses hooks from `.githooks`. Do not bypass them with
`--no-verify`; fix the reported issue or split the commit along the required
boundary.

The pre-commit hook enforces these commit boundaries:

- Files under `.agents`, `.claude`, `.codex`, `.scratch`, `openspec`, `spec`,
  `docs/adr`, `docs/agents`, and `docs/superpowers` must not be committed with
  code files. Protected-path commits are rejected entirely on `main`.
- Changes under `third_party/gpui` must be committed separately from every
  other path. The same independent-commit rule applies to
  `third_party/gpui-component`.
- Newly added content containing the repository's AI-slop marker is rejected.
- Added code comments are checked for implementation-instruction references;
  comments must explain the underlying technical rationale as described above.
- If staged files include Rust, the hook runs `cargo fmt --all --check`,
  `cargo clippy --workspace --all-targets --quiet`, and a first-party clippy
  pass with `-D clippy::absolute_paths`. This path requires `jq` to be
  available.

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

When an AI coding agent materially contributes to the change, include an
appropriate `Co-Authored-By` trailer at the end of the commit message.

For Codex authored or co-authored work, use:

```text
Co-Authored-By: <YOUR_MODEL_NAME> <noreply@openai.com>
```
