//! The text a member is sent for one turn of Team work.

use serde_json::json;

use crate::team::attempt::{BudgetScope, DispatchIntent, Invocation};
use crate::team::budget::TurnPurpose;
use crate::team::content::UserInput;
use crate::team::context::{ContextError, ContextLimits};
use crate::team::discussion::{PublicSnapshot, StageKind};
use crate::team::room::Room;
use crate::team::session::TeamError;
use crate::team::session::planning::DispatchPlan;

/// Build the intent that sends `plan`'s recipient its turn: its role, what a
/// turn of this stage asks of it, the scheduling context a moderator decides
/// from, and the room context `snapshot` leaves it, within `limits`.
pub(super) fn build_intent(
    room: &Room,
    plan: DispatchPlan,
    backend_generation: u64,
    input: UserInput,
    snapshot: &PublicSnapshot,
    limits: &ContextLimits,
) -> Result<DispatchIntent, TeamError> {
    let DispatchPlan {
        recipient,
        operation,
        stage,
        budget,
        purpose,
    } = plan;

    let member = room.member(recipient).ok_or(TeamError::Unavailable)?;

    let instruction = stage_instruction(stage.map(|(_, kind)| kind));

    let mut instructions = format!(
        "Team role: {}\n{instruction}\n",
        json!({"name": member.name(), "role": member.role()})
    );

    if purpose == TurnPurpose::Moderation
        && let BudgetScope::Discussion(id) = budget
    {
        let discussion = room.discussion(id).ok_or(TeamError::Unavailable)?;

        let members: Vec<_> = discussion
            .participants()
            .iter()
            .filter_map(|id| room.member(*id))
            .map(|member| json!({"id": member.id(), "name": member.name()}))
            .collect();

        instructions.push_str(&format!("Application scheduling context: {}\n", json!({"operation": operation, "stage": stage.map(|(id, _)| id), "eligible_members": members, "remaining_non_report_turns": discussion.budget().remaining_non_report_turns()})));
    }

    let body_limits = ContextLimits {
        max_bytes: limits
            .max_bytes
            .checked_sub(instructions.len())
            .ok_or(ContextError::OversizedInput)?,
        recent_messages: limits.recent_messages,
    };

    let prepared = room.prepare_context(recipient, snapshot, &input, &body_limits)?;

    instructions.push_str(&prepared.text);

    Ok(DispatchIntent {
        invocation: Invocation::MemberConversation,
        recipient,
        ownership: member.ownership,
        backend_generation,
        operation,
        stage: stage.map(|(id, _)| id),
        budget,
        purpose,
        input,
        attachments: prepared.attachments,
        prepared_text: instructions,
        snapshot: snapshot.clone(),
        coverage: prepared.coverage,
    })
}

fn stage_instruction(kind: Option<StageKind>) -> &'static str {
    match kind {
        Some(StageKind::InitialAnswers) => {
            "Give your initial answer to the user's objective. Identify assumptions and reasons."
        }
        Some(StageKind::PeerResponses | StageKind::InvitedResponses) => {
            "Respond to the preceding public contributions. Explain agreement, disagreement, and any changed view."
        }
        Some(StageKind::Report) => {
            "Conclude with separate agreements, disagreements, each member's attributed reasons, and suggested next steps. Cite public message IDs. Do not present one member's preference as consensus."
        }
        Some(StageKind::ModeratorDecision) => {
            "Review public progress and call team_decide exactly once. Invite eligible member IDs or request a report with an empty recipients list. Prose alone cannot schedule work."
        }
        _ => "Answer this one request. Further Team turns require a separate scheduling decision.",
    }
}
