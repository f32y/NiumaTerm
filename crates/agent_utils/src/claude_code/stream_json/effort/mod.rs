use std::collections::VecDeque;

use crate::chat::Event;

struct Change {
    id: String,
    value: String,
    result: Option<ChangeResult>,
}

enum ChangeResult {
    Applied,
    Rejected(String),
    Unknown,
}

/// Responses may arrive in a different order from writes. Confirm changes in
/// submission order so an older rejection cannot undo a later accepted level.
#[derive(Default)]
pub(super) struct EffortState {
    confirmed: Option<String>,
    unconfirmed: Option<String>,
    pending: VecDeque<Change>,
}

impl EffortState {
    pub(super) fn new(confirmed: Option<String>) -> Self {
        Self {
            confirmed,
            unconfirmed: None,
            pending: VecDeque::new(),
        }
    }

    pub(super) fn desired(&self) -> Option<&str> {
        self.pending
            .back()
            .map(|change| change.value.as_str())
            .or(self.unconfirmed.as_deref())
            .or(self.confirmed.as_deref())
    }

    pub(super) fn record(&mut self, id: String, value: String) {
        self.pending.push_back(Change {
            id,
            value,
            result: None,
        });
    }

    pub(super) fn contains(&self, id: &str) -> bool {
        self.pending.iter().any(|change| change.id == id)
    }

    pub(super) fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    pub(super) fn resolve(&mut self, id: &str, error: Option<String>) -> Option<Event> {
        let change = self.pending.iter_mut().find(|change| change.id == id)?;
        if change.result.is_some() {
            return None;
        }
        change.result = Some(error.map_or(ChangeResult::Applied, ChangeResult::Rejected));
        self.settle()
    }

    pub(super) fn expire(&mut self, id: &str) -> Option<Event> {
        let change = self.pending.iter_mut().find(|change| change.id == id)?;
        if change.result.is_some() {
            return None;
        }
        // A missing acknowledgment is not a refusal. Retain the requested
        // level as uncertain so sending another prompt does not resend it.
        change.result = Some(ChangeResult::Unknown);
        self.settle()
    }

    pub(super) fn close(&mut self, message: &str) -> Option<Event> {
        for change in &mut self.pending {
            if change.result.is_none() {
                change.result = Some(ChangeResult::Rejected(message.to_string()));
            }
        }
        self.settle()
    }

    fn settle(&mut self) -> Option<Event> {
        let mut errors = Vec::new();
        while self
            .pending
            .front()
            .is_some_and(|change| change.result.is_some())
        {
            let Some(change) = self.pending.pop_front() else {
                break;
            };
            match change.result {
                Some(ChangeResult::Applied) => {
                    self.confirmed = Some(change.value);
                    self.unconfirmed = None;
                }
                Some(ChangeResult::Rejected(error)) => errors.push(error),
                Some(ChangeResult::Unknown) => self.unconfirmed = Some(change.value),
                None => unreachable!("only completed changes are removed"),
            }
        }
        (!errors.is_empty()).then(|| Event::EffortRejected {
            message: errors.join("; "),
            effort: self.desired().map(str::to_owned),
        })
    }

    #[cfg(test)]
    pub(super) fn pending_count(&self) -> usize {
        self.pending.len()
    }
}

#[cfg(test)]
mod tests;
