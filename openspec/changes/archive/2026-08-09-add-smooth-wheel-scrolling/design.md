## Context

GPUI `ListState` currently coalesces wheel input, converts line units with a fixed 20-pixel value, and applies the resulting delta immediately. The local terminal block history and Agent transcript both use `ListState` with tail following, so one opt-in list behavior can serve both consumers. The classic or remote terminal path scrolls terminal rows separately, and terminal mouse-reporting modes can consume wheel input before the host list handles it.

On Windows, traditional wheel input reaches GPUI as `ScrollDelta::Lines` after the platform wheel-line preference has been applied. Precision touchpad input reaches GPUI as `ScrollDelta::Pixels` and already carries platform-managed motion and inertia.

Application settings are held in the global `AppSettings` model, copied to and from the typed configuration model, and saved when the settings dialog closes. The existing Settings > Window group already hosts application-wide window presentation controls. Global settings changes refresh open windows, which provides the update point for applying this behavior to lists that already exist.

The target feel follows Firefox's older cubic Bezier scroll model rather than its current default spring model. Firefox's older model updates an accumulated destination, samples the current velocity when input repeats, and derives a new curve from that velocity. The relevant source is [`ScrollAnimationBezierPhysics.cpp`](https://searchfox.org/firefox-main/source/layout/generic/ScrollAnimationBezierPhysics.cpp); current Firefox selects its spring model by default in [`ScrollContainerFrame.cpp`](https://searchfox.org/firefox-main/source/layout/generic/ScrollContainerFrame.cpp).

## Goals / Non-Goals

**Goals:**

- Give traditional mouse wheels a smooth, continuous 400 ms motion in the local terminal block list and Agent transcript.
- Expose one default-enabled `Smooth Scrolling` switch under Settings > Window and apply it to open supported lists without an application restart.
- Persist the selected value in the appearance configuration.
- Preserve velocity and displayed position when repeated input updates an active destination.
- Preserve platform behavior for precision touchpads and explicit list navigation.
- Reuse existing list bounds, scroll callbacks, item measurement, and tail-follow state.
- Keep animation timing deterministic under the GPUI test clock.

**Non-Goals:**

- Changing touchpad inertia or pixel-input handling.
- Changing terminal mouse reporting, alternate-screen behavior, or the classic or remote terminal row-scroll path.
- Adding smooth behavior to every GPUI list.
- Adding separate controls for duration, distance, acceleration, or curve weights.
- Animating Page Up, Page Down, keyboard navigation, or scrollbar dragging.

## Decisions

### 1. Add one opt-in behavior to `ListState`

`ListState` will expose an idempotent `set_smooth_wheel_enabled(bool)` method. The behavior remains disabled by default at the GPUI layer, and the local terminal block list and Agent transcript set it from the application setting during rendering. Disabling it cancels active motion at the current displayed position. The animation data lives in `StateInner`, beside the logical scroll position and tail-follow state it must coordinate with.

Implementing the behavior separately in the terminal and Agent views would duplicate timing, bounds, cancellation, and test logic. Enabling it globally would change unrelated lists without a product requirement.

### 2. Persist one default-enabled Window setting

`AppSettings` and `AppearanceConfig` will add a `smooth_scrolling` boolean. The typed configuration field will use the key `smooth-scrolling` in `[appearance]` and a default function that returns `true`, so existing files that omit the key enable the new behavior. The settings dialog will add a switch labelled `Smooth Scrolling` to the Window group with a description that names traditional mouse-wheel motion in Terminal View and Agent Pane.

The existing global settings notification refreshes open windows after the switch changes. During their next render, the terminal block list and Agent transcript pass the current value to `set_smooth_wheel_enabled`. This keeps GPUI independent from application configuration and makes disabling the switch cancel an active motion without reconstructing either view. Persisting through the existing settings-close path avoids a second save mechanism.

Storing the value under `[system]` was rejected because the setting controls visual interaction in two application views and the Window group already maps its controls through the appearance model. Storing independent terminal and Agent values was rejected because the requested control is one application-wide switch.

### 3. Animate line input and keep pixel input direct

`ScrollDelta::Lines` will use 50 pixels per line unit before repeated-input acceleration. The incoming line magnitude remains intact, so a Windows wheel setting of three lines requests 150 pixels for the initial event.

`ScrollDelta::Pixels` will cancel any active line-wheel motion and use the current direct path. This avoids adding synthetic motion on top of precision touchpad input that already contains platform-provided deltas and inertia.

### 4. Track a bounded 80 ms scroll series

The state will retain the preceding line-wheel time, direction, and one-based series number. A same-direction event arriving within 80 ms advances the number; a direction change or longer interval resets it to one. The base distance is multiplied by the series number. The destination extension from any one event is limited to the viewport height before it is clamped to the full scroll range.

The viewport limit preserves control during fast wheel bursts while retaining the strong acceleration associated with the older Edge-style settings. Adding another adjustable acceleration layer is unnecessary because the requested preset supplies fixed values.

### 5. Retarget with a velocity-aware cubic Bezier curve

Each new or updated line-wheel motion lasts 400 ms. When input arrives during an active motion, the implementation first samples the current position and velocity, uses them as the new start state, adds the new distance to the prior destination, and restarts the 400 ms duration.

For an axis with current position `p`, destination `d`, velocity `v`, and duration `T = 0.4 s`, the curve uses:

```text
slope = v * T / (d - p)
normal = sqrt(1 + slope * slope)
P1 = (0.25 / normal, slope * 0.25 / normal)
P2 = (0.6, 1.0)
```

When there is no usable destination distance or inherited velocity, the curve reduces to `cubic-bezier(0, 0, 0.6, 1)`. A small local cubic sampler is sufficient; no new dependency is needed. A fixed easing curve was rejected because it cannot retain velocity when repeated input updates the destination. The current Firefox spring model was rejected because the requested 400 ms settings belong to the older cubic model and do not determine spring duration.

### 6. Advance motion through the existing list scroll path

Starting or updating motion requests an animation frame. At the beginning of list prepaint, the state samples time through `cx.background_executor().now()`, computes the incremental movement since the preceding sample, and passes that movement through the existing internal list scroll operation before visible items are prepared. While motion remains active, the list requests another frame.

Using the executor clock allows unit tests to advance time without wall-clock waits. Reusing the internal scroll operation preserves scroll callbacks, logical item offsets, bounds, and notifications.

### 7. Cancel or rebase motion when another operation owns position

Pixel input, scrollbar dragging, reset, direct `scroll_by`, `scroll_to`, `scroll_to_end`, item reveal, and scrollbar-position updates will cancel active smooth wheel motion before applying their requested position. This gives explicit navigation immediate ownership instead of allowing a stale wheel destination to pull the list afterward.

Upward line input will stop tail following when the event is accepted, before the first animated frame. When item measurement rebases the logical position, the animation start and destination will receive the same displacement. At a scroll boundary, the destination is clamped and velocity pointing beyond the range is cleared.

## Risks / Trade-offs

- [A 400 ms tail can feel heavy for users accustomed to short motion] -> Limit it to traditional line input and validate both short and repeated wheel gestures in the terminal and Agent transcript.
- [Repeated input can grow quickly] -> Reset after 80 ms or a direction change and limit each event to one viewport.
- [Item height changes can cause a visible jump] -> Apply the same rebase displacement to the animation start and destination.
- [A direct navigation operation can race an active frame] -> Cancel motion before every public position-changing operation.
- [A settings change can leave an open list in the preceding mode until it renders] -> Refresh open windows through the existing global settings notification and use an idempotent list setter during rendering.
- [Continuous frames can add work while output is streaming] -> Request frames only while line-wheel motion is active and stop immediately at the destination or a boundary.

## Migration Plan

1. Add the opt-in `ListState` behavior and deterministic tests under `third_party/gpui` in its own implementation commit.
2. Add the default-enabled appearance configuration field, `AppSettings` mapping, Window switch, and live terminal and Agent list integration in a separate application-code commit.
3. Run focused GPUI tests and workspace checks, then exercise both pages with `target\debug\NiumaTerm.exe --testing` using a traditional wheel and a precision touchpad.

No stored-data migration is required because a missing key defaults to enabled. Rollback consists of disabling the two list integrations and removing the Window control; unrelated GPUI lists retain their default immediate behavior.

## Open Questions

None. Additional tuning controls can be considered later only if manual use shows that one fixed motion preset cannot serve both supported lists.
