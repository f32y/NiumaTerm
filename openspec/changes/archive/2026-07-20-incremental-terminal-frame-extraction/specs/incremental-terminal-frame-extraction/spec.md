## ADDED Requirements

### Requirement: Persistent row damage versions
The terminal engine SHALL maintain a monotonic content version for every visible row and SHALL copy the complete version set into each captured `RenderBuffer`. It SHALL transfer Ghostty's global and per-row render damage into those versions before clearing both damage sources.

#### Scenario: Initial or global damage
- **WHEN** the first viewport is captured or Ghostty reports full render damage
- **THEN** every visible row receives the current content version

#### Scenario: Partial row damage
- **WHEN** Ghostty reports partial damage for a subset of visible rows
- **THEN** only those rows receive the new content version and all other row versions remain unchanged

#### Scenario: No row damage
- **WHEN** Ghostty reports no render damage
- **THEN** all row versions remain unchanged

#### Scenario: Damage is consumed
- **WHEN** Ghostty damage has been transferred into persistent row versions
- **THEN** both the global render-state damage and all per-row dirty flags are cleared before the next capture

#### Scenario: UI skips a publication
- **WHEN** different rows change in two captured buffers and the UI compares only the frame before both changes with the latest buffer
- **THEN** the latest row versions identify every row changed across the skipped publication

### Requirement: Incremental line extraction
The frame extractor SHALL reuse the immutable line data from the previous frame only when viewport dimensions match, reuse is permitted, and the row's content version, selection interval, and cursor rendering input are unchanged. It SHALL rebuild a row when any of those inputs differ.

#### Scenario: One content row changes
- **WHEN** one row version changes and all other line-rendering inputs remain equal
- **THEN** the changed row is rebuilt and every unaffected row reuses its previous immutable line data

#### Scenario: Cursor-only movement
- **WHEN** the cursor changes without a Ghostty row version change
- **THEN** each row whose old or new cursor rendering differs is rebuilt

#### Scenario: Selection changes
- **WHEN** the visible selection interval changes on one or more rows without terminal content changing
- **THEN** exactly those rows with different selection intervals are rebuilt so both new and removed highlighting are represented

#### Scenario: Viewport dimensions change
- **WHEN** the current and previous viewport dimensions differ
- **THEN** the extractor rebuilds all visible rows

### Requirement: Explicit full visual invalidation
The frame cache SHALL distinguish ordinary invalidation, which permits eligible line reuse, from full visual invalidation, which forbids line reuse for the next rebuild while retaining the displayed frame for coordinate mapping.

#### Scenario: Terminal damage invalidation
- **WHEN** a terminal damage notification marks the frame cache stale
- **THEN** the next rebuild may reuse lines that satisfy the incremental extraction conditions

#### Scenario: Theme or visual configuration changes
- **WHEN** application settings change a color or other line-rendering input not represented by terminal row versions
- **THEN** the cache requests a full visual invalidation and the next rebuild reconstructs every line

#### Scenario: Full invalidation is consumed
- **WHEN** a rebuild completes after full visual invalidation
- **THEN** later ordinary invalidations may reuse eligible lines again

### Requirement: Complete frame behavior
Incremental extraction SHALL preserve the terminal frame's existing text, grapheme, wide-cell, style, background, cursor, selection, scrollbar, and Kitty image behavior. Cursor, scrollbar, image, and other non-line frame metadata SHALL be derived from the current buffer on every rebuild even when all lines are reused.

#### Scenario: Only non-line metadata changes
- **WHEN** line-rendering inputs are unchanged but current cursor-independent frame metadata changes
- **THEN** eligible lines are reused and the resulting frame contains the current metadata

#### Scenario: Complex row is rebuilt
- **WHEN** a dirty row contains styled text, grapheme extras, or wide cells
- **THEN** its rebuilt line has the same observable content and rendering data as full extraction

### Requirement: Performance validation
The profiling harness SHALL measure both forced full-frame extraction and a representative one-row incremental update. Under the same release-build harness, the full-frame median SHALL not regress by more than 10 percent from the pre-change baseline, and the one-row incremental median SHALL be lower than the forced full-frame median.

#### Scenario: Compare extraction profiles
- **WHEN** each profile is run five times against the same terminal dimensions and content
- **THEN** the reported medians satisfy the full-frame regression limit and demonstrate reduced extraction time for a one-row update
