use serde::{Deserialize, Serialize};

/// Provider-owned progress, independent of the transcript and its visible rows.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskList {
    pub explanation: Option<String>,
    pub items: Vec<Task>,
}

impl TaskList {
    pub fn tally(&self) -> Option<(u32, u32)> {
        if self.items.is_empty() {
            return None;
        }

        Some(self.items.iter().fold((0u32, 0u32), |(done, total), task| {
            (
                done.saturating_add(u32::from(task.status == TaskStatus::Completed)),
                total.saturating_add(1),
            )
        }))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub status: TaskStatus,
    pub owner: Option<String>,
    pub blocked_by: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskStatus {
    #[default]
    Pending,
    InProgress,
    Completed,
}

impl TaskStatus {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "in_progress" | "inProgress" => Some(Self::InProgress),
            "completed" => Some(Self::Completed),
            _ => None,
        }
    }
}

/// A goal can survive individual turns; counters retain their provider's units.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalStatus {
    pub objective: String,
    pub phase: String,
    pub reason: Option<String>,
    pub rounds_started: u64,
    pub max_rounds: u64,
    pub tokens_used: Option<u64>,
    pub token_budget: Option<u64>,
    pub elapsed_seconds: Option<u64>,
}
