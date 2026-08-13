# Add i18n

## Why

Every user-visible string in NiumaTerm is hardcoded English, so the app cannot serve Simplified-Chinese users. The vendored gpui-component library already localizes its own chrome (dialogs, search placeholder) via rust-i18n but is stuck on `"en"` because nothing ever calls `set_locale`; app-owned text has no localization path at all.

## What Changes

- Add a translation system: flat key→value string catalogs, one per language, with keys in the form `category-subcategory-name`. Initial languages: English (`en`) and Simplified Chinese (`zh-CN`).
- Provide a lookup API `fn i18n(key: &str) -> &str` usable from any crate. A missing key returns the key itself as the display text.
- Initialize the language system in `app/main` immediately after startup config loading, before any UI is built, and propagate the chosen locale to `gpui_component::set_locale` so the component library's own chrome follows the app language.
- Add an **Appearance → Language** dropdown to the settings UI offering English and 简体中文, persisted in `config.toml`.
- Replace user-visible UI string literals across `crates/` (settings pages, menus, dialogs, tooltips, notifications, status text, tab bar, sidebar, agent pane) with `i18n(...)` lookups. Log messages, protocol identifiers, serde/TOML keys, element IDs, and test fixtures stay untouched.
- English catalog entries reuse the current literals verbatim, so the default-language UI is pixel-identical before and after.

## Capabilities

### New Capabilities

- `ui-localization`: runtime string translation — catalog format and key convention, the `i18n` lookup with key-as-fallback behavior, startup initialization order, the Language setting and its persistence, and live locale switching.

### Modified Capabilities

_None. Existing capability specs describe behavior independent of display language._

## Impact

- **New crate** for the catalog store and `i18n()` lookup, embedding both catalogs at compile time so lookups can return `&'static str`.
- `crates/config`: new `appearance.language` key in `AppearanceConfig` and `patch_settings_document`.
- `crates/app`: initialization in `main.rs`; new dropdown in `ui/settings/appearance_page.rs`; `AppSettings` field in `ui/settings/state.rs`; wide mechanical replacement of ~660 UI literals across `ui/`, `agent_pane/`, `workspace.rs`, and shell dialogs.
- `crates/platform` / `crates/shell_extension`: small number of user-visible strings (notifications, Explorer context-menu verb) evaluated case by case — OS-registered strings written once at install time may stay English in v1.
- **No new external dependency for the app's own lookups**; `rust-i18n` remains a gpui-component implementation detail. `third_party/` code is untouched apart from calling its existing public `set_locale` API.
- Interpolated messages (`format!` templates, ~63 sites) keep their template text in the catalog with placeholders and are filled at the call site; the plain `i18n` lookup covers everything else.
