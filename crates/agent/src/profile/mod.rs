//! Provider launch rules independent of application configuration storage.

use crate::session::AgentKind;
use crate::{CodexProviderConfig, LaunchConfig, deepseek};

#[derive(Clone, Copy)]
pub enum ProfileLauncher {
    Executable,
    Npx,
    PnpmDlx,
}

pub struct LaunchProfile<'a> {
    pub kind: AgentKind,
    pub name: &'a str,
    pub executable: &'a str,
    pub launcher: ProfileLauncher,
    pub model: &'a str,
    pub effort: &'a str,
    pub use_custom_endpoint: bool,
    pub api_base_url: &'a str,
    pub api_key: &'a str,
    pub replace_sub_models: bool,
    pub vision_model: bool,
}

pub const ANTHROPIC_MODEL_ENV: &str = "ANTHROPIC_MODEL";
pub const OPENAI_API_KEY_ENV: &str = "OPENAI_API_KEY";
pub const CODEX_CREDENTIAL_ENV_PREFIX: &str = "NIUMATERM_CODEX_API_KEY_";
/// DeepSeek Harness layers credential sources by trust and puts the inherited
/// process environment above its own managed store, so a key exported here
/// authenticates the host whatever that store holds.
pub const DEEPSEEK_API_KEY_ENV: &str = "DEEPSEEK_API_KEY";
/// The endpoint DeepSeek Harness routes to when its own settings document names
/// none, which is the state a stock installation is in. A `baseURL` written
/// through the harness's own Models page outranks this, because that document is
/// a deliberate local override rather than a default.
pub const DEEPSEEK_BASE_URL_ENV: &str = "DEEPSEEK_BASE_URL";

/// The per-tier model overrides Claude Code reads when it dispatches work to
/// something other than the primary model. A profile that pins every tier to
/// its own model keeps a single-model endpoint from being asked for the three
/// stock Anthropic names.
pub const ANTHROPIC_SUB_MODEL_ENVS: [&str; 3] = [
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

pub fn launch_env_value(launch: &LaunchConfig, target: &str) -> Option<String> {
    launch_env_value_from_entries(&launch.env, target)
}

/// Turn a profile into a protocol-neutral launch spec. Generated environment
/// entries precede user entries so the explicit environment table retains
/// last-value-wins behavior.
pub fn agent_launch<'a>(
    profile: &LaunchProfile<'a>,
    overrides: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> LaunchConfig {
    let mut env: Vec<(String, String)> = Vec::new();
    let model = (!profile.model.trim().is_empty()).then(|| profile.model.trim().to_string());

    let codex_provider_id = (profile.kind == AgentKind::Codex
        && profile.use_custom_endpoint
        && !profile.api_base_url.trim().is_empty())
    .then(|| codex_provider_id(profile.name));

    let codex_credential_env = codex_provider_id.as_deref().map(codex_credential_env);

    if profile.use_custom_endpoint {
        // Codex is absent because it reaches its endpoint through a generated
        // provider entry rather than an environment variable; that entry is
        // built from the same field further down.
        let base_url_env = match profile.kind {
            AgentKind::Claude => Some("ANTHROPIC_BASE_URL"),
            AgentKind::DeepSeek => Some(DEEPSEEK_BASE_URL_ENV),
            AgentKind::Codex => None,
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
                AgentKind::Claude => "ANTHROPIC_API_KEY",
                AgentKind::Codex => codex_credential_env
                    .as_deref()
                    .unwrap_or(OPENAI_API_KEY_ENV),
                AgentKind::DeepSeek => DEEPSEEK_API_KEY_ENV,
            };

            env.push((key_env.to_string(), key.to_string()));
        }
    }

    if profile.kind == AgentKind::Claude
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
        overrides
            .into_iter()
            .filter(|(name, _)| !name.trim().is_empty())
            .map(|(name, value)| (name.trim().to_string(), value.to_string())),
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
        (AgentKind::DeepSeek, ProfileLauncher::Npx) => (
            deepseek::NPX_EXECUTABLE.to_string(),
            deepseek::NPX_ARGUMENTS.map(str::to_string).to_vec(),
        ),
        (AgentKind::DeepSeek, ProfileLauncher::PnpmDlx) => (
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
        declares_image_input: profile.kind == AgentKind::DeepSeek
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
pub fn profile_effort(profile: &LaunchProfile<'_>) -> Option<String> {
    let effort = profile.effort.trim();

    (!effort.is_empty() && effort != "default").then(|| effort.to_string())
}

pub fn launch_model(kind: AgentKind, launch: LaunchConfig) -> Option<String> {
    match kind {
        AgentKind::Claude => launch_env_value(&launch, ANTHROPIC_MODEL_ENV),
        AgentKind::Codex => launch.model,
        AgentKind::DeepSeek => None,
    }
}
