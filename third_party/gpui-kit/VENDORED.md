# GPUI Kit snapshot

Source: `C:/Workspace/gpui-kit`, branch `nmt_gpuikit`, revision `37dfd3ae`.

The imported crates are `base`, `component`, `component-macros`, and `assets`,
using the source repository's `crates/` layout. These include NiumaTerm's
component customizations ported to the newer Base/Component split.
Showcase binaries and examples are not included; library tests, their Markdown
and theme samples, and the motion benchmark are included. The parent NiumaTerm
workspace supplies dependencies.

GPUI continues to come from `third_party/gpui`. This update does not replace
or modify that backend, and it does not depend on a sibling Zed checkout.
`gpui-kit-assets` replaces the previous assets package name. The `sum-tree`
dependency name points to the existing local `sum_tree` package.

Application integration uses `TextareaState` and `Textarea` for multi-line
editors, `TextSelection` for window selection, and the current component
accessibility, settings options, and link callback APIs. Dependency versions
retain the higher versions already present in either workspace.

Secret question answers use a single-line masked `InputState`; ordinary
answers remain auto-growing textareas. This keeps password masking in the
layout supported by the new input types, including disabled clipboard access.

## Validation on Windows

- All targets passed checking for the application, Agent pane, syntax bundle,
  and the four imported crates.
- 223 application tests and 197 Agent pane tests passed, including the new
  question-input regression covering multi-line text and masked secrets.
- 1,274 Base/Component unit and integration tests passed with tree-sitter.
- All 533 imported crate files matched the source snapshot. The comparison
  of 83 direct dependencies found no lower versions.

macOS, Linux, WebAssembly, and an interactive application launch were not
validated during this update.
