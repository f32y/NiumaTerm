## 1. GPUI Smooth Wheel Core

- [x] 1.1 Add an idempotent `set_smooth_wheel_enabled(bool)` API and smooth line-wheel state to `ListState` with the 400 ms duration, 50-pixel line distance, 80 ms series window, current velocity, and accumulated destination; disabling the API must cancel active motion at the displayed position.
- [x] 1.2 Implement the velocity-aware cubic Bezier sampler and destination update logic using the GPUI executor clock.
- [x] 1.3 Route line-based wheel input through the animated path, retain direct pixel input, and limit each repeated-input destination extension to one viewport and the current scroll range.
- [x] 1.4 Advance active motion before list item prepaint through the existing internal scroll operation and request frames only until the motion completes or reaches a boundary.
- [x] 1.5 Cancel motion for direct position-changing operations, stop tail following when upward wheel intent is accepted, and rebase active motion with item measurement changes.

## 2. GPUI Regression Coverage

- [x] 2.1 Add deterministic tests for the 50-pixel line conversion, partial progress, exact 400 ms completion, and final destination.
- [x] 2.2 Add deterministic tests for repeated-input acceleration, the 80 ms reset, direction changes, viewport limiting, destination accumulation, and velocity-continuous updates.
- [x] 2.3 Add deterministic tests for range boundaries, measurement rebasing, disabling during motion, pixel-input cancellation, scrollbar and programmatic cancellation, and tail-follow transitions.
- [x] 2.4 Confirm lists that do not opt in retain their existing immediate line and pixel behavior.

## 3. Settings and Persistence

- [x] 3.1 Add the default-enabled `smooth_scrolling` field to `AppearanceConfig`, load and save it as `[appearance].smooth-scrolling`, and cover missing, enabled, disabled, and patched values in configuration tests.
- [x] 3.2 Add the default-enabled `smooth_scrolling` field to `AppSettings`, map it through configuration load and save, and extend settings round-trip coverage.
- [x] 3.3 Add a `Smooth Scrolling` switch to Settings > Window with a description covering traditional mouse-wheel motion in Terminal View and Agent Pane.

## 4. Application Integration

- [x] 4.1 Apply the current setting to the local terminal block list during rendering without changing terminal mouse reporting or the classic or remote terminal paths.
- [x] 4.2 Apply the current setting to the Agent transcript list during rendering and preserve transcript streaming and tail-follow behavior.
- [x] 4.3 Add application-level coverage that changing the setting updates open supported lists, cancels active motion when disabled, and does not require view reconstruction.

## 5. Verification

- [x] 5.1 Run the focused GPUI list, configuration, and application settings tests plus the required Rust formatting, workspace lint, and build checks.
- [x] 5.2 Launch `target\debug\NiumaTerm.exe --testing` and verify the default-enabled Window switch, live enable and disable behavior, traditional-wheel motion, repeated input, direction reversal, and boundaries in both supported pages.
- [x] 5.3 In the testing instance, verify that precision touchpad scrolling remains direct, scrollbar and programmatic navigation cancel motion, streaming output does not restore a departed tail, and terminal mouse reporting remains unchanged.
- [x] 5.4 Close the settings dialog, restart with `--testing`, and verify that enabled and disabled `Smooth Scrolling` values are restored from `[appearance].smooth-scrolling`.
