## MODIFIED Requirements

### Requirement: Replay the active Claude Code transcript chain
When replaying JSONL, the system SHALL reconstruct the current message chain by walking `parentUuid` backward from the last valid active leaf. It SHALL render only user text, assistant text, and reasoning from that chain. Messages abandoned on another branch MUST NOT appear in the transcript. The system SHALL associate each chain `tool_use` with a later `tool_result` through `tool_use_id` and preserve every tool type already supported by the live transcript, its input summary, result output, success or failure state, and file-change diff. Tool rows MAY reuse the live transcript's grouping and on-demand expansion, but grouping SHALL affect only presentation and SHALL NOT reduce persisted details to a count or discard them. Hook output, internal sidechain records, meta records, and queue-operation records SHALL be skipped. When multiple leaves exist, the system SHALL choose the last valid leaf in JSONL file order. If a parent is missing, it SHALL replay only messages that can be shown to be connected, record a non-fatal diagnostic, and MUST NOT join another branch to fill the gap.

#### Scenario: History contains tool calls
- **WHEN** the restored active message chain contains 20 tool calls and five user and assistant exchanges
- **THEN** the transcript shows all five exchanges and all 20 tool calls; consecutive calls may be grouped by default, and expanding the group reveals each call's true type, title, state, result, and available diff

#### Scenario: Associate tool results with calls
- **WHEN** a chain tool call has id `tool_123` and a later `tool_result.tool_use_id` is `tool_123`
- **THEN** the system updates that tool row with the result and completion state, allowing the restored row to reveal the result instead of creating a count placeholder or separate result row

#### Scenario: History contains an abandoned branch
- **WHEN** Claude JSONL retains an old branch and a new branch continued from an earlier message
- **THEN** the transcript shows only the `parentUuid` chain reachable from the last valid leaf and omits user, assistant, and tool content unique to the old branch

#### Scenario: The active chain has a missing parent
- **WHEN** a `parentUuid` referenced by the last valid leaf is absent from the transcript
- **THEN** the system replays only the connected suffix that can be established from the leaf, records a non-fatal diagnostic, and does not add records from another leaf
