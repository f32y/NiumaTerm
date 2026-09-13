# Settings ownership and mutation

Status: accepted, 2026-09-13.

Live previews need shared application settings, but temporary theme filtering
and pairing progress must not trigger global settings observers or survive a
closed settings page. Copying each configuration section into another public
struct also allowed widgets to bypass validation and profile invariants.

`AppSettings` owns a private `Config` and exposes an immutable read view.
Named section edits and profile operations own mutations. Appearance values are
normalized at configuration load and after live edits using the same rules.
Profile operations reject missing indices, preserve default references, and
keep agent names unique. Rejected operations return a result to their caller.

Each open `SettingsSurface` owns its page state, temporary editor, and theme
watcher. Temporary edits notify that surface. Pairing completion holds a weak
reference, so a closed editor is released and late updates are rejected.

Saving performs a patch to the configuration file and returns its error without
mutating global settings. The view owns retry notifications. Settings close and
application quit own save timing; focus and workspace reorder do not save.
The final window's explicit discard also suppresses the final quit save.

Regression tests cover live normalization, default selection, stale indices,
reordering, failed-write retry, and independent rendered settings windows.

Implementations: `crates/app/src/ui/settings/state.rs`,
`crates/app/src/ui/shell/settings_workspace.rs`, and
`crates/config/src/appearance.rs`.
