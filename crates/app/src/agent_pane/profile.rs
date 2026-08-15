use crate::agent_pane::*;

/// Which agent backs this pane; the persisted tab snapshot stores the agent
/// name so future kinds can slot in without a schema change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AgentKind {
    Codex,
    Claude,
}

impl AgentKind {
    pub(crate) fn id(self) -> &'static str {
        match self {
            AgentKind::Codex => "codex",
            AgentKind::Claude => "claude",
        }
    }

    pub(crate) fn display(self) -> &'static str {
        match self {
            AgentKind::Codex => "Codex",
            AgentKind::Claude => "Claude",
        }
    }

    /// `None` for unknown kinds (a newer snapshot), which degrade to a plain
    /// terminal tab instead of losing the tab.
    pub(crate) fn from_id(id: &str) -> Option<Self> {
        match id {
            "codex" => Some(AgentKind::Codex),
            "claude" => Some(AgentKind::Claude),
            _ => None,
        }
    }

    pub(crate) fn from_profile(kind: AgentProfileKind) -> Self {
        match kind {
            AgentProfileKind::ClaudeCode => AgentKind::Claude,
            AgentProfileKind::Codex => AgentKind::Codex,
        }
    }

    pub(crate) fn profile_kind(self) -> AgentProfileKind {
        match self {
            AgentKind::Claude => AgentProfileKind::ClaudeCode,
            AgentKind::Codex => AgentProfileKind::Codex,
        }
    }
}

pub(super) const ANTHROPIC_MODEL_ENV: &str = "ANTHROPIC_MODEL";
pub(super) const OPENAI_API_KEY_ENV: &str = "OPENAI_API_KEY";

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

