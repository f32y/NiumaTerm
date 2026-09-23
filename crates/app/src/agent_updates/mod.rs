//! Provider update registration, test isolation, and presentation reduction.

pub(crate) use crate::agent_updates::notification::{
    FocusedVisibleLifetime, NotificationPrimaryAction, UpdateNotificationView, notification_view,
};
pub(crate) use crate::agent_updates::transaction::{provider_for_profile, request_update};

mod doubles;
mod maintenance;
mod notification;
mod transaction;

#[cfg(test)]
mod tests;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use std::{env, process};

use app::agent_tab::agent_launch;
use gpui::{App, AsyncApp, Global};
use nmt_agent::launcher::AgentCli;
use nmt_agent::update::{
    ClaudeMaintenance, CodexMaintenance, HttpClaudeReleaseChannel, InstallationKey,
    InstallationSnapshot, ProviderKind, ProviderMaintenance, UpdateCoordinator,
};
use nmt_config::profile::AgentProfile;

use crate::agent_updates::doubles::{FakeMaintenance, UnavailableMaintenance};
#[cfg(test)]
use crate::agent_updates::maintenance::{
    PreflightFailure, PreflightResolution, UpdateMode, resolve_preflight,
};
#[cfg(test)]
use crate::agent_updates::transaction::{affected_installation_indices, combine_transaction_error};
use crate::ui::AppSettings;
use crate::utils::on_runtime;

pub(crate) struct AgentUpdates {
    pub(crate) coordinator: UpdateCoordinator,
    testing: bool,
    claude: Arc<dyn ProviderMaintenance>,
    codex: Arc<dyn ProviderMaintenance>,
    registrations: Vec<RegisteredLauncher>,
}

struct RegisteredLauncher {
    definition: LauncherDefinition,
    key: InstallationKey,
}

#[derive(PartialEq, Eq)]
struct LauncherDefinition {
    provider: ProviderKind,
    launcher: AgentCli,
}

impl Global for AgentUpdates {}

impl AgentUpdates {
    pub(crate) fn notify_changed(cx: &mut App) {
        // Worker threads mutate the shared coordinator outside GPUI. Mutable
        // global access publishes those changes to the registered views.
        cx.global_mut::<Self>();
    }

    /// `None` for a profile whose harness has no vendor-managed installation:
    /// it registers nothing, so it never reaches the status rows or the shared
    /// Check action.
    fn register_profile(&mut self, profile: &AgentProfile) -> Option<InstallationKey> {
        Some(self.register_launcher(profile_launcher(profile)?))
    }

    fn register_launcher(&mut self, definition: LauncherDefinition) -> InstallationKey {
        let maintenance = match definition.provider {
            ProviderKind::Claude => self.claude.clone(),
            ProviderKind::Codex => self.codex.clone(),
        };

        let key = self.coordinator.register(
            definition.provider,
            definition.launcher.clone(),
            maintenance,
        );

        if let Some(registered) = self
            .registrations
            .iter_mut()
            .find(|registered| registered.definition == definition)
        {
            registered.key = key.clone();
        } else {
            self.registrations.push(RegisteredLauncher {
                definition,
                key: key.clone(),
            });
        }

        key
    }

    fn registered_key(&self, profile: &AgentProfile) -> Option<InstallationKey> {
        let definition = profile_launcher(profile)?;

        self.registrations
            .iter()
            .find(|registered| registered.definition == definition)
            .map(|registered| registered.key.clone())
    }

    pub(crate) fn testing(&self) -> bool {
        self.testing
    }
}

fn profile_launcher(profile: &AgentProfile) -> Option<LauncherDefinition> {
    let provider = provider_for_profile(profile.kind)?;
    let launch = agent_launch(profile);

    Some(LauncherDefinition {
        provider,
        launcher: AgentCli::from_launch(&launch, provider.default_executable()),
    })
}

pub(crate) fn initialize(testing: bool, profiles: &[AgentProfile], cx: &mut App) {
    let cache_path = update_cache_path(testing);

    let (claude, codex): (Arc<dyn ProviderMaintenance>, Arc<dyn ProviderMaintenance>) = if testing {
        (
            Arc::new(FakeMaintenance::new(ProviderKind::Claude)),
            Arc::new(FakeMaintenance::new(ProviderKind::Codex)),
        )
    } else {
        let claude: Arc<dyn ProviderMaintenance> = HttpClaudeReleaseChannel::new().map_or_else(
            |error| {
                Arc::new(UnavailableMaintenance {
                    provider: ProviderKind::Claude,
                    reason: error.to_string(),
                }) as Arc<dyn ProviderMaintenance>
            },
            |releases| Arc::new(ClaudeMaintenance::new(Box::new(releases))),
        );

        (claude, Arc::new(CodexMaintenance))
    };

    let mut updates = AgentUpdates {
        coordinator: UpdateCoordinator::new(cache_path),
        testing,
        claude,
        codex,
        registrations: Vec::new(),
    };

    for profile in profiles {
        updates.register_profile(profile);
    }

    cx.set_global(updates);
}

