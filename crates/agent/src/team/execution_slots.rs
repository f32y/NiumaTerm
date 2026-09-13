use std::collections::BTreeMap;

use crate::team::identity::{AttemptId, MemberId, OwnershipGeneration};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExecutionKey {
    pub member: MemberId,
    pub ownership: OwnershipGeneration,
    pub attempt: AttemptId,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WorkStatus {
    pub foreground: bool,
    pub background: usize,
    pub interaction: bool,
    pub uncertain: bool,
}

struct Slot {
    key: ExecutionKey,
    status: WorkStatus,
}

#[derive(Default)]
pub struct ExecutionSlots {
    members: BTreeMap<MemberId, Slot>,
}

impl ExecutionSlots {
    pub(crate) fn is_idle(&self) -> bool {
        self.members.is_empty()
    }

    pub fn reserve(&mut self, key: ExecutionKey) -> bool {
        if self.members.contains_key(&key.member) {
            return false;
        }

        self.members.insert(
            key.member,
            Slot {
                key,
                status: WorkStatus {
                    foreground: true,
                    ..WorkStatus::default()
                },
            },
        );

        true
    }

    /// Completion releases ownership only after every source of continued work
    /// has settled. An interaction or an unknown shutdown keeps the slot held.
    pub fn update(&mut self, key: ExecutionKey, status: WorkStatus) -> bool {
        let Some(slot) = self.members.get_mut(&key.member) else {
            return false;
        };

        if slot.key != key {
            return false;
        }

        slot.status = status;

        if !status.foreground && status.background == 0 && !status.interaction && !status.uncertain
        {
            self.members.remove(&key.member);
        }

        true
    }
}
