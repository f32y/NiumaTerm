## ADDED Requirements

### Requirement: Direct render-buffer capture
The terminal engine SHALL capture the visible viewport directly into a caller-provided `RenderBuffer` without constructing an owned intermediate frame containing `TerminalSnapshot` or `SnapshotCell` values.

#### Scenario: Capture a visible viewport
- **WHEN** the Ghostty render state updates successfully and a caller requests capture into a render buffer
- **THEN** the target render buffer contains the current dimensions, cells, styles, grapheme extras, row metadata, cursor, colors, scrollbar, and Kitty placement metadata

#### Scenario: Reuse after resize
- **WHEN** a caller captures into an existing render buffer after the terminal dimensions change
- **THEN** all dimension-dependent storage matches the new viewport and no cells or row metadata from the previous dimensions remain visible

### Requirement: Complete-frame publication
The terminal SHALL construct each new frame outside the shared front-buffer lock and SHALL atomically replace the published render buffer only after capture succeeds.

#### Scenario: Successful PTY frame
- **WHEN** direct capture of a PTY or resize update completes successfully
- **THEN** the completed back buffer is swapped into the shared front buffer under a short lock and `TerminalDamaged` is emitted after the swap

#### Scenario: Failed capture
- **WHEN** direct capture returns an error
- **THEN** the previously published render buffer remains unchanged and no damage event is emitted for the incomplete frame

#### Scenario: Concurrent frame read
- **WHEN** the UI reads the shared render buffer while the terminal engine is preparing another frame
- **THEN** the UI observes one complete published frame and does not wait for Ghostty viewport traversal while holding the front-buffer lock

### Requirement: Rendering and selection equivalence
Directly captured render buffers SHALL preserve the rendering and selection semantics of the previous snapshot conversion pipeline.

#### Scenario: Styled Unicode content
- **WHEN** a viewport contains ASCII text, wide characters, combining marks, grapheme clusters, styled cells, soft-wrapped rows, and a cursor
- **THEN** frame extraction produces the same cell text, widths, styles, row continuity, colors, and cursor presentation as before the change

#### Scenario: Word selection in a scrolled viewport
- **WHEN** a user double-clicks a word such as `pipelines` in `pipelines.universal` while the viewport is scrolled
- **THEN** selection and copy operate on the complete word using the published render buffer and the existing viewport-to-screen coordinate conversion

#### Scenario: Synchronous scroll refresh
- **WHEN** the application scrolls the Ghostty viewport and refreshes the render buffer synchronously
- **THEN** it publishes a complete refreshed frame while preserving the existing cursor-visibility behavior

### Requirement: Single owned frame model
The terminal crate SHALL expose `RenderBuffer` as the owned renderable snapshot type and SHALL remove the legacy owned snapshot conversion interface.

#### Scenario: Workspace compilation after migration
- **WHEN** all terminal, application, trace, and test consumers are compiled
- **THEN** none depend on `TerminalSnapshot`, `SnapshotCell`, or `RenderBuffer::update(&TerminalSnapshot)`

### Requirement: Dense frame-extraction performance
The change SHALL retain dense-grid frame extraction and SHALL not introduce a greater than 10 percent slowdown in the five-run median of the existing full-frame extraction profile.

#### Scenario: Compare extraction baseline
- **WHEN** the same full-frame profile is run five times before and after the change under equivalent conditions
- **THEN** the post-change median frame-extraction time is no more than 110 percent of the baseline median
