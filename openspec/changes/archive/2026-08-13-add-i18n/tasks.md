# Tasks: add-i18n

## 1. i18n core crate

- [x] 1.1 Create `crates/i18n` (`nmt_i18n`): `include_str!` embed `locales/en.toml` and `locales/zh-CN.toml`, parse into `OnceLock` static maps, `AtomicU8` active-language index, `pub fn init(lang)`, `pub fn set_language(lang)`, `pub fn i18n(key: &str) -> &str` with key-as-fallback and debug-only `tracing::warn!` on miss
- [x] 1.2 Seed both locale files with an initial minimal key set and add a unit test asserting en/zh-CN key sets are identical
- [x] 1.3 Register the crate in the workspace `Cargo.toml` members and verify `cargo test -p nmt_i18n` passes

## 2. Config and settings plumbing

- [x] 2.1 Add `Language` enum (`En`, `ZhCn`) with `as_str()`/`from_value()` and a `language` field on `AppearanceConfig` in `crates/config/src/appearance.rs`, defaulting to English on missing/unknown values
- [x] 2.2 Persist the field in `patch_settings_document()` in `crates/config/src/lib.rs`
- [x] 2.3 Thread `language` through `AppSettings` in `crates/app/src/ui/settings/state.rs` (struct, `Default`, `load()`, `appearance_config()`)
- [x] 2.4 Add the Language dropdown (`("en", "English")`, `("zh-CN", "简体中文")`) to `crates/app/src/ui/settings/appearance_page.rs`

## 3. Startup initialization and live switching

- [x] 3.1 Call `nmt_i18n::init(...)` in `crates/app/src/main.rs` immediately after `load_startup_files_or_exit()`, and `gpui_component::set_locale(...)` right after `init_components(cx)`
- [x] 3.2 Extend the `AppSettings` global observer in `main.rs` to detect language changes, call `nmt_i18n::set_language` + `gpui_component::set_locale`, and refresh open windows
- [x] 3.3 Verify with `target\debug\NiumaTerm.exe --testing`: persisted zh-CN renders Chinese from first frame, live switch updates settings UI and component chrome without restart, unknown config value falls back to English

## 4. String replacement — settings UI

- [x] 4.1 Replace UI literals with `i18n(...)` keys in `ui/settings/` pages (`appearance_page`, `terminal_page`, `profiles_page`, `agent_page`, `system_page`, `remote_session_page`, `about_page`, `mod.rs`, label helpers in `state.rs`), including `.description(...)` and `.keywords(...)`, adding en+zh-CN catalog entries as they are introduced (en values verbatim from current literals)
- [x] 4.2 Replace UI literals in `ui/settings/theme.rs`, `agent_profile_dialog.rs`, `agent_profile_list.rs`, and shared field builders

## 5. String replacement — shell, dialogs, chrome

- [x] 5.1 Replace UI literals in `ui/shell/` (settings entry, close-confirmation dialogs in `close.rs`, `workspaces.rs` dialog, `updates_layer.rs`, `agent_notifications.rs`)
- [x] 5.2 Replace UI literals in `ui/tab_bar.rs` (context menu, tooltips), `ui/workspace_sidebar.rs`, `ui/git_sidebar.rs`, `ui/right_panel.rs`, `ui/font_picker.rs`, `ui/background_tasks/`, `ui/token_usage/`
- [x] 5.3 Replace UI literals in `main.rs` (window title, action-surface text), `workspace.rs` (default workspace/tab names), `tabs.rs`

## 6. String replacement — agent pane

- [x] 6.1 Replace UI literals in `agent_pane/` (`context_usage.rs`, `commands.rs`, `profile.rs`, `usage.rs`, `links.rs`, `view/`, `session/`, `updates/`, `transcript/`), converting `format!` templates to catalog templates with `{placeholder}` + `replace` per design D6
- [x] 6.2 Replace UI literals in `agent_pane/composer/` (including `rewind.rs` status messages)

## 7. Verification and cleanup

- [x] 7.1 Sweep `crates/` for remaining user-visible literals (Grep for `Label::new(`, `.label(`, `SettingItem::new(`, `PopupMenuItem`, `Tooltip::new`, `.placeholder(`, `.description(`, dialog builders) and confirm each hit is either keyed or legitimately excluded (IDs, logs, protocol, tests)
- [x] 7.2 Confirm the key-sync unit test still passes and `cargo clippy --all-targets` is clean
- [x] 7.3 Manual pass with `--testing` in both languages over the hotspot surfaces (settings pages, close dialogs, tab bar menus, sidebar, agent pane, notifications); check CJK glyph rendering and layout on dense surfaces per design risks
