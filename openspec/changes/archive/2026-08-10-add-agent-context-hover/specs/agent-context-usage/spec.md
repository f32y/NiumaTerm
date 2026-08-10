## Purpose

Expose live agent context accounting in a compact, provider-aware surface so users can understand remaining capacity and where tokens are being consumed.

## ADDED Requirements

### Requirement: Context usage hover panel
The system SHALL open a compact Agent Context panel when the user hovers the composer context indicator. The panel SHALL show the current used-token total, the model context limit when available, and the remaining percentage when that limit is known.

#### Scenario: Context limit is available
- **WHEN** the user hovers an indicator whose latest snapshot includes a context limit
- **THEN** the panel shows the used and maximum token counts and the calculated remaining percentage

#### Scenario: Context limit is unavailable
- **WHEN** the user hovers an indicator whose latest snapshot does not include a context limit
- **THEN** the panel shows the used-token total without inventing a maximum or percentage

### Requirement: Provider-reported token categories
The system SHALL show every token category carried by the latest provider context snapshot and SHALL omit categories that the provider did not report. Cache and reasoning values SHALL be presented as detail within their associated input or output category so they are not interpreted as additional context usage.

#### Scenario: Codex reports a complete breakdown
- **WHEN** Codex reports input, cached input, cache-write input, output, and reasoning-output values
- **THEN** the panel shows all reported values and identifies cache and reasoning values as parts of input or output

#### Scenario: Claude does not report reasoning output separately
- **WHEN** Claude reports input, cache-read input, cache-creation input, and output without a separate reasoning value
- **THEN** the panel shows the reported categories and omits the reasoning row

#### Scenario: Compaction reports only a replacement total
- **WHEN** a provider supplies a post-compaction context total without a category breakdown
- **THEN** the panel updates the total and omits stale category values until a later provider snapshot replaces them

### Requirement: Scoped cumulative accounting
The system SHALL show cumulative token accounting when the provider supplies it and SHALL label its scope as either thread total or last-turn total. The system SHALL omit the cumulative section when no cumulative accounting is available.

#### Scenario: Codex supplies thread accounting
- **WHEN** the latest Codex context notification includes cumulative thread usage
- **THEN** the panel shows that breakdown under a Thread total heading

#### Scenario: Claude completes a turn
- **WHEN** a Claude result includes cumulative usage for the completed turn
- **THEN** the panel shows that breakdown under a Last turn heading

### Requirement: Live replacement behavior
Each provider context event SHALL replace the prior context snapshot as one coherent update so the compact label and hover panel cannot show values from different provider events.

#### Scenario: Provider publishes a newer snapshot
- **WHEN** a newer context snapshot arrives for the active agent tab
- **THEN** the compact label and every visible hover value are derived from that same snapshot

