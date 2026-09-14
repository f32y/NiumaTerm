use std::collections::{BTreeMap, HashMap};

use gpui::{Context, Entity};
use nmt_agent::chat::Item;
use nmt_agent::team::attempt::{Attempt, AttemptState, BudgetScope};
use nmt_agent::team::budget::TurnPurpose;
use nmt_agent::team::content::{Author, Publication};
use nmt_agent::team::identity::{AttemptId, OperationId};
use nmt_agent::team::room::Room;
use nmt_agent::transcript::TranscriptEntry;
use nmt_agent::transcript::conversation::EntryMetadata;
use rust_i18n::t;

use crate::agent_tab::team::TeamRuntime;
use crate::agent_tab::team::view::TeamPane;
use crate::agent_tab::transcript::{TranscriptAttribution, TranscriptView};

#[derive(PartialEq)]
pub(super) struct TimelineRow {
    id: String,
    pub(super) heading: String,
    pub(super) text: String,
    user: bool,
    cwd: Option<String>,
}

#[derive(Default)]
pub(super) struct TimelineMirror {
    pub(super) rows: Vec<TimelineRow>,
    revision: Option<u64>,
}

impl TimelineMirror {
    pub(super) fn sync_timeline(
        &mut self,
        runtime: &Entity<TeamRuntime>,
        transcript: &Entity<TranscriptView>,
        cx: &mut Context<TeamPane>,
    ) {
        let runtime = runtime.read(cx);

        let mut live = BTreeMap::new();

        for host in runtime.hosts.values() {
            let Some(attempt) = host.active else { continue };

            let state = host.owner.session().read(cx).controller.borrow();

            if !runtime.room().attempts().iter().any(|entry| {
                entry.id == attempt
                    && entry.intent.backend_generation == state.runtime.epoch()
                    && matches!(
                        entry.state,
                        AttemptState::Sending | AttemptState::Accepted { .. }
                    )
            }) {
                continue;
            }

            let conversation = state.conversation.borrow();

            if let Some(text) = conversation
                .content
                .latest_agent_message(state.delivery.turn())
            {
                live.insert(attempt, text.to_owned());
            }
        }

        let revision = runtime.session.store().revision();

        if self.revision == Some(revision) {
            for (id, text) in live {
                let id = id.to_string();

                let Some(row) = self.rows.iter_mut().find(|row| row.id == id) else {
                    continue;
                };

                if row.text == text {
                    continue;
                }

                row.text.clone_from(&text);

                let item = Item::AgentMessage {
                    id: row.id.clone(),
                    text: Some(text),
                    questions: None,
                };

                transcript.update(cx, |transcript, cx| {
                    transcript.conversation.borrow_mut().merge_completed(&item);

                    transcript.sync_content();

                    cx.notify();
                });
            }

            return;
        }

        self.revision = Some(revision);

        let rows = public_rows(runtime.room(), &live);

        let first = self
            .rows
            .iter()
            .zip(&rows)
            .take_while(|(old, next)| old == next)
            .count();

        if first == self.rows.len() && first == rows.len() {
            return;
        }

        let mut attribution = HashMap::new();

        let entries = rows
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let item = if row.user {
                    Item::UserMessage {
                        text: Some(row.text.clone()),
                    }
                } else {
                    attribution.insert(
                        row.id.clone(),
                        TranscriptAttribution {
                            name: row.heading.clone().into(),
                            cwd: row.cwd.clone(),
                        },
                    );

                    Item::AgentMessage {
                        id: row.id.clone(),
                        text: Some(row.text.clone()),
                        questions: None,
                    }
                };

                TranscriptEntry {
                    turn: index as u64 + 1,
                    item,
                    metadata: EntryMetadata::default(),
                }
            })
            .collect();

        transcript.update(cx, |transcript, cx| {
            transcript.show_attributed_entries(entries, attribution, first, cx)
        });

        self.rows = rows;
    }
}