pub(super) fn launch_env_value(launch: &LaunchConfig, target: &str) -> Option<String> {
    launch
        .env
        .iter()
        .rev()
        .find(|(name, _)| name.trim().eq_ignore_ascii_case(target))
        .map(|(_, value)| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Turn a profile into a protocol-neutral launch spec. Generated environment
/// entries precede user entries so the explicit environment table retains
/// last-value-wins behavior.
pub(crate) fn agent_launch(profile: &AgentProfile) -> LaunchConfig {
    let mut env: Vec<(String, String)> = Vec::new();
    let model = (!profile.model.trim().is_empty()).then(|| profile.model.trim().to_string());

    if profile.use_custom_endpoint {
        let url = profile.api_base_url.trim();
        if profile.kind == AgentProfileKind::ClaudeCode && !url.is_empty() {
            env.push(("ANTHROPIC_BASE_URL".to_string(), url.to_string()));
        }

        let key = profile.api_key.trim();
        if !key.is_empty() {
            let key_env = match profile.kind {
                AgentProfileKind::ClaudeCode => "ANTHROPIC_API_KEY",
                AgentProfileKind::Codex => OPENAI_API_KEY_ENV,
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

    let api_key_env = env
        .iter()
        .rev()
        .find(|(name, _)| name.trim().eq_ignore_ascii_case(OPENAI_API_KEY_ENV))
        .filter(|(_, value)| !value.trim().is_empty())
        .map(|_| OPENAI_API_KEY_ENV.to_string());
    let codex_provider = (profile.kind == AgentProfileKind::Codex && profile.use_custom_endpoint)
        .then(|| profile.api_base_url.trim())
        .filter(|url| !url.is_empty())
        .map(|base_url| CodexProviderConfig {
            id: codex_provider_id(&profile.name),
            name: if profile.name.trim().is_empty() {
                "NiumaTerm custom endpoint".to_string()
            } else {
                profile.name.trim().to_string()
            },
            base_url: base_url.to_string(),
            api_key_env,
        });

    LaunchConfig {
        executable: profile.executable.trim().to_string(),
        env,
        model,
        effort: profile_effort(profile),
        provider: codex_provider,
    }
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
pub(crate) struct AgentThreadDefaults(pub(crate) HashMap<String, ThreadSettings>);

impl gpui::Global for AgentThreadDefaults {}

impl AgentThreadDefaults {
    pub(crate) fn from_local_state(stored: &BTreeMap<String, StoredAgentDefaults>) -> Self {
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

    pub(crate) fn to_local_state(&self) -> BTreeMap<String, StoredAgentDefaults> {
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
mod agent_profile_launch_tests {
    use nmt_config::profile::{AgentProfile, AgentProfileKind, EnvVar};

    use super::{
        ANTHROPIC_MODEL_ENV, ANTHROPIC_SUB_MODEL_ENVS, OPENAI_API_KEY_ENV, agent_launch,
        launch_env_value,
    };

    #[test]
    fn claude_profile_model_is_an_environment_default_with_user_override_last() {
        let profile = AgentProfile {
            name: "Claude Proxy".into(),
            kind: AgentProfileKind::ClaudeCode,
            executable: "claude".into(),
            model: "claude-profile-model".into(),
            env: vec![EnvVar {
                name: "anthropic_model".into(),
                value: "claude-env-override".into(),
            }],
            ..AgentProfile::default()
        };

        let launch = agent_launch(&profile);

        assert_eq!(
            launch_env_value(&launch, ANTHROPIC_MODEL_ENV).as_deref(),
            Some("claude-env-override")
        );
        assert!(launch.provider.is_none());
    }

    #[test]
    fn a_pinned_effort_reaches_the_launch_and_default_leaves_it_unset() {
        let pinned = AgentProfile {
            executable: "claude".into(),
            effort: "xhigh".into(),
            ..AgentProfile::default()
        };

        assert_eq!(agent_launch(&pinned).effort.as_deref(), Some("xhigh"));

        // The picker's own word for "no choice" and a profile written before
        // the field existed are the same state.
        for unset in ["", "default", "  "] {
            let profile = AgentProfile {
                executable: "claude".into(),
                effort: unset.into(),
                ..AgentProfile::default()
            };

            assert_eq!(agent_launch(&profile).effort, None);
        }
    }

    #[test]
    fn replacing_sub_models_points_every_tier_at_the_profile_model() {
        let profile = AgentProfile {
            kind: AgentProfileKind::ClaudeCode,
            executable: "claude".into(),
            model: "vendor/only-model".into(),
            replace_sub_models: true,
            ..AgentProfile::default()
        };

        let launch = agent_launch(&profile);

        assert_eq!(
            launch_env_value(&launch, ANTHROPIC_MODEL_ENV).as_deref(),
            Some("vendor/only-model")
        );
        for name in ANTHROPIC_SUB_MODEL_ENVS {
            assert_eq!(
                launch_env_value(&launch, name).as_deref(),
                Some("vendor/only-model"),
                "{name} should follow the profile model"
            );
        }
    }

    #[test]
    fn a_user_env_entry_overrides_a_replaced_sub_model() {
        // Case-insensitive, because Windows resolves process environment keys
        // that way and the launcher hands the table straight to the command.
        let profile = AgentProfile {
            kind: AgentProfileKind::ClaudeCode,
            executable: "claude".into(),
            model: "vendor/only-model".into(),
            replace_sub_models: true,
            env: vec![EnvVar {
                name: "anthropic_default_haiku_model".into(),
                value: "vendor/small-model".into(),
            }],
            ..AgentProfile::default()
        };

        let launch = agent_launch(&profile);

        assert_eq!(
            launch_env_value(&launch, "ANTHROPIC_DEFAULT_HAIKU_MODEL").as_deref(),
            Some("vendor/small-model")
        );
        assert_eq!(
            launch_env_value(&launch, "ANTHROPIC_DEFAULT_OPUS_MODEL").as_deref(),
            Some("vendor/only-model")
        );
    }

    #[test]
    fn sub_model_replacement_is_off_by_default_and_needs_a_model() {
        let off = AgentProfile {
            kind: AgentProfileKind::ClaudeCode,
            executable: "claude".into(),
            model: "vendor/only-model".into(),
            ..AgentProfile::default()
        };

        // Without a model there is nothing to propagate, so the switch alone
        // must not export empty overrides that would break model selection.
        let no_model = AgentProfile {
            kind: AgentProfileKind::ClaudeCode,
            executable: "claude".into(),
            replace_sub_models: true,
            ..AgentProfile::default()
        };

        for profile in [off, no_model] {
            let launch = agent_launch(&profile);

            for name in ANTHROPIC_SUB_MODEL_ENVS {
                assert_eq!(launch_env_value(&launch, name), None);
            }
        }
    }

    #[test]
    fn claude_custom_endpoint_exports_base_url_and_api_key() {
        // The runtime profile holds the decrypted URL and key restored by
        // nmt_config, so this pins the full path from restored values to the
        // provider environment.
        let profile = AgentProfile {
            name: "Claude Proxy".into(),
            kind: AgentProfileKind::ClaudeCode,
            executable: "claude".into(),
            use_custom_endpoint: true,
            api_base_url: "https://proxy.example.com".into(),
            api_key: "sk-test".into(),
            ..AgentProfile::default()
        };

        let launch = agent_launch(&profile);

        assert_eq!(
            launch_env_value(&launch, "ANTHROPIC_BASE_URL").as_deref(),
            Some("https://proxy.example.com")
        );
        assert_eq!(
            launch_env_value(&launch, "ANTHROPIC_API_KEY").as_deref(),
            Some("sk-test")
        );
        assert!(launch.provider.is_none());
    }

    #[test]
    fn codex_custom_endpoint_becomes_a_thread_provider_not_a_base_url_env_var() {
        let profile = AgentProfile {
            name: "Codex Proxy".into(),
            kind: AgentProfileKind::Codex,
            executable: "codex".into(),
            model: "vendor/custom-model".into(),
            use_custom_endpoint: true,
            api_base_url: "https://proxy.example.com/v1".into(),
            api_key: "secret".into(),
            ..AgentProfile::default()
        };

        let launch = agent_launch(&profile);
        let provider = launch.provider.as_ref().expect("custom provider");

        assert_eq!(launch.model.as_deref(), Some("vendor/custom-model"));
        assert_eq!(provider.base_url, "https://proxy.example.com/v1");
        assert_eq!(provider.api_key_env.as_deref(), Some(OPENAI_API_KEY_ENV));
        assert!(
            launch
                .env
                .iter()
                .all(|(name, _)| !name.eq_ignore_ascii_case("OPENAI_BASE_URL"))
        );
        assert_eq!(
            launch_env_value(&launch, OPENAI_API_KEY_ENV).as_deref(),
            Some("secret")
        );
    }
}
