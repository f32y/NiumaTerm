use std::sync::atomic::{AtomicBool, Ordering};

use futures::FutureExt as _;
use futures::future::{BoxFuture, ready};
use nmt_agent::launcher::AgentCli;
use nmt_agent::update::{
    DiscoverySupport, ProviderKind, ProviderMaintenance, UpdateError, UpdateErrorKind,
    VersionStatus,
};
use semver::Version;

pub(super) struct UnavailableMaintenance {
    pub(super) provider: ProviderKind,
    pub(super) reason: String,
}

impl ProviderMaintenance for UnavailableMaintenance {
    fn provider(&self) -> ProviderKind {
        self.provider
    }

    fn probe<'a>(&'a self, _: &'a AgentCli) -> BoxFuture<'a, Result<VersionStatus, UpdateError>> {
        ready(Err(UpdateError::new(
            UpdateErrorKind::Unsupported,
            &self.reason,
        )))
        .boxed()
    }

    fn update<'a>(&'a self, _: &'a AgentCli) -> BoxFuture<'a, Result<String, UpdateError>> {
        ready(Err(UpdateError::new(
            UpdateErrorKind::Unsupported,
            &self.reason,
        )))
        .boxed()
    }
}

/// `--testing` exposes a complete fake workflow without touching provider
/// executables, release endpoints, or the production cache.
pub(super) struct FakeMaintenance {
    provider: ProviderKind,
    updated: AtomicBool,
}

impl FakeMaintenance {
    pub(super) fn new(provider: ProviderKind) -> Self {
        Self {
            provider,
            updated: AtomicBool::new(false),
        }
    }
}

impl ProviderMaintenance for FakeMaintenance {
    fn provider(&self) -> ProviderKind {
        self.provider
    }

    fn probe<'a>(&'a self, _: &'a AgentCli) -> BoxFuture<'a, Result<VersionStatus, UpdateError>> {
        let current = if self.updated.load(Ordering::SeqCst) {
            Version::new(1, 1, 0)
        } else {
            Version::new(1, 0, 0)
        };

        ready(Ok(VersionStatus {
            provider: self.provider,
            current: Some(current),
            available: Some(Version::new(1, 1, 0)),
            install_method: Some("testing fixture".to_string()),
            channel: Some("testing".to_string()),
            can_update: true,
            support: DiscoverySupport::Supported,
            remediation: None,
        }))
        .boxed()
    }

    fn update<'a>(&'a self, _: &'a AgentCli) -> BoxFuture<'a, Result<String, UpdateError>> {
        self.updated.store(true, Ordering::SeqCst);

        ready(Ok("testing provider updated".to_string())).boxed()
    }
}
