# Pull request checks

The `PR hooks` workflow runs on pull requests using a Windows runner. Its
`Git hooks` job calls the repository's `commit-msg` and `pre-commit` hooks for
every commit reachable from the PR head but not from the base branch. The hooks
from the PR head apply to every checked commit.

Each commit is checked separately in a temporary clone, with its tree staged
against its first parent and the PR source branch name restored. This preserves
the hooks' rules for separating documentation, application code, and third-party
changes into different commits. Merge commits are also checked against their
first parent. Default merge messages may need to be rewritten to meet the
repository's commit message rules.

For Rust changes, the existing hooks run readability checks, formatting checks,
and Clippy on the affected workspace members. The workflow supplies Zig and the
pinned Rust toolchain, and caches Cargo downloads and build outputs. A failure
in any commit fails the job even if a later commit corrects it.

`pre-push` is not invoked because its rule concerns the local branch and push
remote, which do not represent a pull request check. The workflow uses
`pull_request` with read-only repository permissions and no persisted checkout
credentials.

To require a successful check before merging, add `Git hooks` as a required
status check in the target branch's GitHub ruleset or branch protection settings.

To run the same checks locally on Windows, check out the PR head and supply full
commit IDs and its source branch name:

```powershell
./scripts/check-pr-hooks.ps1 -Base <base-sha> -Head <head-sha> -Branch <source-branch>
```

Git for Windows, PowerShell 7, jq, and the regular Rust build dependencies must
be installed. The source checkout's branch, index, and working files remain
unchanged. Run `./scripts/check-pr-hooks.tests.ps1` to exercise commit selection,
hook failures, merge handling, and preservation of local edits in temporary
repositories.
