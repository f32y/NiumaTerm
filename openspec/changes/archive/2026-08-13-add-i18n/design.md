# Design: add-i18n

## Context

See proposal.md — Why. Constraints that shape the approach:

- GPUI text APIs (`SettingItem::new`, `Label::new`, `PopupMenuItem::new`, tooltips, `.description(...)`) all take `impl Into<SharedString>`; `SharedString::from(&str)` heap-copies strings longer than 22 bytes on every render pass, so a `&'static str`-returning lookup slots in everywhere with `.into()` and no signature churn.
- The vendored `third_party/gpui-component/ui` already embeds rust-i18n 4.x with `en` and `zh-CN` catalogs for its own chrome and exposes `gpui_component::set_locale(&str)`; nothing calls it today, so the library is pinned to English.
- Startup order in `crates/app/src/main.rs`: `load_startup_files_or_exit()` (loads `config.toml` + `local_state.toml`) runs before `Application::run`; `init_components(cx)` and `cx.set_global(AppSettings::load())` run inside it. Language must be active before the first view is built.
- Settings persist via `nmt_config::save_settings(&SettingsPatch)` patch-writes into `config.toml` when the settings surface closes; live edits mutate the `AppSettings` global immediately.
- String surface: ~660 UI-visible literals in `crates/app` (hotspots: `ui/settings/*`, `agent_pane/*`, `ui/shell/*`, `ui/tab_bar.rs`, `ui/workspace_sidebar.rs`, `workspace.rs`), plus ~63 human-facing `format!` templates.

## Goals / Non-Goals

**Goals:**

- One lookup function `i18n(key: &str) -> &'static str` callable from any first-party crate with no per-call allocation.
- Live language switching from the settings dropdown, covering both app-owned text and gpui-component chrome.
- Catalog files that are trivially diffable and reviewable (flat key = value, one file per language).

**Non-Goals:**

- No plural rules, gender, or ICU MessageFormat — two languages, simple UI strings; `{name}`-style placeholder substitution is the ceiling.
- No runtime-loadable or user-supplied translation files; catalogs are compiled in. External files can come later without changing the API.
- No locale autodetection from the OS; the setting defaults to English until the user changes it.
- Diagnostic text stays English: log/tracing lines, protocol identifiers, agent backend error strings surfaced verbatim in banners, and OS-registered strings written at registration time (Explorer context-menu verb in `shell_extension`/`platform`).
- `third_party/` sources are untouched; the only interaction is calling the existing public `set_locale`.

## Decisions

### D1: Hand-rolled catalog store in a new `crates/i18n` crate; rust-i18n stays a gpui-component internal

A new minimal crate `nmt_i18n` (`crates/i18n`) owns the catalogs and the lookup. Both locale files are embedded with `include_str!`, parsed once at startup into two static maps held in `OnceLock`s, and the active language is an atomic index. `i18n(key)` reads the active map and falls back to the key.

Why our own store over reusing rust-i18n in the app: rust-i18n's `t!` returns `Cow<str>` (so every call site needs `.to_string()` or type juggling), requires a proc-macro and YAML layout, and its dotted-key convention conflicts with the requested flat `category-subcategory-name` keys and the plain `fn i18n(&str) -> &str` API. A flat `HashMap` lookup with a fallback is ~50 lines; a framework buys nothing here. gpui-component keeps its internal rust-i18n untouched — we only forward the locale name.

Why a new crate over a module in `nmt_config`: `crates/app` and `crates/platform` both render user-visible text, and `platform` does not depend on `nmt_config`. A leaf crate with no dependencies except the TOML parser keeps the dependency graph clean.

### D2: `&'static str` return type, made sound by loading all catalogs at init

Both catalogs (they are small — a few hundred entries each) are parsed into `OnceLock` statics at init and never dropped or mutated afterward. Language switching flips an `AtomicU8` index; it never replaces a map. Every `&str` handed out therefore genuinely lives for `'static`, so live switching and the borrowed return type coexist without unsafety or leaks. The missing-key fallback returns the caller's `&str` unchanged, so the signature is `fn i18n(key: &str) -> &str` (returned lifetime tied to the input, which is `'static` at every real call site since keys are literals).

Alternative considered — load only the selected locale: smaller resident memory, but a `&'static str` return then forbids runtime switching (the old catalog could never be freed safely anyway without leaking per switch). Loading both is simpler and enables live switching.

