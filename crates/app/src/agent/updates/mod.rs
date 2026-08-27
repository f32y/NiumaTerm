//! Provider update registration, test isolation, and presentation reduction.

mod doubles;
mod notification;
mod transaction;

#[cfg(test)]
mod tests;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use std::{env, process};

use gpui::{App, Global};
use nmt_agent_utils::launcher::AgentCli;
use nmt_agent_utils::update::{
    ClaudeMaintenance, CodexMaintenance, HttpClaudeReleaseChannel, InstallationKey,
    InstallationSnapshot, ProviderKind, ProviderMaintenance, UpdateCoordinator,
};
use nmt_config::profile::AgentProfile;

use crate::agent::agent_launch;
use crate::agent::updates::doubles::{FakeMaintenance, UnavailableMaintenance};
pub(crate) use crate::agent::updates::notification::{
    FocusedVisibleLifetime, NotificationPrimaryAction, NotificationProgress,
    UpdateNotificationTone, UpdateNotificationView, notification_view,
};
#[cfg(test)]
use crate::agent::updates::transaction::{
    PreflightResolution, UpdateMode, affected_installation_indices, combine_transaction_error,
    resolve_preflight,
};
pub(crate) use crate::agent::updates::transaction::{provider_for_profile, request_update};
use crate::ui::AppSettings;

pub(crate) struct AgentUpdates {
    pub(crate) coordinator: UpdateCoordinator,
    testing: bool,
    claude: Arc<dyn ProviderMaintenance>,
    codex: Arc<dyn ProviderMaintenance>,
}

impl Global for AgentUpdates {}

impl AgentUpdates {
    /// `None` for a profile whose harness has no vendor-managed installation:
    /// it registers nothing, so it never reaches the status rows or the shared
    /// Check action.
    fn register_profile(&self, profile: &AgentProfile) -> Option<InstallationKey> {
        let provider = provider_for_profile(profile.kind)?;
        let launch = agent_launch(profile);
        let launcher = AgentCli::from_launch(&launch, provider.default_executable());
        let maintenance = match provider {
            ProviderKind::Claude => self.claude.clone(),
            ProviderKind::Codex => self.codex.clone(),
        };
        Some(self.coordinator.register(provider, launcher, maintenance))
    }

    pub(crate) fn testing(&self) -> bool {
        self.testing
    }
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
            |releases| Arc::new(ClaudeMaintenance::new(releases)),
        );
        (claude, Arc::new(CodexMaintenance))
    };

    let updates = AgentUpdates {
        coordinator: UpdateCoordinator::new(cache_path),
        testing,
        claude,
        codex,
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
    for profile in profiles {
        updates.register_profile(profile);
    }
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
            .filter_map(|profile| updates.register_profile(profile)),
    )
    .into_iter()
    .filter_map(|key| updates.coordinator.snapshot(&key))
    .collect()
}

pub(crate) fn installation(key: &InstallationKey, cx: &App) -> Option<InstallationSnapshot> {
    cx.try_global::<AgentUpdates>()?.coordinator.snapshot(key)
}

pub(crate) fn manual_check_profiles(profiles: &[AgentProfile], cx: &mut App) {
    let updates = cx.global::<AgentUpdates>();
    let keys = distinct_installation_keys(
        profiles
            .iter()
            .filter_map(|profile| updates.register_profile(profile)),
    );
    let coordinator = updates.coordinator.clone();
    cx.spawn(async move |cx| {
        let worker = cx.background_executor().spawn(async move {
            for key in keys {
                let _ = coordinator.check(&key, true);
            }
        });
        worker.await;
        cx.update(|cx| cx.refresh_windows());
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
    cx.spawn(async move |cx| {
        // Let the first windows finish opening before probing providers.
        cx.background_executor().timer(Duration::from_secs(3)).await;
        loop {
            // Re-read the switch every tick: the user can toggle it, and the
            // registered installations change, while the app runs.
            let active = cx.update(|cx| {
                let coordinator = cx.global::<AgentUpdates>().coordinator.clone();
                cx.global::<AppSettings>()
                    .check_agent_updates
                    .then_some(coordinator)
            });
            if let Some(coordinator) = active {
                let worker = cx.background_executor().spawn(async move {
                    for snapshot in coordinator.snapshots() {
                        let _ = coordinator.check(&snapshot.identity.key, false);
                    }
                });
                worker.await;
                cx.update(|cx| cx.refresh_windows());
            }
            cx.background_executor()
                .timer(AUTOMATIC_CHECK_INTERVAL)
                .await;
        }
    })
    .detach();
}
