use nmt_config::profile::{AgentKind, AgentProfile, AgentProfileLauncher, DshVersion, EnvVar};

use crate::agent_tab::profile::{
    ANTHROPIC_MODEL_ENV, ANTHROPIC_SUB_MODEL_ENVS, CODEX_CREDENTIAL_ENV_PREFIX,
    DEEPSEEK_API_KEY_ENV, DEEPSEEK_BASE_URL_ENV, OPENAI_API_KEY_ENV, agent_launch,
    launch_env_value,
};

#[test]
fn claude_profile_model_is_an_environment_default_with_user_override_last() {
    let profile = AgentProfile {
        name: "Claude Proxy".into(),
        kind: AgentKind::Claude,
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
fn pinned_approval_and_sandbox_reach_the_launch_only_in_their_harness_values() {
    let profile = |kind, approval: &str, sandbox: &str| AgentProfile {
        kind,
        executable: "agent".into(),
        approval: approval.into(),
        sandbox: sandbox.into(),
        ..AgentProfile::default()
    };

    let claude = agent_launch(&profile(AgentKind::Claude, "acceptEdits", "readOnly"));

    assert_eq!(claude.approval.as_deref(), Some("acceptEdits"));
    assert_eq!(claude.sandbox, None, "Claude Code has no sandbox policy");

    let codex = agent_launch(&profile(AgentKind::Codex, "never", "dangerFullAccess"));

    assert_eq!(codex.approval.as_deref(), Some("never"));
    assert_eq!(codex.sandbox.as_deref(), Some("dangerFullAccess"));

    // A value from the other harness, left behind by a kind switch or a hand
    // edit, would be refused by the harness, so it pins nothing.
    assert_eq!(
        agent_launch(&profile(AgentKind::Codex, "acceptEdits", "")).approval,
        None
    );
    assert_eq!(
        agent_launch(&profile(AgentKind::Claude, "never", "")).approval,
        None
    );

    for unset in ["", "default", "  "] {
        let launch = agent_launch(&profile(AgentKind::Codex, unset, unset));

        assert_eq!((launch.approval, launch.sandbox), (None, None));
    }
}

#[test]
fn replacing_sub_models_points_every_tier_at_the_profile_model() {
    let profile = AgentProfile {
        kind: AgentKind::Claude,
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
        kind: AgentKind::Claude,
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
        kind: AgentKind::Claude,
        executable: "claude".into(),
        model: "vendor/only-model".into(),
        ..AgentProfile::default()
    };

    // Without a model there is nothing to propagate, so the switch alone
    // must not export empty overrides that would break model selection.
    let no_model = AgentProfile {
        kind: AgentKind::Claude,
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
        kind: AgentKind::Claude,
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
fn deepseek_custom_endpoint_exports_base_url_and_api_key() {
    // The harness reads both from the environment it is launched with and
    // ranks that above its own stored credentials, so exporting them is the
    // whole of pointing a profile at another provider.
    let profile = AgentProfile {
        name: "DeepSeek Proxy".into(),
        kind: AgentKind::DeepSeek,
        executable: "dsh".into(),
        use_custom_endpoint: true,
        api_base_url: "https://gateway.example.com/v1".into(),
        api_key: "sk-deepseek".into(),
        ..AgentProfile::default()
    };

    let launch = agent_launch(&profile);

    assert_eq!(
        launch_env_value(&launch, DEEPSEEK_BASE_URL_ENV).as_deref(),
        Some("https://gateway.example.com/v1")
    );
    assert_eq!(
        launch_env_value(&launch, DEEPSEEK_API_KEY_ENV).as_deref(),
        Some("sk-deepseek")
    );

    // The endpoint is environment, not a generated provider entry,
    // which is the shape only Codex needs.
    assert!(launch.provider.is_none());
}

#[test]
fn a_deepseek_profile_without_the_switch_exports_no_endpoint() {
    // A URL left in the field while the switch is off is a draft the user
    // did not turn on; exporting it would route the harness somewhere they
    // did not ask for.
    let profile = AgentProfile {
        kind: AgentKind::DeepSeek,
        executable: "dsh".into(),
        api_base_url: "https://gateway.example.com/v1".into(),
        api_key: "sk-deepseek".into(),
        ..AgentProfile::default()
    };

    let launch = agent_launch(&profile);

    assert_eq!(launch_env_value(&launch, DEEPSEEK_BASE_URL_ENV), None);
    assert_eq!(launch_env_value(&launch, DEEPSEEK_API_KEY_ENV), None);
}

#[test]
fn deepseek_package_launchers_pin_the_selected_release() {
    for (version, package) in [
        (DshVersion::V0_2_1, "@deepseek-ai/dsh@0.2.1-rc.2"),
        (DshVersion::V0_2_0, "@deepseek-ai/dsh@0.2.0-rc.2"),
        (DshVersion::V0_1_5, "@deepseek-ai/dsh@0.1.5-rc.1"),
    ] {
        let cases = [
            (AgentProfileLauncher::Npx, "npx", vec!["-y", package]),
            (
                AgentProfileLauncher::PnpmDlx,
                "pnpm",
                vec!["dlx", "--config.dlx-cache-max-age=Infinity", package],
            ),
        ];

        for (launcher, expected_executable, expected_args) in cases {
            let profile = AgentProfile {
                kind: AgentKind::DeepSeek,
                executable: "ignored-dsh".into(),
                launcher,
                dsh_version: version,
                ..AgentProfile::default()
            };

            let launch = agent_launch(&profile);

            assert_eq!(launch.executable, expected_executable);
            assert_eq!(launch.executable_args, expected_args);
        }
    }
}

#[test]
fn codex_custom_endpoint_becomes_a_thread_provider_not_a_base_url_env_var() {
    let profile = AgentProfile {
        name: "Codex Proxy".into(),
        kind: AgentKind::Codex,
        executable: "codex".into(),
        model: "vendor/custom-model".into(),
        use_custom_endpoint: true,
        api_base_url: "https://proxy.example.com/v1".into(),
        api_key: "secret".into(),
        ..AgentProfile::default()
    };

    let launch = agent_launch(&profile);
    let provider = launch.provider.as_ref().expect("custom provider");
    let credential_env = provider.api_key_env.as_deref().expect("credential env");

    assert_eq!(launch.model.as_deref(), Some("vendor/custom-model"));
    assert_eq!(provider.base_url, "https://proxy.example.com/v1");
    assert!(credential_env.starts_with(CODEX_CREDENTIAL_ENV_PREFIX));
    assert!(
        launch
            .env
            .iter()
            .all(|(name, _)| !name.eq_ignore_ascii_case("OPENAI_BASE_URL"))
    );
    assert_eq!(
        launch_env_value(&launch, credential_env).as_deref(),
        Some("secret")
    );
    assert_eq!(launch_env_value(&launch, OPENAI_API_KEY_ENV), None);
}
