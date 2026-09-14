use std::collections::VecDeque;
use std::mem::take;

use crate::chat::{SlashCommandOutcome, SlashCommandRunPolicy};
use crate::session::Backend;
use crate::session::lifecycle::Status;

#[derive(Clone)]
pub struct PendingSlashCommand {
    pub name: String,
    pub arguments: String,
}

impl PendingSlashCommand {
    /// One command a control runs directly, without the composer's parsing
    /// stage: the caller already knows the name and the value it is passing.
    pub fn new(name: &str, arguments: String) -> Self {
        Self {
            name: name.to_string(),
            arguments,
        }
    }
}

#[derive(Default)]
pub struct CommandQueue {
    pub queue: VecDeque<PendingSlashCommand>,
    pub awaiting_turn: bool,
}

pub enum CommandAdmission {
    Execute(PendingSlashCommand),
    Queued { name: String, count: usize },
    Busy { name: String },
}

impl CommandQueue {
    pub fn while_busy(
        &mut self,
        command: PendingSlashCommand,
        policy: SlashCommandRunPolicy,
    ) -> CommandAdmission {
        match policy {
            SlashCommandRunPolicy::Immediate => CommandAdmission::Execute(command),
            SlashCommandRunPolicy::IdleOnly => CommandAdmission::Busy { name: command.name },
            SlashCommandRunPolicy::QueueUntilIdle => {
                let name = command.name.clone();

                self.queue.push_back(command);

                CommandAdmission::Queued {
                    name,
                    count: self.queue.len(),
                }
            }
        }
    }

    pub fn execute(
        &mut self,
        backend: Option<&mut Backend>,
        command: &PendingSlashCommand,
    ) -> SlashCommandOutcome {
        let outcome = backend.map_or(SlashCommandOutcome::NotReady, |session| {
            session.execute_slash_command(&command.name, &command.arguments)
        });

        if matches!(outcome, SlashCommandOutcome::Accepted) {
            self.awaiting_turn = true;
        }

        outcome
    }

    pub fn clear(&mut self) -> bool {
        let discarded = !self.queue.is_empty();

        self.queue.clear();

        self.awaiting_turn = false;

        discarded
    }

    pub fn settle(&mut self, outcome: &SlashCommandOutcome, status: Status) -> bool {
        match outcome {
            SlashCommandOutcome::Accepted => false,
            SlashCommandOutcome::Completed { .. } => {
                if status == Status::Running {
                    return false;
                }

                take(&mut self.awaiting_turn)
            }
            SlashCommandOutcome::Rejected { .. } | SlashCommandOutcome::NotReady => {
                self.awaiting_turn = false;

                true
            }
        }
    }

    pub fn turn_started(&mut self) -> bool {
        take(&mut self.awaiting_turn)
    }

    pub(crate) fn turn_completed(&mut self) {
        self.awaiting_turn = false;
    }
}