fn update_cache_path(testing: bool) -> PathBuf {
    if testing {
        env::temp_dir()
            .join("NiumaTerm")
            .join(format!("update-testing-{}", process::id()))
            .join("agent-update-status.json")
    } else {
        nmt_config::config_dir_path().join("agent-update-status.json")
    }
}

pub(crate) fn reconcile_profiles(profiles: &[AgentProfile], cx: &mut App) {
    let Some(updates) = cx.try_global::<AgentUpdates>() else {
        return;
    };

    let launchers: Vec<_> = profiles.iter().filter_map(profile_launcher).collect();

    let unchanged = updates
        .registrations
        .iter()
        .all(|registered| launchers.contains(&registered.definition))
        && launchers.iter().all(|definition| {
            updates
                .registrations
                .iter()
                .any(|registered| registered.definition == *definition)
        });

    if unchanged {
        return;
    }

    let updates = cx.global_mut::<AgentUpdates>();

    updates
        .registrations
        .retain(|registered| launchers.contains(&registered.definition));

    for definition in launchers {
        if !updates
            .registrations
            .iter()
            .any(|registered| registered.definition == definition)
        {
            updates.register_launcher(definition);
        }
    }

    let kept: HashSet<InstallationKey> = updates
        .registrations
        .iter()
        .map(|registered| registered.key.clone())
        .collect();

    updates.coordinator.retain(|key| kept.contains(key));
}

fn distinct_installation_keys(
    keys: impl IntoIterator<Item = InstallationKey>,
) -> Vec<InstallationKey> {
    let mut seen = HashSet::new();

    keys.into_iter()
        .filter(|key| seen.insert(key.clone()))
        .collect()
}

pub(crate) fn installations_for_profiles(
    profiles: &[AgentProfile],
    cx: &App,
) -> Vec<InstallationSnapshot> {
    let Some(updates) = cx.try_global::<AgentUpdates>() else {
        return Vec::new();
    };

    distinct_installation_keys(
        profiles
            .iter()
            .filter_map(|profile| updates.registered_key(profile)),
    )
    .into_iter()
    .filter_map(|key| updates.coordinator.snapshot(&key))
    .collect()
}

pub(crate) fn installation(key: &InstallationKey, cx: &App) -> Option<InstallationSnapshot> {
    cx.try_global::<AgentUpdates>()?.coordinator.snapshot(key)
}

pub(crate) fn manual_check_profiles(profiles: &[AgentProfile], cx: &mut App) {
    let updates = cx.global_mut::<AgentUpdates>();

    let keys = distinct_installation_keys(
        profiles
            .iter()
            .filter_map(|profile| updates.register_profile(profile)),
    );

    let coordinator = updates.coordinator.clone();

    cx.spawn(async move |cx| {
        on_runtime(async move {
            for key in keys {
                let _ = coordinator.check(&key, true).await;
            }
        })
        .await;

        cx.update(AgentUpdates::notify_changed);
    })
    .detach();
}

/// Automatic re-check cadence. Matches the coordinator's cache freshness, so a
/// tick that finds a fresh cached result costs nothing.
const AUTOMATIC_CHECK_INTERVAL: Duration = Duration::from_secs(60 * 60);

pub(crate) fn schedule_automatic_checks(cx: &mut App) {
    if cx.global::<AgentUpdates>().testing() {
        return;
    }

    cx.spawn(run_automatic_checks).detach();
}

async fn run_automatic_checks(cx: &mut AsyncApp) {
    // Let the first windows finish opening before probing providers.
    cx.background_executor().timer(Duration::from_secs(3)).await;

    loop {
        // Re-read the switch every tick: the user can toggle it, and the
        // registered installations change, while the app runs.
        let active = cx.update(|cx| {
            let coordinator = cx.global::<AgentUpdates>().coordinator.clone();

            cx.global::<AppSettings>()
                .config()
                .agent
                .check_agent_updates
                .then_some(coordinator)
        });

        if let Some(coordinator) = active {
            on_runtime(async move {
                for snapshot in coordinator.snapshots() {
                    let _ = coordinator.check(&snapshot.identity.key, false).await;
                }
            })
            .await;

            cx.update(AgentUpdates::notify_changed);
        }

        cx.background_executor()
            .timer(AUTOMATIC_CHECK_INTERVAL)
            .await;
    }
}
