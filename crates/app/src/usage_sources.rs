use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use app::agent_tab::profile::agent_launch;
use chrono::Local;
use nmt_agent::claude_code::usage_fetcher::{UsageFetchError, fetch_with_cancel};
use nmt_agent::codex::usage_fetcher::fetch;
use nmt_agent::launcher::AgentCli;
use nmt_agent::usage::UsageSnapshot;
use nmt_platform::process::{decode_child_output, hidden_cmd_command};

use crate::daily_usage::{DailyTokenUsage, parse_usage};
use nmt_config::profile::AgentProfileKind;

use crate::ui::AppSettings;
use crate::usage_refresh::{FetchError, UsageSource};

pub(crate) fn account_sources(launcher: AgentCli) -> [Arc<dyn UsageSource<UsageSnapshot>>; 2] {
    [
        codex_source(launcher),
        Arc::new(|cancelled: &AtomicBool| {
            fetch_with_cancel(cancelled).map_err(|error| match error {
                UsageFetchError::Cancelled => FetchError::Cancelled,
                UsageFetchError::Failed(message) => FetchError::Failed(message),
            })
        }),
    ]
}

pub(crate) fn codex_usage_launcher(settings: &AppSettings) -> AgentCli {
    let profile = settings
        .agent_profiles
        .iter()
        .filter(|profile| profile.kind == AgentProfileKind::Codex)
        .find(|profile| profile.name == settings.default_agent_profile)
        .or_else(|| {
            settings
                .agent_profiles
                .iter()
                .find(|profile| profile.kind == AgentProfileKind::Codex)
        });

    let launch = profile.map(agent_launch).unwrap_or_default();

    AgentCli::from_launch(&launch, "codex")
}

pub(crate) fn codex_source(launcher: AgentCli) -> Arc<dyn UsageSource<UsageSnapshot>> {
    Arc::new(move |cancelled: &AtomicBool| fetch(&launcher, cancelled).map_err(FetchError::Failed))
}

pub(crate) fn daily_source() -> Arc<dyn UsageSource<Option<DailyTokenUsage>>> {
    Arc::new(fetch_daily_usage)
}

fn fetch_daily_usage(_: &AtomicBool) -> Result<Option<DailyTokenUsage>, FetchError> {
    let now = Local::now();
    let since = now.format("%Y%m%d").to_string();
    let date = now.format("%Y-%m-%d").to_string();

    let output = hidden_cmd_command("npx")
        .args(["ccusage@latest", "-j", "--since", &since])
        .output()
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
    use nmt_config::profile::{AgentProfile, AgentProfileKind, EnvVar};

    use crate::ui::AppSettings;
    use crate::usage_sources::codex_usage_launcher;

    #[test]
    fn account_usage_prefers_the_default_codex_profile() {
        let mut settings = AppSettings {
            agent_profiles: vec![
                AgentProfile {
                    name: "First".into(),
                    kind: AgentProfileKind::Codex,
                    executable: "first-codex".into(),
                    ..AgentProfile::default()
                },
                AgentProfile {
                    name: "Chosen".into(),
                    kind: AgentProfileKind::Codex,
                    executable: "chosen-codex".into(),
                    env: vec![EnvVar {
                        name: "CODEX_HOME".into(),
                        value: "custom-home".into(),
                    }],
                    ..AgentProfile::default()
                },
            ],
            default_agent_profile: "Chosen".into(),
            ..AppSettings::default()
        };

        let launcher = codex_usage_launcher(&settings);

        assert_eq!(launcher.executable(), "chosen-codex");
        assert_eq!(
            launcher.effective_env_os("CODEX_HOME"),
            Some("custom-home".into())
        );

        settings.default_agent_profile = "Another provider".into();

        assert_eq!(codex_usage_launcher(&settings).executable(), "first-codex");

        settings.agent_profiles.clear();

        assert_eq!(codex_usage_launcher(&settings).executable(), "codex");
    }
}
