## Why

Traditional mouse-wheel input currently moves the terminal block history and Agent transcript in immediate, discrete jumps. Smooth, velocity-continuous motion will make long output easier to scan while retaining direct platform behavior for precision touchpads and explicit navigation.

## What Changes

- Add opt-in smooth handling for line-based wheel input in GPUI lists, using a 400 ms velocity-aware cubic Bezier motion, accumulated destinations, and bounded repeated-input acceleration inspired by the older Edge scrolling feel.
- Add a default-enabled `Smooth Scrolling` switch under Settings > Window, persist it in the appearance configuration, and apply changes to open views without restarting the application.
- Enable the behavior for the local terminal block list and the Agent transcript list while the setting is on; restore immediate line scrolling when it is off.
- Keep pixel-based touchpad input immediate so platform-provided precision and inertia are not animated a second time.
- Keep scrollbar dragging, programmatic navigation, terminal mouse reporting, and the classic or remote terminal scroll path immediate.
- Add deterministic coverage for timing, repeated input, direction changes, bounds, tail following, and cancellation by direct navigation.

## Capabilities

### New Capabilities

- `smooth-wheel-scrolling`: Defines smooth traditional-wheel behavior, direct-input exclusions, destination updates, bounds, and tail-follow interaction for supported terminal and Agent lists.

### Modified Capabilities

## Impact

- Affects GPUI list wheel handling and animation state in `third_party/gpui`.
- Affects the application settings model, appearance configuration persistence, the Window settings group, and the local terminal and Agent list integration paths.
- Adds no external dependency and changes no terminal protocol, remote transport, or public command behavior. Existing configuration files that omit the new key retain the default-enabled behavior.
