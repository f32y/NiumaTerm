# Agent Context Compaction Specification

## Purpose
Define how Agent Tab surfaces provider-driven context compaction during live turns and history replay.

## Requirements

### Requirement: Codex automatic compaction has explicit live progress
Agent Tab SHALL translate a live Codex `contextCompaction` item lifecycle into the shared compaction progress state. The compaction SHALL remain part of the active turn and SHALL NOT create a user message or a second turn.

#### Scenario: Automatic compaction starts during a turn
- **WHEN** Codex emits `item/started` for a `contextCompaction` item while a normal turn is running
- **THEN** Agent Tab displays the compaction working state without inserting a completed transcript boundary or changing the turn identity

#### Scenario: Automatic compaction completes and generation continues
- **WHEN** Codex emits `item/completed` for the active `contextCompaction` item
- **THEN** Agent Tab clears the compaction working state, inserts one structural compaction boundary in the current turn, and continues processing later output from that turn

### Requirement: Live Codex compaction trigger is classified from client intent
Agent Tab SHALL mark a live Codex compaction as manual only when the same session has an outstanding NiumaTerm `thread/compact/start` request. Every other live Codex compaction SHALL be marked automatic.

#### Scenario: Provider initiates automatic compaction
- **WHEN** a `contextCompaction` lifecycle starts without an outstanding manual compact request
- **THEN** the completed boundary is labelled as automatic

#### Scenario: User initiates manual compaction
- **WHEN** Agent Tab sends `thread/compact/start` and the resulting `contextCompaction` lifecycle completes
- **THEN** the completed boundary is labelled as manual and the `/compact` command is reported complete from that lifecycle

#### Scenario: Manual compact RPC is acknowledged
- **WHEN** Codex returns the immediate successful response to `thread/compact/start` before its item lifecycle completes
- **THEN** Agent Tab treats the response as acceptance and SHALL NOT report the compaction complete yet

### Requirement: Codex compaction accounting is conservative
Agent Tab SHALL correlate `thread/tokenUsage/updated` snapshots around a live compaction. It SHALL include before and after token counts only when the observed post-compaction count is strictly lower than the captured pre-compaction count, and SHALL omit values it cannot establish.

#### Scenario: Usage reset brackets compaction
- **WHEN** the latest usage is 230000 tokens at compaction start and 17000 tokens at completion
- **THEN** the boundary reports 230000 before, 17000 after, and the existing UI derives 213000 tokens freed

#### Scenario: Post-compaction usage is missing or stale
- **WHEN** no lower usage snapshot arrives before the compaction item completes
- **THEN** Agent Tab preserves the boundary without inventing an after count or freed-token value

### Requirement: Codex compaction survives history replay
Agent Tab SHALL map a persisted Codex `contextCompaction` turn item to the shared structural boundary during thread replay. Metadata absent from the app-server item SHALL remain unknown. Agent Tab SHALL NOT treat a persisted `compacted.payload.message` or encrypted replacement-history item as a readable summary.

#### Scenario: Resume a thread containing compaction
- **WHEN** `thread/resume` returns a turn containing `{type: "contextCompaction", id: "compact-1"}`
- **THEN** the replayed transcript contains one compaction boundary with id `compact-1` and no fabricated trigger, accounting, message count, instructions, or summary

#### Scenario: Codex summary is unavailable
- **WHEN** Agent Tab renders a Codex compaction boundary whose app-server item has no readable summary
- **THEN** the boundary is not expandable and Agent Tab renders no missing-summary explanation or resume promise

#### Scenario: Claude summary has different persistence semantics
- **WHEN** the user expands a live Claude compaction boundary whose plaintext summary is written only to the session transcript
- **THEN** Agent Tab retains the disclosure so a resumed plaintext summary can be shown, but renders no placeholder prose while it is absent

### Requirement: Current app-server item avoids legacy duplicates
Agent Tab SHALL use the `contextCompaction` item lifecycle as the authoritative compaction signal and SHALL NOT add another boundary for the deprecated `thread/compacted` notification.

#### Scenario: Server publishes current and legacy signals
- **WHEN** one compaction produces a `contextCompaction` item and a legacy `thread/compacted` notification
- **THEN** Agent Tab displays exactly one compaction boundary derived from the item
