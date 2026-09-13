use std::time::{Duration, Instant};

use chrono::Utc;

use crate::chat::{Item, ReplayTurn};
use crate::session::controller::{SessionController, SessionEffect};
use crate::transcript::TextField;
use crate::transcript::conversation::hidden;

impl SessionController {
    pub fn push_item(&mut self, item: Item) {
        let images = if let Item::UserMessage { text: Some(text) } = &item {
            self.pending_images
                .iter()
                .position(|(pending, _)| pending == text)
                .and_then(|index| self.pending_images.remove(index))
                .map(|(_, images)| images)
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        self.conversation
            .borrow_mut()
            .push(self.delivery.turn(), item, images);
    }

    pub fn note_visible_output(&mut self) {
        self.delivery.visible_output();
        self.conversation.borrow_mut().visible_output();
    }

    pub fn publish_confirmed(&mut self) {
        while let Some(text) = self.delivery.pop_confirmed() {
            self.push_item(Item::UserMessage { text: Some(text) });
        }
    }

    pub fn start_item(&mut self, item: Item) {
        if let Item::UserMessage { text } = item {
            if let Some(text) = text.and_then(|text| self.delivery.echoed(&text)) {
                self.push_item(Item::UserMessage { text: Some(text) });
            }

            return;
        }

        if !hidden(&item) {
            self.note_visible_output();
        }

        if matches!(item, Item::AgentMessage { .. }) {
            self.delivery.agent_message();
            self.publish_confirmed();
        }

        self.push_item(item);
    }

    pub fn complete_item(&mut self, item: Item) {
        let Some(id) = item.id() else {
            return;
        };

        if !self.conversation.borrow().content.contains_item(id) {
            self.start_item(item);

            return;
        }

        if !hidden(&item) {
            self.note_visible_output();
        }

        self.conversation.borrow_mut().merge_completed(&item);
    }

    pub fn append_delta(&mut self, item_id: &str, delta: &str, field: TextField) {
        let visible = self
            .conversation
            .borrow_mut()
            .append_delta(item_id, delta, field)
            .is_some_and(|update| update.non_blank);

        if visible {
            self.note_visible_output();
        }
    }

    pub fn apply_replay(&mut self, replay: Vec<ReplayTurn>) {
        let answered_at = replay
            .iter()
            .flat_map(|turn| turn.items.iter())
            .filter_map(|entry| entry.at)
            .max();

        if let Some(at) = answered_at {
            let age = Duration::from_secs(
                u64::try_from(Utc::now().timestamp().saturating_sub(at)).unwrap_or(0),
            )
            .min(Duration::from_secs(3600));

            let now = Instant::now();

            self.conversation.borrow_mut().last_response_at =
                Some(now.checked_sub(age).unwrap_or(now));
        }

        for turn in replay {
            let id = self.delivery.replay_turn();

            self.conversation.borrow_mut().replay(id, turn);
        }
    }

    /// Publish content and delivery changes before returning control to readers.
    pub(super) fn record_content(&mut self, effect: SessionEffect) -> SessionEffect {
        match effect {
            SessionEffect::ItemStarted(item) => {
                self.start_item(item);

                SessionEffect::Changed
            }

            SessionEffect::ItemCompleted(item) => {
                self.complete_item(item);

                SessionEffect::Changed
            }

            SessionEffect::TextDelta {
                item_id,
                delta,
                field,
            } => {
                self.append_delta(&item_id, &delta, field);

                SessionEffect::Changed
            }

            SessionEffect::ConfirmedPrompts(prompts) => {
                for text in prompts {
                    self.push_item(Item::UserMessage { text: Some(text) });
                }

                SessionEffect::Changed
            }

            SessionEffect::OutputTokens(tokens) => {
                let mut conversation = self.conversation.borrow_mut();

                if conversation.live.set_output_tokens(tokens) {
                    conversation.changed_turn(self.delivery.turn());
                }

                SessionEffect::Changed
            }

            SessionEffect::ContextWindow(usage) => {
                self.conversation.borrow_mut().context_window_usage = Some(usage);

                SessionEffect::Changed
            }

            SessionEffect::ContextComposition(composition) => {
                self.conversation.borrow_mut().context_composition = Some(composition);

                SessionEffect::Changed
            }

            SessionEffect::Stats(stats) => {
                self.conversation.borrow_mut().session_stats = Some(stats);

                SessionEffect::Changed
            }

            SessionEffect::Error {
                message,
                fatal,
                failure,
            } => {
                self.note_visible_output();

                if fatal {
                    self.publish_confirmed();
                    self.conversation.borrow_mut().settle(self.delivery.turn());
                }

                self.push_item(Item::Error {
                    text: message.clone(),
                });

                SessionEffect::Error {
                    message,
                    fatal,
                    failure,
                }
            }

            SessionEffect::InputResolved(mut completion) => {
                if let Some(text) = completion.message.take() {
                    if completion.started_turn {
                        self.conversation.borrow_mut().start();
                    }

                    self.push_item(Item::UserMessage { text: Some(text) });
                }

                SessionEffect::InputResolved(completion)
            }

            SessionEffect::TurnCompleted { error, interrupted } => {
                if let Some(text) = &error {
                    let conversation = self.conversation.borrow();

                    let show = !conversation.turns.was_interrupted(self.delivery.turn())
                        && !conversation
                            .content
                            .turn_has_error(self.delivery.turn(), text);

                    drop(conversation);

                    if show {
                        self.push_item(Item::Error { text: text.clone() });
                    }
                }

                SessionEffect::TurnCompleted { error, interrupted }
            }

            SessionEffect::CompactionStarted => {
                self.note_visible_output();

                let mut conversation = self.conversation.borrow_mut();

                conversation.live.set_compacting(true);
                conversation.changed_turn(self.delivery.turn());

                SessionEffect::Changed
            }

            SessionEffect::CompactionFinished { error } => {
                if let Some(text) = error {
                    self.push_item(Item::Error { text });
                }

                let mut conversation = self.conversation.borrow_mut();

                conversation.live.set_compacting(false);
                conversation.changed_turn(self.delivery.turn());

                SessionEffect::Changed
            }

            effect @ (SessionEffect::ApprovalRequested | SessionEffect::InputRequested { .. }) => {
                self.note_visible_output();

                effect
            }

            effect => effect,
        }
    }
}
