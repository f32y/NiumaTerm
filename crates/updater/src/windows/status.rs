use crate::windows::{CheckError, InstallError, Release};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Status {
    /// Nothing has been asked yet, which is what a build with checking turned
    /// off reports for as long as it stays off.
    #[default]
    Unknown,
    Checking,
    /// The channel has published nothing this build can be compared against,
    /// which is not the same as being current: an empty channel says nothing
    /// about what is running.
    NothingPublished,
    UpToDate,
    Available(Release),
    /// The package is being fetched and unpacked, or its captured plan is being
    /// applied without needing a user decision.
    Installing(Release),
    InspectingFileUse(Release),
    AwaitingFileUse(Release),
    ClosingFileUsers(Release),
    RecoveryWarning {
        release: Release,
        applications: Vec<String>,
    },
    Failed(CheckError),
    InstallFailed(InstallError),
}

impl Status {
    pub fn busy(&self) -> bool {
        matches!(self, Self::Checking) || self.installation_in_progress()
    }

    pub(super) fn installation_in_progress(&self) -> bool {
        matches!(
            self,
            Self::Installing(_)
                | Self::InspectingFileUse(_)
                | Self::AwaitingFileUse(_)
                | Self::ClosingFileUsers(_)
                | Self::RecoveryWarning { .. }
        )
    }

    pub fn release(&self) -> Option<&Release> {
        match self {
            Self::Available(release)
            | Self::Installing(release)
            | Self::InspectingFileUse(release)
            | Self::AwaitingFileUse(release)
            | Self::ClosingFileUsers(release) => Some(release),
            Self::RecoveryWarning { release, .. } => Some(release),
            Self::Unknown
            | Self::Checking
            | Self::NothingPublished
            | Self::UpToDate
            | Self::Failed(_)
            | Self::InstallFailed(_) => None,
        }
    }
}
