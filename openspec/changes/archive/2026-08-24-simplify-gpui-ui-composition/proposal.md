## Why

GPUI views repeatedly spell out the same surface, row, overlay, status, close-action, and inline-rename structures. This duplication makes render functions long and allows related views to drift when theme or interaction details change.

## What Changes

- Introduce an application-level UI composition layer with typed style recipes built on GPUI styling and the existing theme semantics.
- Keep cross-view geometry in a small set of named metrics while leaving one-off measurements with the view that owns them.
- Add reusable UI components for repeated interaction shapes such as status marks, hover actions, inline rename controls, and blocking overlays.
- Migrate shared surfaces and Agent overlays first, then use the proven pieces to simplify tab-bar and workspace-sidebar rendering.
- Preserve view-owned state, event handling outcomes, accessibility descriptions, theme switching, drag behavior, keyboard behavior, and visible layout.
- Do not add a runtime CSS parser, selector cascade, new configuration format, or new library dependency.

## Capabilities

### New Capabilities

None. This change reorganizes internal UI composition without introducing new product behavior.

### Modified Capabilities

None. Existing visual and interaction requirements remain unchanged and serve as migration acceptance criteria.

## Impact

- Application UI modules under `crates/app/src/ui`, especially floating surfaces, right-side panels, settings presentation, the tab bar, and the workspace sidebar.
- Agent view rendering under `crates/app/src/agent/view`, especially repeated banners and blocking overlays.
- Existing application theme values and `gpui-component` styling helpers are reused; vendored GPUI and `gpui-component` behavior does not need to change.
- No persisted settings, user configuration, protocol messages, public APIs, or dependencies change.