### D3: Catalog format — flat TOML, one file per language

`crates/i18n/locales/en.toml` and `zh-CN.toml`, flat `"key" = "value"` pairs, parsed with the workspace's existing `toml` dependency. TOML gives escaping and multiline strings for free and diffs cleanly. A key-sync unit test in `nmt_i18n` asserts both files contain identical key sets, enforcing the catalogs-stay-in-sync requirement at test time.

### D4: Config plumbing follows the existing enum-setting pattern

- `crates/config/src/appearance.rs`: `Language` enum (`En`, `ZhCn`) with `as_str()` (`"en"`/`"zh-CN"`) and `from_value()` defaulting to `En` on unknown input, mirroring `SmoothScrollingMode`. New `#[serde(default, rename = "language")]` field on `AppearanceConfig`.
- `crates/config/src/lib.rs`: one line in `patch_settings_document()`.
- `crates/app/src/ui/settings/state.rs`: `language` field on `AppSettings` (+ `Default`, `load()`, `appearance_config()`).
- `crates/app/src/ui/settings/appearance_page.rs`: `SettingField::dropdown` with options `("en", "English")` and `("zh-CN", "简体中文")`, in the existing Appearance page. Option display labels are proper names and stay as-is in both languages.

### D5: Initialization and switch propagation

- `main.rs`, immediately after `load_startup_files_or_exit()` returns: `nmt_i18n::init(config.appearance.language)` — parses both catalogs, sets the active index. This precedes `Application::run`, so the first frame is already localized.
- Inside `Application::run`, right after `init_components(cx)`: `gpui_component::set_locale(language.as_str())`.
- The existing `cx.observe_global::<AppSettings>` observer in `main.rs` compares the language field; on change it calls `nmt_i18n::set_language(...)` + `gpui_component::set_locale(...)` and refreshes open windows. GPUI re-renders views on notify, and every render re-executes the `i18n(...)` calls, so text updates without restart.

### D6: Interpolated strings keep templates in the catalog

The ~63 `format!` sites store their template under a key with named `{placeholder}` markers (e.g. `agent-usage-resets-in = "Resets in {duration}"`). Call sites do `i18n(key).replace("{duration}", &value)` directly; if a site has several placeholders, chained `replace` calls are still clearer than a substitution engine. No macro, no formatter abstraction.

### D7: Settings search keywords are translated

`SettingItem::keywords([...])` feeds the settings search index; untranslated keywords would make search useless in Chinese. Keyword lists pass through `i18n(...)` like labels do, and the Chinese catalog entries may append pinyin/English aliases in the value where that helps discovery.

## Risks / Trade-offs

- [CJK glyph rendering: default UI font is Segoe UI, which lacks CJK glyphs] → GPUI resolves missing glyphs through DirectWrite font fallback on Windows; verify visually with the zh-CN locale during implementation, and if fallback renders poorly, add a per-locale default UI font in `AppSettings::load()` as a follow-up.
- [~660 mechanical call-site edits risk typo'd keys that silently render as raw keys] → the key-as-fallback design makes mistakes visible on screen instead of crashing; a unit test asserts en/zh-CN key-set equality, and a debug-only `tracing::warn!` on missing keys surfaces typos in `app.log` during manual passes.
- [Wide diff across `crates/app` collides with concurrent feature branches] → the replacement is split into per-area commits (settings, shell/dialogs, agent pane, tab bar/sidebar, misc) so rebases stay tractable.
- [Translated string lengths differ (zh strings are shorter, en descriptions can wrap)] → GPUI layouts here are flex-based and already handle variable-length labels; spot-check dense surfaces (tab bar, status labels) in both languages.
- [Longer English text through `SharedString::from(&'static str)` still allocates on >22-byte strings each render] → identical to today's behavior with literals; no regression. Call sites that build `SharedString`s explicitly can use `SharedString::new_static(i18n(key))` where it matters.

## Open Questions

- Whether agent slash-command descriptions (`agent_pane/commands.rs`) should localize in v1 or track the upstream agent CLI's language — decidable during the replacement pass without affecting the spec or task breakdown.
