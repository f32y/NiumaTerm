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

use crate::agent_pane::agent_launch;
use crate::agent_pane::updates::doubles::{FakeMaintenance, UnavailableMaintenance};
pub(crate) use crate::agent_pane::updates::notification::{
    FocusedVisibleLifetime, NotificationPrimaryAction, NotificationProgress,
    UpdateNotificationTone, UpdateNotificationView, notification_view,
};
#[cfg(test)]
use crate::agent_pane::updates::transaction::{
    PreflightResolution, UpdateMode, affected_installation_indices, combine_transaction_error,
    resolve_preflight,
};
pub(crate) use crate::agent_pane::updates::transaction::{provider_for_profile, request_update};

pub(crate) struct AgentUpdates {
    pub(crate) coordinator: UpdateCoordinator,
    testing: bool,
    claude: Arc<dyn ProviderMaintenance>,
    codex: Arc<dyn ProviderMaintenance>,
}

impl Global for AgentUpdates {}

impl AgentUpdates {
    fn register_profile(&self, profile: &AgentProfile) -> InstallationKey {
        let provider = provider_for_profile(profile.kind);
        let launch = agent_launch(profile);
        let launcher = AgentCli::from_launch(&launch, provider.default_executable());
        let maintenance = match provider {
            ProviderKind::Claude => self.claude.clone(),
            ProviderKind::Codex => self.codex.clone(),
        };
        self.coordinator.register(provider, launcher, maintenance)
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
            .map(|profile| updates.register_profile(profile)),
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
            .map(|profile| updates.register_profile(profile)),
    );
    let coordinator = updates.coordinator.clone();
    cx.spawn(async move |cx| {
        let worker = cx.background_executor().spawn(async move {
            for key in keys {
                let _ = coordinator.check(&key, true);
            }
        });
        worker.await;
        let _ = cx.update(|cx| cx.refresh_windows());
    })
    .detach();
}

pub(crate) fn schedule_startup_checks(cx: &mut App) {
    let updates = cx.global::<AgentUpdates>();
    if updates.testing() {
        return;
    }
    let coordinator = updates.coordinator.clone();
    let keys = coordinator
        .snapshots()
        .into_iter()
        .map(|snapshot| snapshot.identity.key)
        .collect::<Vec<_>>();
    cx.spawn(async move |cx| {
        cx.background_executor().timer(Duration::from_secs(3)).await;
        let worker = cx.background_executor().spawn(async move {
            for key in keys {
                let _ = coordinator.check(&key, false);
            }
        });
        worker.await;
        let _ = cx.update(|cx| cx.refresh_windows());
    })
    .detach();
}
