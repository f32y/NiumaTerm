## ADDED Requirements

### Requirement: Window setting controls smooth scrolling
The system SHALL expose a switch labelled `Smooth Scrolling` under Settings > Window. The setting SHALL default to enabled when no stored value exists, SHALL persist as `smooth-scrolling` in the `[appearance]` configuration section, and SHALL apply to open local terminal block-history and Agent transcript views without restarting the application.

#### Scenario: Use the default value
- **WHEN** the application loads a configuration that does not contain `[appearance].smooth-scrolling`
- **THEN** the `Smooth Scrolling` switch is on
- **AND** supported lists use smooth line-wheel motion

#### Scenario: Disable smooth scrolling
- **WHEN** the user turns off `Smooth Scrolling` while a supported list is open
- **THEN** any active smooth wheel motion is cancelled without changing the current displayed position
- **AND** later line-based wheel input in supported lists uses the existing immediate behavior
- **AND** the change takes effect without restarting the application

#### Scenario: Enable smooth scrolling
- **WHEN** the user turns on `Smooth Scrolling` while a supported list is open
- **THEN** later line-based wheel input in supported lists uses smooth wheel motion
- **AND** the change takes effect without restarting the application

#### Scenario: Persist the selected value
- **WHEN** the user changes `Smooth Scrolling` and closes the settings dialog
- **THEN** the selected value is saved as `[appearance].smooth-scrolling`
- **AND** the same value is restored on the next application launch

### Requirement: Smooth line-wheel motion on supported lists
While `Smooth Scrolling` is enabled, the system SHALL use smooth line-based wheel scrolling for the local terminal block history and the Agent transcript. Each newly started or updated motion SHALL use a 400 ms duration and a velocity-aware cubic Bezier curve.

#### Scenario: Start a traditional wheel scroll
- **WHEN** a supported list receives non-zero line-based wheel input while no smooth wheel motion is active
- **THEN** the list starts moving toward the requested destination without an immediate full-distance jump
- **AND** the list reaches the destination 400 ms after the motion starts unless another input updates it

#### Scenario: Extend an active motion
- **WHEN** another line-based wheel input arrives while smooth wheel motion is active
- **THEN** the system samples the current displayed position and velocity
- **AND** extends the existing destination from that position without a visible position or velocity discontinuity
- **AND** restarts the 400 ms motion toward the updated destination

### Requirement: Traditional wheel distance and acceleration
While `Smooth Scrolling` is enabled, the system SHALL convert each line-based wheel unit to at least 50 device-independent pixels, preserve the magnitude supplied by the platform, and accelerate repeated same-direction input within an 80 ms scroll series. The destination extension caused by one input event SHALL NOT exceed one list viewport.

#### Scenario: Apply the initial wheel distance
- **WHEN** the first event in a line-based wheel series supplies a magnitude of three line units
- **THEN** the requested destination moves by 150 device-independent pixels in the input direction, subject to the list bounds

#### Scenario: Accelerate repeated input
- **WHEN** same-direction line-based wheel events arrive no more than 80 ms apart
- **THEN** the first event uses the base distance, the second uses twice the base distance, and each later event uses its one-based series number as the distance multiplier
- **AND** each event remains limited to one viewport

#### Scenario: Reset the scroll series
- **WHEN** the input direction changes or more than 80 ms elapses after the preceding line-based wheel event
- **THEN** the next event starts a new series at the base distance

### Requirement: Direct input and navigation remain immediate
The system SHALL apply pixel-based input directly and SHALL cancel active smooth wheel motion before a scrollbar or programmatic navigation operation changes the list position. Terminal mouse reporting and classic or remote terminal scrolling SHALL retain their existing behavior.

#### Scenario: Use a precision touchpad
- **WHEN** a supported list receives pixel-based scroll input
- **THEN** the list applies the supplied pixel delta directly
- **AND** does not add a second animation or synthetic inertia

#### Scenario: Navigate directly during wheel motion
- **WHEN** scrollbar dragging, scrolling to the end, scrolling to an item, or another direct list-position operation occurs during smooth wheel motion
- **THEN** the smooth wheel motion is cancelled
- **AND** the requested operation takes effect immediately

#### Scenario: Report mouse input to a terminal application
- **WHEN** the terminal is in a mode that consumes wheel input for terminal mouse reporting
- **THEN** the wheel input is sent to the terminal application
- **AND** the host terminal list does not start smooth wheel motion

#### Scenario: Scroll a classic or remote terminal
- **WHEN** a terminal session uses the existing row-based scroll path instead of the local block list
- **THEN** the system retains the existing immediate row-based behavior

### Requirement: Smooth motion respects list state changes
The system SHALL clamp smooth wheel destinations to the current scroll range and SHALL preserve the displayed position when item measurement changes while motion is active.

#### Scenario: Reach a list boundary
- **WHEN** an active destination would move beyond the beginning or end of the list
- **THEN** the destination is clamped to the valid scroll range
- **AND** velocity directed beyond that boundary is cleared

#### Scenario: Remeasure content during motion
- **WHEN** item measurement rebases the current logical scroll position while smooth wheel motion is active
- **THEN** the motion start and destination are rebased by the same displacement
- **AND** the visible content does not jump because of the measurement update

### Requirement: Tail following responds to wheel intent
The system SHALL stop active tail following as soon as upward line-based wheel input is accepted and SHALL preserve the existing behavior that resumes tail following after the user returns to the end.

#### Scenario: Scroll away from a followed tail
- **WHEN** a tail-following terminal block list or Agent transcript accepts upward line-based wheel input
- **THEN** tail following stops before the first animation frame advances
- **AND** new output does not pull the list back to the end during that motion

#### Scenario: Return to the end
- **WHEN** the user scrolls a supported list back to its end after leaving the followed tail
- **THEN** the existing tail-follow behavior resumes
