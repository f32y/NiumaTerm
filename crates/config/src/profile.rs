//! Shell profiles, persisted as top-level `[[profiles]]` entries in
//! `config.toml` by the settings dialog.

use serde::{Deserialize, Serialize};
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, value};

/// The `[profiles]` section: the default-profile name plus the profile
/// entries (`[[profiles.list]]`). TOML cannot mix a scalar key with
/// array-of-tables entries under the same name, hence the nested list.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ProfilesConfig {
    /// Name of the default profile; empty falls back to the first profile.
    #[serde(default)]
    pub default: String,
    #[serde(default)]
    pub list: Vec<Profile>,
}

/// One `[[profiles]]` entry. An empty list means "use the app's built-in
/// default profile".
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Profile {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub shell: String,
    #[serde(default)]
    pub args: String,
}

/// Write the `[profiles]` section (`default` plus the `[[profiles.list]]`
/// entries) into a parsed `config.toml` document, replacing any existing one.
pub(crate) fn patch_document(doc: &mut DocumentMut, profiles: &[Profile], default_profile: &str) {
    crate::appearance::ensure_explicit_table(doc, "profiles");
    doc["profiles"]["default"] = value(default_profile);

    let mut tables = ArrayOfTables::new();
    for profile in profiles {
        let mut table = Table::new();
        table["name"] = value(&profile.name);
        table["shell"] = value(&profile.shell);
        table["args"] = value(&profile.args);
        tables.push(table);
    }
    doc["profiles"]["list"] = Item::ArrayOfTables(tables);
}
