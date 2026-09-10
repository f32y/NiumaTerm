use std::collections::{BTreeMap, HashMap};

use gpui::SharedString;
use gpui_component::{Icon, IconNamed};
use nmt_agent::chat::ThreadSettings;
use nmt_agent::{CodexProviderConfig, LaunchConfig, deepseek};
use nmt_config::local_state::AgentDefaults as StoredAgentDefaults;
use nmt_config::profile::{AgentProfile, AgentProfileKind, AgentProfileLauncher};

/// Provider icons, defined beside the kind they mark. The usage view and the
/// settings chrome borrow them from here.
pub struct CodexIcon;

impl IconNamed for CodexIcon {
    fn path(self) -> SharedString {
        "icons/codex.svg".into()
    }
}

pub struct ClaudeIcon;

impl IconNamed for ClaudeIcon {
    fn path(self) -> SharedString {
        "icons/claude.svg".into()
    }
}

pub struct DeepSeekIcon;

impl IconNamed for DeepSeekIcon {
    fn path(self) -> SharedString {
        "icons/deepseek.svg".into()
    }
}

pub use nmt_agent::session::AgentKind;

/// Application configuration and presentation for an agent kind.
pub trait AgentKindExt {
    fn from_profile(kind: AgentProfileKind) -> Self;
    fn profile_kind(self) -> AgentProfileKind;
    fn icon(self) -> Icon;
}

impl AgentKindExt for AgentKind {
    fn from_profile(kind: AgentProfileKind) -> Self {
        match kind {
            AgentProfileKind::ClaudeCode => AgentKind::Claude,
            AgentProfileKind::Codex => AgentKind::Codex,
            AgentProfileKind::DeepSeek => AgentKind::DeepSeek,
        }
    }

    fn profile_kind(self) -> AgentProfileKind {
        match self {
            AgentKind::Claude => AgentProfileKind::ClaudeCode,
            AgentKind::Codex => AgentProfileKind::Codex,
            AgentKind::DeepSeek => AgentProfileKind::DeepSeek,
        }
    }

    /// The harness's own mark. Tabs and the profile list read it from here so
    /// one kind cannot end up wearing another's glyph in one of them.
    fn icon(self) -> Icon {
        match self {
            AgentKind::Codex => Icon::new(CodexIcon),
            AgentKind::Claude => Icon::new(ClaudeIcon),
            AgentKind::DeepSeek => Icon::new(DeepSeekIcon),
        }
    }
}

pub(super) const ANTHROPIC_MODEL_ENV: &str = "ANTHROPIC_MODEL";
pub(super) const OPENAI_API_KEY_ENV: &str = "OPENAI_API_KEY";
const CODEX_CREDENTIAL_ENV_PREFIX: &str = "NIUMATERM_CODEX_API_KEY_";
/// DeepSeek Harness layers credential sources by trust and puts the inherited
/// process environment above its own managed store, so a key exported here
/// authenticates the host whatever that store holds.
pub(super) const DEEPSEEK_API_KEY_ENV: &str = "DEEPSEEK_API_KEY";
/// The endpoint DeepSeek Harness routes to when its own settings document names
/// none, which is the state a stock installation is in. A `baseURL` written
/// through the harness's own Models page outranks this, because that document is
/// a deliberate local override rather than a default.
pub(super) const DEEPSEEK_BASE_URL_ENV: &str = "DEEPSEEK_BASE_URL";

/// The per-tier model overrides Claude Code reads when it dispatches work to
/// something other than the primary model. A profile that pins every tier to
/// its own model keeps a single-model endpoint from being asked for the three
/// stock Anthropic names.
pub(super) const ANTHROPIC_SUB_MODEL_ENVS: [&str; 3] = [
    "ANTHROPIC_DEFAULT_OPUS_MODEL",
    "ANTHROPIC_DEFAULT_SONNET_MODEL",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
];

/// A deterministic provider id keeps Codex history scoped to the profile
/// without exposing display names as config keys. Profile names are already
/// unique and act as the identity for restored tabs and remembered settings.
fn codex_provider_id(profile_name: &str) -> String {
    let mut hash = 0xcbf29ce484222325_u64;

    for byte in profile_name.trim().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }

    format!("niumaterm-{hash:016x}")
}

fn codex_credential_env(provider_id: &str) -> String {
    let suffix: String = provider_id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();

    format!("{CODEX_CREDENTIAL_ENV_PREFIX}{suffix}")
}

pub(super) fn launch_env_value(launch: &LaunchConfig, target: &str) -> Option<String> {
    launch_env_value_from_entries(&launch.env, target)
}