fn public_rows(room: &Room, live: &BTreeMap<AttemptId, String>) -> Vec<TimelineRow> {
    let mut groups = Vec::new();

    for (index, message) in room.messages().iter().enumerate() {
        if !matches!(
            message.publication,
            Publication::UserInput | Publication::ExplicitShare
        ) {
            continue;
        }

        let (heading, cwd, user) = match &message.author {
            Author::User => (String::new(), None, true),
            Author::Member { id, name } => (
                name.clone(),
                room.member(*id)
                    .and_then(|member| member.roots().primary())
                    .map(str::to_owned),
                false,
            ),
        };

        groups.push((
            index * 2,
            vec![TimelineRow {
                id: message.id.to_string(),
                heading,
                text: message.text.clone(),
                user,
                cwd,
            }],
        ));
    }

    for discussion in room.discussions() {
        for stage in discussion.stages() {
            let boundary = stage
                .segments
                .first()
                .map_or(0, |snapshot| snapshot.messages.len());

            let mut rows = Vec::new();

            for attempt in room.attempts().iter().filter(|attempt| {
                attempt.intent.stage == Some(stage.id)
                    && attempt.intent.purpose == TurnPurpose::Summary
            }) {
                rows.push(attempt_row(room, attempt, live));
            }

            for arrangement in &stage.arrangements {
                let attempt = room
                    .attempts()
                    .iter()
                    .rev()
                    .find(|attempt| attempt.intent.operation == arrangement.operation);

                let member = room.member(arrangement.recipient);

                rows.push(attempt.map_or_else(
                    || {
                        TimelineRow {
                            id: arrangement.operation.to_string(),
                            heading: member.map_or("", |member| member.name()).to_owned(),
                            text: t!("team-waiting").into_owned(),
                            user: false,
                            cwd: member
                                .and_then(|member| member.roots().primary())
                                .map(str::to_owned),
                        }
                    },
                    |attempt| attempt_row(room, attempt, live),
                ));
            }

            groups.push((boundary.saturating_mul(2).saturating_sub(1), rows));
        }
    }

    let mut direct: BTreeMap<OperationId, Vec<&Attempt>> = BTreeMap::new();

    for attempt in room.attempts() {
        if let BudgetScope::Direct(operation) = attempt.intent.budget {
            direct.entry(operation).or_default().push(attempt);
        }
    }

    for attempts in direct.into_values() {
        let boundary = attempts[0].intent.snapshot.messages.len();

        groups.push((
            boundary * 2 + 1,
            attempts
                .iter()
                .map(|attempt| attempt_row(room, attempt, live))
                .collect(),
        ));
    }

    groups.sort_by_key(|(position, _)| *position);

    groups.into_iter().flat_map(|(_, rows)| rows).collect()
}

fn attempt_row(room: &Room, attempt: &Attempt, live: &BTreeMap<AttemptId, String>) -> TimelineRow {
    let member = room.member(attempt.intent.recipient);

    let text = match attempt.state {
        AttemptState::Completed { message } => room
            .messages()
            .iter()
            .find(|entry| entry.id == message)
            .map(|message| message.text.clone())
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| t!("team-no-text").into_owned()),
        AttemptState::Summarized { summary } => room
            .summaries()
            .iter()
            .find(|entry| entry.id == summary)
            .map(|summary| {
                let positions = summary
                    .disagreements
                    .iter()
                    .map(|position| {
                        let name = room
                            .member(position.member)
                            .map_or("", |member| member.name());

                        format!("{name}: {}\n{}", position.position, position.reasons)
                    })
                    .collect::<Vec<_>>()
                    .join("\n\n");

                format!(
                    "{}\n\n{}\n\n{}\n\n{positions}",
                    summary.goals, summary.constraints, summary.agreements
                )
            })
            .unwrap_or_default(),
        AttemptState::Reserved => t!("team-waiting").into_owned(),
        AttemptState::Sending | AttemptState::Accepted { .. } => live
            .get(&attempt.id)
            .cloned()
            .unwrap_or_else(|| t!("team-responding").into_owned()),
        AttemptState::Rejected => t!("team-not-sent").into_owned(),
        AttemptState::Failed => t!("team-response-failed").into_owned(),
        AttemptState::Uncertain => t!("team-response-uncertain").into_owned(),
        AttemptState::Abandoned => t!("team-response-abandoned").into_owned(),
    };

    TimelineRow {
        id: attempt.id.to_string(),
        heading: member.map_or("", |member| member.name()).to_owned(),
        text,
        user: false,
        cwd: member
            .and_then(|member| member.roots().primary())
            .map(str::to_owned),
    }
}
