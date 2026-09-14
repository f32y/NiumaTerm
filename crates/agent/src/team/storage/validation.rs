use std::collections::BTreeSet;

use crate::team::attempt::{AttemptState, BudgetScope};
use crate::team::budget::ReservationState;
use crate::team::content::Author;
use crate::team::discussion::DiscussionState;
use crate::team::room::Room;
use crate::team::storage::StorageError;

pub(super) fn validate(room: &Room) -> Result<(), StorageError> {
    let invalid = StorageError::Invalid;

    let mut members = BTreeSet::new();
    let mut names = BTreeSet::new();

    for member in &room.members {
        if !members.insert(member.id) {
            return Err(invalid("duplicate member or conversation identity"));
        }

        if member.name.trim().is_empty()
            || member.name.chars().any(char::is_control)
            || member.name.trim() != member.name
            || !names.insert(member.name.to_lowercase())
        {
            return Err(invalid("invalid or duplicate member name"));
        }

        if member.roots.primary().is_none() && !member.roots.additional().is_empty() {
            return Err(invalid("member roots have no primary directory"));
        }
    }

    let mut messages = BTreeSet::new();

    for message in &room.messages {
        if message.replies_to.iter().any(|id| !messages.contains(id)) {
            return Err(invalid("reply source is missing"));
        }

        if !messages.insert(message.id) {
            return Err(invalid("duplicate public message"));
        }

        if let Author::Member { id, .. } = message.author
            && !members.contains(&id)
        {
            return Err(invalid("public author is missing"));
        }
    }

    let mut summaries = BTreeSet::new();

    for summary in &room.summaries {
        if summary.version == 0
            || !members.contains(&summary.owner)
            || summary.sources.iter().any(|id| !messages.contains(id))
            || summary.fragments.iter().any(|part| {
                !summary.sources.contains(&part.source)
                    || room
                        .messages
                        .iter()
                        .find(|message| message.id == part.source)
                        .is_none_or(|message| {
                            part.start > part.end
                                || part.end > message.text.len()
                                || !message.text.is_char_boundary(part.start)
                                || !message.text.is_char_boundary(part.end)
                        })
            })
            || summary
                .prior_summaries
                .iter()
                .any(|id| !summaries.contains(id))
            || summary.disagreements.iter().any(|position| {
                !members.contains(&position.member)
                    || position
                        .sources
                        .iter()
                        .any(|id| !summary.sources.contains(id))
            })
        {
            return Err(invalid("summary owner or source is missing"));
        }

        if !summaries.insert(summary.id) {
            return Err(invalid("duplicate summary identity"));
        }
    }

    let mut discussions = BTreeSet::new();
    let mut stages = BTreeSet::new();
    let mut operations = BTreeSet::new();
    let mut active = 0;

    for discussion in &room.discussions {
        if !discussions.insert(discussion.id) {
            return Err(invalid("duplicate discussion identity"));
        }

        active += usize::from(discussion.state != DiscussionState::Completed);

        let participants: BTreeSet<_> = discussion.participants.iter().copied().collect();

        if participants.is_empty()
            || participants.len() != discussion.participants.len()
            || !participants.is_subset(&members)
            || !members.contains(&discussion.mode.report_author())
        {
            return Err(invalid("invalid discussion participants"));
        }

        if !discussion.budget.validate() {
            return Err(invalid("budget exceeds scheduled turn limit"));
        }

        for stage in &discussion.stages {
            if !stages.insert(stage.id) {
                return Err(invalid("duplicate stage identity"));
            }

            let mut recipients = BTreeSet::new();

            for arrangement in &stage.arrangements {
                if !members.contains(&arrangement.recipient)
                    || !recipients.insert(arrangement.recipient)
                    || !operations.insert(arrangement.operation)
                {
                    return Err(invalid("invalid stage arrangement"));
                }
            }

            for snapshot in &stage.segments {
                if snapshot.messages.iter().any(|id| !messages.contains(id))
                    || snapshot.summaries.iter().any(|id| !summaries.contains(id))
                {
                    return Err(invalid("stage context source is missing"));
                }
            }
        }
    }

    if active > 1 {
        return Err(invalid("multiple active discussions"));
    }

    for budget in room.direct_allowances.values() {
        if !budget.validate() {
            return Err(invalid("direct request exceeds scheduled turn limit"));
        }
    }

    let mut attempts = BTreeSet::new();
    let mut provider_turns = BTreeSet::new();

    for attempt in &room.attempts {
        if !attempts.insert(attempt.id) || !members.contains(&attempt.intent.recipient) {
            return Err(invalid("duplicate attempt or missing recipient"));
        }

        if attempt
            .intent
            .snapshot
            .messages
            .iter()
            .any(|id| !messages.contains(id))
            || attempt
                .intent
                .snapshot
                .summaries
                .iter()
                .any(|id| !summaries.contains(id))
            || attempt
                .intent
                .coverage
                .messages
                .iter()
                .any(|id| !attempt.intent.snapshot.messages.contains(id))
            || attempt
                .intent
                .coverage
                .summaries
                .iter()
                .any(|id| !attempt.intent.snapshot.summaries.contains(id))
        {
            return Err(invalid(
                "attempt context source is missing or outside its boundary",
            ));
        }

        if let Some(turn) = &attempt.provider_turn
            && (turn.is_empty() || !provider_turns.insert((attempt.intent.recipient, turn)))
        {
            return Err(invalid("duplicate or empty accepted provider identity"));
        }

        match &attempt.state {
            AttemptState::Accepted { provider_turn } if attempt.provider_turn.as_ref() != Some(provider_turn) => return Err(invalid("accepted provider identity changed")),
            AttemptState::Completed { message } if !room.messages.iter().any(|entry| entry.id == *message && matches!(entry.author, Author::Member { id, .. } if id == attempt.intent.recipient)) => return Err(invalid("completed reply or its author is missing")),
            _ => {}
        }

        let budget = match attempt.intent.budget {
            BudgetScope::Discussion(id) => room
                .discussions
                .iter()
                .find(|run| run.id == id)
                .map(|run| &run.budget),
            BudgetScope::Direct(id) => room.direct_allowances.get(&id),
        }
        .ok_or(invalid("attempt has no budget"))?;

        let reservation = budget.reservations().get(&attempt.id);

        match attempt.state {
            AttemptState::Rejected if reservation.is_none() => {}
            AttemptState::Reserved
                if reservation.is_some_and(|entry| {
                    entry.state == ReservationState::Unsent
                        && entry.purpose == attempt.intent.purpose
                }) => {}
            AttemptState::Sending
            | AttemptState::Accepted { .. }
            | AttemptState::Completed { .. }
            | AttemptState::Summarized { .. }
            | AttemptState::Failed
            | AttemptState::Uncertain
            | AttemptState::Abandoned
                if reservation.is_some_and(|entry| {
                    entry.state == ReservationState::Charged
                        && entry.purpose == attempt.intent.purpose
                }) => {}
            _ => return Err(invalid("attempt and budget reservation disagree")),
        }
    }

    for member in &room.members {
        if !member.coverage.messages.is_subset(&messages)
            || !member.coverage.summaries.is_subset(&summaries)
        {
            return Err(invalid("accepted context source is missing"));
        }
    }

    Ok(())
}
