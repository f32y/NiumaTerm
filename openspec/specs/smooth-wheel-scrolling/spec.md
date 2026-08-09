# Smooth Wheel Scrolling Specification

## Purpose

TBD - Define the long-term purpose of smooth wheel scrolling.

## Requirements

### Requirement: Window setting controls smooth scrolling
The system SHALL expose a dropdown labelled `Smooth Scrolling` under Settings > Window with the options `All`, `Only Terminal`, `Only Agent`, and `Off`. The setting SHALL default to `All` when no stored value exists, SHALL persist as `smooth-scrolling` in the `[appearance]` configuration section, and SHALL apply to open local terminal block-history and Agent transcript views without restarting the application.

#### Scenario: Use the default value
- **WHEN** the application loads a configuration that does not contain `[appearance].smooth-scrolling`
- **THEN** the `Smooth Scrolling` dropdown selects `All`
- **AND** local terminal block-history and Agent transcript lists use smooth line-wheel motion

#### Scenario: Enable only Terminal scrolling
- **WHEN** the user selects `Only Terminal` while Terminal and Agent lists are open
- **THEN** local terminal block-history lists use smooth line-wheel motion
- **AND** any active Agent transcript smooth motion is cancelled without changing the current displayed position
- **AND** later line-based wheel input in Agent transcript lists uses the existing immediate behavior
- **AND** the change takes effect without restarting the application

#### Scenario: Enable only Agent scrolling
- **WHEN** the user selects `Only Agent` while Terminal and Agent lists are open
- **THEN** Agent transcript lists use smooth line-wheel motion
- **AND** any active local terminal block-history smooth motion is cancelled without changing the current displayed position
- **AND** later line-based wheel input in local terminal block-history lists uses the existing immediate behavior
- **AND** the change takes effect without restarting the application

#### Scenario: Disable smooth scrolling
- **WHEN** the user selects `Off` while supported lists are open
- **THEN** any active smooth wheel motion is cancelled without changing the current displayed position
- **AND** later line-based wheel input in supported lists uses the existing immediate behavior

#### Scenario: Enable smooth scrolling everywhere
- **WHEN** the user selects `All` while supported lists are open
- **THEN** later line-based wheel input in local terminal block-history and Agent transcript lists uses smooth wheel motion

#### Scenario: Persist the selected value
- **WHEN** the user changes `Smooth Scrolling` and closes the settings dialog
- **THEN** the selected value is saved as `[appearance].smooth-scrolling` using `all`, `only-terminal`, `only-agent`, or `off`
- **AND** the same value is restored on the next application launch

#### Scenario: Load a legacy Boolean value
- **WHEN** the application loads `smooth-scrolling = true` or `smooth-scrolling = false`
- **THEN** `true` is interpreted as `All`
- **AND** `false` is interpreted as `Off`

### Requirement: Smooth line-wheel motion on supported lists
For each view enabled by the selected `Smooth Scrolling` mode, the system SHALL use smooth line-based wheel scrolling. Each motion SHALL use the Chromium wheel curve `(0.42, 0, 0.58, 1)`. Its base segment duration SHALL be computed as `clamp(14 - abs(distance) / 60, 6, 12) / 60` seconds, producing durations from 100 ms to 200 ms.

#### Scenario: Start a traditional wheel scroll
- **WHEN** a supported list receives non-zero line-based wheel input while no smooth wheel motion is active
- **THEN** the list starts moving toward the requested destination without an immediate full-distance jump
- **AND** a requested distance of up to 120 device-independent pixels reaches its destination after 200 ms unless another input updates it
- **AND** a requested distance of at least 480 device-independent pixels reaches its destination after 100 ms unless another input updates it

#### Scenario: Extend an active motion
- **WHEN** another line-based wheel input arrives while smooth wheel motion is active
- **THEN** the system samples the current displayed position and velocity
- **AND** extends the existing destination by the unscaled input distance
- **AND** adjusts the new curve's initial slope to preserve the sampled velocity
- **AND** shortens the base segment duration when the current same-direction velocity would otherwise overshoot the updated destination

### Requirement: Chromium wheel distance
For each view enabled by the selected `Smooth Scrolling` mode, the system SHALL convert each line-based wheel unit to `100 / 3` device-independent pixels and preserve the magnitude supplied by the platform without application-level acceleration.

#### Scenario: Apply the initial wheel distance
- **WHEN** a line-based wheel event supplies a magnitude of three line units
- **THEN** the requested destination moves by 100 device-independent pixels in the input direction, subject to the list bounds

#### Scenario: Accumulate repeated input
- **WHEN** repeated line-based wheel events arrive before the active motion completes
- **THEN** each event extends the existing destination by its own converted distance
- **AND** the application does not multiply the distance based on the number or timing of preceding events

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
