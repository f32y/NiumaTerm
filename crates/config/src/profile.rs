//! Shell profiles, persisted as top-level `[[profiles]]` entries in
//! `config.toml` by the settings dialog.

use serde::{Deserialize, Serialize};
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, value};

use crate::{credentials, ensure_explicit_table};

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

/// The `[agent-profiles]` section: the default agent-profile name plus the
/// entries (`[[agent-profiles.list]]`). Same nested-list layout as
/// `[profiles]`, for the same TOML reason.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AgentProfilesConfig {
    /// Name of the default agent profile; empty falls back to the first one.
    #[serde(default)]
    pub default: String,
    /// True once the settings dialog has managed this section. Distinguishes
    /// "never configured" (seed the built-in profiles) from "user deleted
    /// every profile" (respect the empty list).
    #[serde(default)]
    pub initialized: bool,
    #[serde(default)]
    pub list: Vec<AgentProfile>,
}

/// Which agent CLI protocol a profile speaks; decides the spawn command line
/// and which provider env vars a custom endpoint maps to.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AgentProfileKind {
    #[default]
    ClaudeCode,
    Codex,
}

impl AgentProfileKind {
    pub fn as_str(self) -> &'static str {
        match self {
            AgentProfileKind::ClaudeCode => "claude-code",
            AgentProfileKind::Codex => "codex",
        }
    }
}

/// One environment variable applied to the agent process on launch.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct EnvVar {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub value: String,
}

/// One `[[agent-profiles.list]]` entry. The runtime type keeps the custom
/// API URL and API key as plaintext strings for the settings editor and the
/// launch adapters; on disk they live in one encrypted `api-credentials`
/// value, decrypted through [`PersistedAgentProfile`] during load.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(try_from = "PersistedAgentProfile")]
pub struct AgentProfile {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub kind: AgentProfileKind,
    /// Executable name or path; a bare name resolves via PATH (and PATHEXT on
    /// Windows, so `claude` finds both `claude.exe` and the npm `claude.cmd`).
    #[serde(default)]
    pub executable: String,
    /// Model selected when a new agent conversation starts. Each adapter maps
    /// this to its native configuration surface.
    #[serde(default)]
    pub model: String,
    /// Reasoning effort forced on every conversation this profile starts.
    /// Empty leaves the choice to the remembered thread settings and whatever
    /// the agent reports, which is what the pickers showed before this field
    /// existed.
    #[serde(default)]
    pub effort: String,
    /// Point Claude Code's per-tier model settings at [`Self::model`] too, so
    /// a custom endpoint that serves a single model still answers the requests
    /// the CLI routes to its Opus, Sonnet, and Haiku tiers.
    #[serde(default, rename = "replace-sub-models")]
    pub replace_sub_models: bool,
    #[serde(default, rename = "use-custom-endpoint")]
    pub use_custom_endpoint: bool,
    #[serde(default, rename = "api-base-url")]
    pub api_base_url: String,
    #[serde(default, rename = "api-key")]
    pub api_key: String,
    #[serde(default)]
    pub env: Vec<EnvVar>,
}

/// On-disk shape of one `[[agent-profiles.list]]` entry. Credentials arrive
/// either as the encrypted `api-credentials` value or as the legacy plaintext
/// fields written by builds that predate encryption.
#[derive(Deserialize)]
struct PersistedAgentProfile {
    #[serde(default)]
    name: String,
    #[serde(default)]
    kind: AgentProfileKind,
    #[serde(default)]
    executable: String,
    #[serde(default)]
    model: String,
    #[serde(default)]
    effort: String,
    #[serde(default, rename = "replace-sub-models")]
    replace_sub_models: bool,
    #[serde(default, rename = "use-custom-endpoint")]
    use_custom_endpoint: bool,
    #[serde(default, rename = "api-credentials")]
    api_credentials: Option<String>,
    #[serde(default, rename = "api-base-url")]
    api_base_url: String,
    #[serde(default, rename = "api-key")]
    api_key: String,
    #[serde(default)]
    env: Vec<EnvVar>,
}

impl TryFrom<PersistedAgentProfile> for AgentProfile {
    type Error = String;

    fn try_from(persisted: PersistedAgentProfile) -> Result<Self, Self::Error> {
        // An encrypted value always wins over adjacent legacy fields, even
        // when it fails to decrypt: falling back would let a modified
        // ciphertext silently downgrade the profile to attacker-visible or
        // stale plaintext left beside it. The error names the profile but
        // never its credential data.
        let (api_base_url, api_key) = match &persisted.api_credentials {
            Some(stored) => credentials::decrypt(stored).map_err(|err| {
                format!(
                    "agent profile \"{}\": cannot read api-credentials: {err}",
                    persisted.name
                )
            })?,
            None => (persisted.api_base_url, persisted.api_key),
        };
        Ok(AgentProfile {
            name: persisted.name,
            kind: persisted.kind,
            executable: persisted.executable,
            model: persisted.model,
            effort: persisted.effort,
            replace_sub_models: persisted.replace_sub_models,
            use_custom_endpoint: persisted.use_custom_endpoint,
            api_base_url,
            api_key,
            env: persisted.env,
        })
    }
}

/// Write the `[profiles]` section (`default` plus the `[[profiles.list]]`
/// entries) into a parsed `config.toml` document, replacing any existing one.
pub(crate) fn patch_document(doc: &mut DocumentMut, profiles: &[Profile], default_profile: &str) {
    ensure_explicit_table(doc, "profiles");
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

/// Write the `[agent-profiles]` section (`default` plus the
/// `[[agent-profiles.list]]` entries) into a parsed `config.toml` document,
/// replacing any existing one. Credentials are written only as the encrypted
/// `api-credentials` value; rebuilding every entry from the runtime type is
/// what removes legacy plaintext fields on the first save after migration.
/// An encryption failure aborts the whole patch so the caller never persists
/// a document with missing credentials.
pub(crate) fn patch_agent_document(
    doc: &mut DocumentMut,
    profiles: &[AgentProfile],
    default_profile: &str,
) -> Result<(), String> {
    ensure_explicit_table(doc, "agent-profiles");
    doc["agent-profiles"]["default"] = value(default_profile);
    // Saving means the dialog managed this section; from now on an empty
    // list is a deliberate state, never re-seeded.
    doc["agent-profiles"]["initialized"] = value(true);

    let mut tables = ArrayOfTables::new();
    for profile in profiles {
        let mut table = Table::new();
        table["name"] = value(&profile.name);
        table["kind"] = value(profile.kind.as_str());
        table["executable"] = value(&profile.executable);
        table["model"] = value(&profile.model);
        table["effort"] = value(&profile.effort);
        table["replace-sub-models"] = value(profile.replace_sub_models);
        table["use-custom-endpoint"] = value(profile.use_custom_endpoint);
        if !profile.api_base_url.is_empty() || !profile.api_key.is_empty() {
            let stored =
                credentials::encrypt(&profile.api_base_url, &profile.api_key).map_err(|err| {
                    format!(
                        "agent profile \"{}\": cannot save credentials: {err}",
                        profile.name
                    )
                })?;
            table["api-credentials"] = value(stored);
        }

        let mut env = toml_edit::Array::new();
        for var in &profile.env {
            let mut entry = toml_edit::InlineTable::new();
            entry.insert("name", var.name.as_str().into());
            entry.insert("value", var.value.as_str().into());
            env.push(entry);
        }
        table["env"] = value(env);

        tables.push(table);
    }
    doc["agent-profiles"]["list"] = Item::ArrayOfTables(tables);
    Ok(())
}
