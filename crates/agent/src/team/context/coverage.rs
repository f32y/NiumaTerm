use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

use crate::team::content::Summary;
use crate::team::context::ContextError;
use crate::team::identity::MessageId;
use crate::team::room::Room;

pub(super) fn include_summary(
    room: &Room,
    summary: &Summary,
    ranges: &mut BTreeMap<MessageId, Vec<Range<usize>>>,
) -> Result<(), ContextError> {
    let mut pending = vec![summary];
    let mut visited = BTreeSet::new();

    while let Some(summary) = pending.pop() {
        if !visited.insert(summary.id) {
            continue;
        }

        for source in &summary.sources {
            let message = room
                .messages
                .iter()
                .find(|message| message.id == *source)
                .ok_or(ContextError::MissingSource)?;

            let parts: Vec<_> = summary
                .fragments
                .iter()
                .filter(|fragment| fragment.source == *source)
                .collect();

            let source_ranges = ranges.entry(*source).or_default();

            if parts.is_empty() {
                source_ranges.push(0..message.text.len());
            } else {
                for part in parts {
                    if part.start > part.end
                        || part.end > message.text.len()
                        || !message.text.is_char_boundary(part.start)
                        || !message.text.is_char_boundary(part.end)
                    {
                        return Err(ContextError::MissingSource);
                    }

                    source_ranges.push(part.start..part.end);
                }
            }
        }

        for id in &summary.prior_summaries {
            pending.push(
                room.summaries
                    .iter()
                    .find(|summary| summary.id == *id)
                    .ok_or(ContextError::MissingSource)?,
            );
        }
    }

    Ok(())
}

pub(super) fn complete_sources(
    room: &Room,
    ranges: &BTreeMap<MessageId, Vec<Range<usize>>>,
) -> Result<BTreeSet<MessageId>, ContextError> {
    let mut complete = BTreeSet::new();

    for (id, parts) in ranges {
        let message = room
            .messages
            .iter()
            .find(|message| message.id == *id)
            .ok_or(ContextError::MissingSource)?;

        let mut parts = parts.clone();

        parts.sort_by_key(|part| part.start);

        let mut end = 0;

        for part in parts {
            if part.start > end {
                break;
            }

            end = end.max(part.end);
        }

        if end == message.text.len() {
            complete.insert(*id);
        }
    }

    Ok(complete)
}
