## 1. Live Claude Titles

- [x] 1.1 Add Claude's six-word, 60-character provisional-title projection and publish it after the first accepted prompt; verify focused session tests cover normalization, truncation, and accepted-send behavior.
- [x] 1.2 Mark resumed Claude conversations as already named and verify a focused session test shows a follow-up does not enter the first-prompt title path.

## 2. Provider and History Persistence

- [x] 2.1 Bound Claude title descriptions to 2,000 characters in the stream-json adapter and verify focused adapter tests cover trimming and Unicode-safe truncation.
- [x] 2.2 Resolve Claude history titles in `custom-title`, `ai-title`, provisional, and ID order; verify focused session-history tests cover each precedence case.

## 3. Integration Verification

- [x] 3.1 Run formatting and focused tests for the affected workspace members, then verify the full affected member test suites report no regressions.
- [x] 3.2 Validate the OpenSpec change strictly and confirm every implementation task is marked complete.
