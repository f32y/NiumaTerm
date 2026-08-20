use std::sync::atomic::{AtomicBool, Ordering};

use nmt_agent_utils::launcher::AgentCli;
use nmt_agent_utils::update::{
    DiscoverySupport, ProviderKind, ProviderMaintenance, UpdateError, UpdateErrorKind,
    VendorUpdateResult, VersionStatus,
};
use semver::Version;

pub(in crate::agent::updates) struct UnavailableMaintenance {
    pub(in crate::agent::updates) provider: ProviderKind,
    pub(in crate::agent::updates) reason: String,
}

impl ProviderMaintenance for UnavailableMaintenance {
    fn provider(&self) -> ProviderKind {
        self.provider
    }

    fn probe(&self, _: &AgentCli) -> Result<VersionStatus, UpdateError> {
        Err(UpdateError::new(UpdateErrorKind::Unsupported, &self.reason))
    }

    fn update(&self, _: &AgentCli) -> Result<VendorUpdateResult, UpdateError> {
        Err(UpdateError::new(UpdateErrorKind::Unsupported, &self.reason))
    }
}

/// `--testing` exposes a complete fake workflow without touching provider
/// executables, release endpoints, or the production cache.
pub(in crate::agent::updates) struct FakeMaintenance {
    provider: ProviderKind,
    updated: AtomicBool,
}

impl FakeMaintenance {
    pub(in crate::agent::updates) fn new(provider: ProviderKind) -> Self {
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

    fn probe(&self, _: &AgentCli) -> Result<VersionStatus, UpdateError> {
        let current = if self.updated.load(Ordering::SeqCst) {
            Version::new(1, 1, 0)
        } else {
            Version::new(1, 0, 0)
        };
        Ok(VersionStatus {
            provider: self.provider,
            current: Some(current),
            available: Some(Version::new(1, 1, 0)),
            install_method: Some("testing fixture".to_string()),
            channel: Some("testing".to_string()),
            can_update: true,
            support: DiscoverySupport::Supported,
            remediation: None,
        })
    }

    fn update(&self, _: &AgentCli) -> Result<VendorUpdateResult, UpdateError> {
        self.updated.store(true, Ordering::SeqCst);
        Ok(VendorUpdateResult {
            diagnostic: "testing provider updated".to_string(),
        })
    }
}
