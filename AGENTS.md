# AGENTS.md

Repo-level guidance for AI coding agents working in this repository.

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

Recent history uses concise, imperative subjects. Prefer Conventional Commit-style
subjects when the change has a clear type and scope:

- `feat(area): add new behavior`
- `fix(area): correct broken behavior`
- `refactor(area): restructure without behavior change`
- `docs(area): update documentation`

Use a plain imperative subject for mechanical commits where a typed scope adds no
signal, for example `chore: apply cargo fmt`.

For non-trivial commits, include a body that explains the reason for the change,
the important implementation details, and the verification that was run. Bullet
lists are common. Keep the subject focused on the user-visible or architectural
effect, not just the files touched.

When an AI coding agent materially contributes to the change, include an
appropriate `Co-Authored-By` trailer at the end of the commit message. For Codex
authored or co-authored work, use:

```text
Co-Authored-By: OpenAI Codex <noreply@openai.com>
```
