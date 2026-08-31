## 1. Codex Title Worker

- [x] 1.1 Add title prompt normalization, structured request builders, and generated-title parsing in a dedicated Codex app-server module; verify unit tests cover valid, oversized, empty, and malformed output.
- [x] 1.2 Run title generation through a temporary Host registration with an ephemeral read-only thread, bounded wait, unsubscribe, and cancellation; verify adapter tests show auxiliary notifications never produce primary conversation events.

## 2. Session and Tab Integration

- [x] 2.1 Replace first-line Codex naming with an immediate local provisional title followed by the worker result; verify AgentPane tests cover prompt acceptance, slash-command exclusion, and unchanged Claude behavior.
- [x] 2.2 Apply generated and fallback titles only to the matching active generation, persist them through `thread/name/set`, and cancel generation on user rename or conversation replacement; verify tests cover success, failure, stale result, and rename races.
- [x] 2.3 Restore a non-empty Codex `thread.name` as an already-named Tab; verify resume tests show the stored name and prevent a follow-up from starting first-prompt naming.

## 3. Integration Verification

- [x] 3.1 Run the focused Agent Tab and Codex app-server test suites and verify the provisional-to-generated transition, isolated title activity, fallback behavior, rename priority, and restored-title behavior all pass.
- [x] 3.2 Run repository formatting and lint checks for the changed Rust workspace members and inspect the final diff for unrelated edits.
