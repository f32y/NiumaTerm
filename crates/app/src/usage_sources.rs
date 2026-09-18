use std::sync::Arc;

use app::agent_tab::profile::agent_launch;
use chrono::Local;
use futures::FutureExt as _;
use nmt_agent::claude_code::usage_fetcher::{UsageFetchError, fetch_with_cancel};
use nmt_agent::codex::usage_fetcher::fetch;
use nmt_agent::launcher::AgentCli;
use nmt_agent::usage::{FetchCancellation, UsageSnapshot};
use nmt_config::profile::AgentKind;
use nmt_platform::process::{decode_child_output, hidden_cmd_command, output};

use crate::daily_usage::{DailyTokenUsage, parse_usage};
use crate::ui::AppSettings;
use crate::usage_refresh::{FetchError, UsageSource};

pub(crate) fn account_sources(launcher: AgentCli) -> [UsageSource<UsageSnapshot>; 2] {
    [
        codex_source(launcher),
        Arc::new(|cancellation: Arc<FetchCancellation>| {
            async move {
                fetch_with_cancel(&cancellation)
                    .await
                    .map_err(|error| match error {
                        UsageFetchError::Cancelled => FetchError::Cancelled,
                        UsageFetchError::Failed(message) => FetchError::Failed(message),
                    })
            }
            .boxed()
        }),
    ]
}

pub(crate) fn codex_usage_launcher(settings: &AppSettings) -> AgentCli {
    let profile = settings
        .config()
        .agent_profiles
        .list
        .iter()
        .filter(|profile| profile.kind == AgentKind::Codex)
        .find(|profile| profile.name == settings.config().agent_profiles.default)
        .or_else(|| {
            settings
                .config()
                .agent_profiles
                .list
                .iter()
                .find(|profile| profile.kind == AgentKind::Codex)
        });

    let launch = profile.map(agent_launch).unwrap_or_default();

    AgentCli::from_launch(&launch, "codex")
}

pub(crate) fn codex_source(launcher: AgentCli) -> UsageSource<UsageSnapshot> {
    Arc::new(move |cancellation: Arc<FetchCancellation>| {
        let launcher = launcher.clone();

        async move {
            fetch(&launcher, &cancellation)
                .await
                .map_err(FetchError::Failed)
        }
        .boxed()
    })
}

pub(crate) fn daily_source() -> UsageSource<Option<DailyTokenUsage>> {
    Arc::new(|_| fetch_daily_usage().boxed())
}

async fn fetch_daily_usage() -> Result<Option<DailyTokenUsage>, FetchError> {
    let now = Local::now();
    let since = now.format("%Y%m%d").to_string();
    let date = now.format("%Y-%m-%d").to_string();

    let mut command = hidden_cmd_command("npx");

    command.args(["ccusage@latest", "-j", "--since", &since]);

    let output = output(command)
        .await
        .map_err(|error| FetchError::Failed(format!("failed to run ccusage: {error}")))?;

    if !output.status.success() {
        return Err(FetchError::Failed(format!(
            "ccusage exited with {}: {}",
            output.status,
            decode_child_output(&output.stderr).trim()
        )));
    }

    parse_usage(&output.stdout, &date)
        .map(Some)
        .map_err(FetchError::Failed)
}

#[cfg(test)]
mod tests {
    use nmt_config::Config;
    use nmt_config::profile::{AgentKind, AgentProfile, AgentProfilesConfig, EnvVar};

    use crate::ui::AppSettings;
    use crate::usage_sources::codex_usage_launcher;

    #[test]
    fn account_usage_prefers_the_default_codex_profile() {
        let mut settings = AppSettings::from_config(Config {
            agent_profiles: AgentProfilesConfig {
                list: vec![
                    AgentProfile {
                        name: "First".into(),
                        kind: AgentKind::Codex,
                        executable: "first-codex".into(),
                        ..AgentProfile::default()
                    },
                    AgentProfile {
                        name: "Chosen".into(),
                        kind: AgentKind::Codex,
                        executable: "chosen-codex".into(),
                        env: vec![EnvVar {
                            name: "CODEX_HOME".into(),
                            value: "custom-home".into(),
                        }],
                        ..AgentProfile::default()
                    },
                ],
                default: "Chosen".into(),
                initialized: true,
            },
            ..Config::default()
        });

        let launcher = codex_usage_launcher(&settings);

        assert_eq!(launcher.executable(), "chosen-codex");
        assert_eq!(
            launcher.effective_env_os("CODEX_HOME"),
            Some("custom-home".into())
        );

        settings.save_agent_profile(
            None,
            AgentProfile {
                name: "Another provider".into(),
                kind: AgentKind::Claude,
                ..AgentProfile::default()
            },
        );

        settings.set_default_agent_profile("Another provider".into());

        assert_eq!(codex_usage_launcher(&settings).executable(), "first-codex");

        while !settings.config().agent_profiles.list.is_empty() {
            settings.remove_agent_profile(0);
        }

        assert_eq!(codex_usage_launcher(&settings).executable(), "codex");
    }
}
