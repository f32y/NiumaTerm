use std::fmt::{self, Display, Formatter};
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::{Error as UuidError, Uuid};

macro_rules! identities {
    ($($name:ident),+ $(,)?) => {
        $(
            #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
            #[serde(transparent)]
            pub struct $name(Uuid);

            impl $name {
                pub fn new() -> Self {
                    Self(Uuid::new_v4())
                }
            }

            impl Default for $name {
                fn default() -> Self {
                    Self::new()
                }
            }

            impl FromStr for $name {
                type Err = UuidError;
                fn from_str(value: &str) -> Result<Self, Self::Err> {
                    Uuid::parse_str(value).map(Self)
                }
            }

            impl Display for $name {
                fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
                    self.0.fmt(formatter)
                }
            }
        )+
    };
}

identities!(
    RoomId,
    MemberId,
    ConversationId,
    DiscussionId,
    StageId,
    OperationId,
    SummaryId,
    AttemptId,
    MessageId,
    InteractionId,
    AttachmentId,
);

/// A transfer advances ownership independently of provider restarts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OwnershipGeneration(u64);

impl Default for OwnershipGeneration {
    fn default() -> Self {
        Self(1)
    }
}

impl OwnershipGeneration {
    pub fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}