/// Turn a profile into a protocol-neutral launch spec. Generated environment
/// entries precede user entries so the explicit environment table retains
/// last-value-wins behavior.
pub fn agent_launch(profile: &AgentProfile) -> LaunchConfig {
    let mut env: Vec<(String, String)> = Vec::new();
    let model = (!profile.model.trim().is_empty()).then(|| profile.model.trim().to_string());

    let codex_provider_id = (profile.kind == AgentProfileKind::Codex
        && profile.use_custom_endpoint
        && !profile.api_base_url.trim().is_empty())
    .then(|| codex_provider_id(&profile.name));

    let codex_credential_env = codex_provider_id.as_deref().map(codex_credential_env);

    if profile.use_custom_endpoint {
        // Codex is absent because it reaches its endpoint through a generated
        // provider entry rather than an environment variable; that entry is
        // built from the same field further down.
        let base_url_env = match profile.kind {
            AgentProfileKind::ClaudeCode => Some("ANTHROPIC_BASE_URL"),
            AgentProfileKind::DeepSeek => Some(DEEPSEEK_BASE_URL_ENV),
            AgentProfileKind::Codex => None,
        };

        let url = profile.api_base_url.trim();

        if let Some(name) = base_url_env
            && !url.is_empty()
        {
            env.push((name.to_string(), url.to_string()));
        }

        let key = profile.api_key.trim();

        if !key.is_empty() {
            let key_env = match profile.kind {
                AgentProfileKind::ClaudeCode => "ANTHROPIC_API_KEY",
                AgentProfileKind::Codex => codex_credential_env
                    .as_deref()
                    .unwrap_or(OPENAI_API_KEY_ENV),
                AgentProfileKind::DeepSeek => DEEPSEEK_API_KEY_ENV,
            };

            env.push((key_env.to_string(), key.to_string()));
        }
    }

    if profile.kind == AgentProfileKind::ClaudeCode
        && let Some(model) = model.as_ref()
    {
        env.push((ANTHROPIC_MODEL_ENV.to_string(), model.clone()));

        if profile.replace_sub_models {
            for name in ANTHROPIC_SUB_MODEL_ENVS {
                env.push((name.to_string(), model.clone()));
            }
        }
    }

    env.extend(
        profile
            .env
            .iter()
            .filter(|var| !var.name.trim().is_empty())
            .map(|var| (var.name.trim().to_string(), var.value.clone())),
    );

    if let Some(generated_name) = codex_credential_env.as_deref() {
        let generated_value = launch_env_value_from_entries(&env, generated_name)
            .or_else(|| launch_env_value_from_entries(&env, OPENAI_API_KEY_ENV));

        env.retain(|(name, _)| !name.eq_ignore_ascii_case(OPENAI_API_KEY_ENV));

        if launch_env_value_from_entries(&env, generated_name).is_none()
            && let Some(value) = generated_value
        {
            env.push((generated_name.to_string(), value));
        }
    }

    let api_key_env =
        codex_credential_env.filter(|name| launch_env_value_from_entries(&env, name).is_some());

    let codex_provider = codex_provider_id.map(|id| CodexProviderConfig {
        id,
        name: if profile.name.trim().is_empty() {
            "NiumaTerm custom endpoint".to_string()
        } else {
            profile.name.trim().to_string()
        },
        base_url: profile.api_base_url.trim().to_string(),
        api_key_env,
    });

    // Package launchers move the package name into their own arguments and
    // leave the configured executable unused. Other agent kinds always launch
    // their configured executable even if a hand-edited file names a package
    // launcher that their adapter does not support.
    let (executable, executable_args) = match (profile.kind, profile.launcher) {
        (AgentProfileKind::DeepSeek, AgentProfileLauncher::Npx) => (
            deepseek::NPX_EXECUTABLE.to_string(),
            deepseek::NPX_ARGUMENTS.map(str::to_string).to_vec(),
        ),
        (AgentProfileKind::DeepSeek, AgentProfileLauncher::PnpmDlx) => (
            deepseek::PNPM_DLX_EXECUTABLE.to_string(),
            deepseek::PNPM_DLX_ARGUMENTS.map(str::to_string).to_vec(),
        ),
        _ => (profile.executable.trim().to_string(), Vec::new()),
    };

    LaunchConfig {
        executable,
        executable_args,
        env,
        model,
        effort: profile_effort(profile),
        provider: codex_provider,
        // Only the harness keeps a provider catalog to declare a model in, and
        // only a model this profile names can be declared in it.
        declares_image_input: profile.kind == AgentProfileKind::DeepSeek
            && profile.vision_model
            && !profile.model.trim().is_empty(),
    }
}

fn launch_env_value_from_entries(env: &[(String, String)], target: &str) -> Option<String> {
    env.iter()
        .rev()
        .find(|(name, _)| name.trim().eq_ignore_ascii_case(target))
        .map(|(_, value)| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// The reasoning effort this profile pins, or `None` when it leaves the choice
/// to the remembered pick and the agent. The literal `default` is accepted
/// alongside an empty field because that word is the picker's own label for
/// "no choice".
pub(crate) fn profile_effort(profile: &AgentProfile) -> Option<String> {
    let effort = profile.effort.trim();

    (!effort.is_empty() && effort != "default").then(|| effort.to_string())
}

/// Last-chosen thread settings per agent profile name (agent ID for
/// entries written by older builds), seeding the dropdowns of newly opened
/// conversations, resumed Claude conversations, and the reviewer of resumed
/// Codex threads. Loaded from local_state.toml at startup, saved after user
/// changes, and included in the final quit snapshot.
#[derive(Default)]
pub struct AgentThreadDefaults(pub(crate) HashMap<String, ThreadSettings>);

impl gpui::Global for AgentThreadDefaults {}

impl AgentThreadDefaults {
    pub fn from_local_state(stored: &BTreeMap<String, StoredAgentDefaults>) -> Self {
        Self(
            stored
                .iter()
                .map(|(kind, d)| {
                    (
                        kind.clone(),
                        ThreadSettings {
                            model: d.model.clone(),
                            approval: d.approval.clone(),
                            approvals_reviewer: d.approvals_reviewer.clone(),
                            sandbox: d.sandbox.clone(),
                            effort: d.effort.clone(),
                            tier: d.tier.clone(),
                        },
                    )
                })
                .collect(),
        )
    }

    pub fn to_local_state(&self) -> BTreeMap<String, StoredAgentDefaults> {
        self.0
            .iter()
            .map(|(kind, s)| {
                (
                    kind.clone(),
                    StoredAgentDefaults {
                        model: s.model.clone(),
                        approval: s.approval.clone(),
                        approvals_reviewer: s.approvals_reviewer.clone(),
                        sandbox: s.sandbox.clone(),
                        effort: s.effort.clone(),
                        tier: s.tier.clone(),
                    },
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod agent_profile_launch_tests;
