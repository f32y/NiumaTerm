## Purpose

Lets the application display all user-visible interface text in the user's chosen language, initially English and Simplified Chinese, through a key-based translation lookup with a persisted language setting.

## ADDED Requirements

### Requirement: Key-based translation lookup

The system SHALL provide a translation lookup that maps a string key to the translated text for the active language. Keys SHALL follow the flat form `category-subcategory-name` (lowercase, hyphen-separated). When the active language has no entry for a key, the lookup SHALL return the key itself as the display text.

#### Scenario: Existing key resolves to active-language text

- **WHEN** the active language is Simplified Chinese and a UI element requests the key `settings-appearance-theme`
- **THEN** the lookup returns the Simplified Chinese translation recorded for that key

#### Scenario: Missing key falls back to the key text

- **WHEN** a UI element requests a key that is absent from the active language catalog
- **THEN** the lookup returns the key string itself and the application continues without error

### Requirement: Language catalogs for English and Simplified Chinese

The system SHALL ship complete string catalogs for English (`en`) and Simplified Chinese (`zh-CN`). Every translated UI string SHALL have an entry in both catalogs. The English catalog SHALL reproduce the pre-i18n literal text exactly, so the English UI is unchanged by the migration.

#### Scenario: English UI unchanged after migration

- **WHEN** the language is English and any previously hardcoded UI surface (settings pages, menus, dialogs, tooltips, notifications) is rendered
- **THEN** the displayed text is identical to the text shown before the i18n migration

#### Scenario: Catalogs stay in sync

- **WHEN** the two shipped catalogs are compared key by key
- **THEN** every key present in one catalog is present in the other

### Requirement: Language selection setting

The settings UI SHALL offer a Language dropdown in the Appearance section with the options English and 简体中文. The selection SHALL persist in the user configuration file and survive restarts. The default language SHALL be English.

#### Scenario: User switches to Simplified Chinese

- **WHEN** the user selects 简体中文 in Appearance → Language
- **THEN** user-visible interface text renders in Simplified Chinese without requiring an application restart, and the choice is written to the configuration file when settings are saved

#### Scenario: Persisted choice restored on launch

- **WHEN** the configuration file records Simplified Chinese and the application starts
- **THEN** all UI text renders in Simplified Chinese from the first frame

#### Scenario: Unrecognized configured value

- **WHEN** the configuration file contains a language value that is neither `en` nor `zh-CN`
- **THEN** the application falls back to English and continues normally

### Requirement: Startup initialization before first render

The language system SHALL be initialized immediately after the startup configuration is loaded and before any UI is constructed, so no frame ever renders untranslated placeholder text. The embedded component library's locale SHALL be set to the same language, so its built-in chrome (dialog buttons, search placeholders) matches the application language at startup and after every language change.

#### Scenario: Component chrome follows the application language

- **WHEN** the active language is Simplified Chinese and a component-library dialog with built-in button labels is shown
- **THEN** those built-in labels render in Simplified Chinese

### Requirement: Translation coverage of user-visible strings

All user-visible interface strings in first-party crates — settings page/group/item titles and descriptions, dropdown option labels, menu and context-menu entries, dialog titles/bodies/buttons, tooltips, placeholders, notification text, status labels, and default names for user-created objects — SHALL be served through the translation lookup. Log/tracing output, protocol and serde identifiers, configuration keys, element IDs, keyboard shortcut names, and test fixtures SHALL remain untranslated literals.

#### Scenario: Settings page renders through lookup

- **WHEN** the language is Simplified Chinese and the Appearance settings page is rendered
- **THEN** its page title, group titles, item labels, descriptions, and dropdown option labels all display Simplified Chinese text

#### Scenario: Diagnostics stay stable across languages

- **WHEN** the language is Simplified Chinese and the application writes a log line
- **THEN** the log line text is unaffected by the language setting
