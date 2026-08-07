## 1. Codex adapter lifecycle

- [x] 1.1 Add session state for the latest context usage, pending manual intent, and active compaction correlation, including safe reset paths.
- [x] 1.2 Translate live `contextCompaction` started/completed notifications into shared progress and boundary events with conservative token accounting.
- [x] 1.3 Align manual `/compact` acknowledgement and completion feedback with the asynchronous item lifecycle.
- [x] 1.4 Map replayed `contextCompaction` items to structural compaction boundaries while continuing to ignore the legacy notification.
- [x] 1.5 Keep replayed Codex boundaries metadata-free when app-server omits a readable summary; do not parse private or encrypted rollout state.
- [x] 1.6 Make Codex compaction boundaries non-expandable and remove missing-summary placeholder prose while retaining Claude disclosure behavior.

## 2. Regression coverage

- [x] 2.1 Add focused tests for automatic progress, same-turn completion, and before/after token correlation.
- [x] 2.2 Add focused tests for manual trigger classification, acknowledgement semantics, and state cleanup after failed or incomplete turns.
- [x] 2.3 Add replay and legacy-signal tests that prevent generic tool rendering or duplicate boundaries.
- [x] 2.4 Add regression coverage for metadata-free Codex replay and provider-specific disclosure availability.

## 3. Validation

- [x] 3.1 Run Rust formatting checks and the `nmt_agent_utils` test suite.
- [x] 3.2 Run focused clippy validation and validate the OpenSpec change.
- [x] 3.3 Re-run focused tests, formatting, clippy, app compilation, and strict OpenSpec validation after simplifying Codex boundaries.
