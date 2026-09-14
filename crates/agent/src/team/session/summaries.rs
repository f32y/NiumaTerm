use crate::team::attempt::BudgetScope;
#[cfg(test)]
use crate::team::content::AttributedPosition;
use crate::team::content::{Author, SourceFragment};
use crate::team::context::SummaryChunk;
use crate::team::discussion::PublicSnapshot;
use crate::team::identity::{MemberId, StageId};
#[cfg(test)]
use serde::Deserialize;
use serde::Serialize;

pub struct SummaryRequest {
    pub owner: MemberId,
    pub budget: BudgetScope,
    pub stage: Option<StageId>,
    pub snapshot: PublicSnapshot,
    pub chunks: Vec<SummaryChunk>,
}

#[cfg(test)]
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SummaryText {
    pub goals: String,
    pub constraints: String,
    pub agreements: String,
    pub disagreements: Vec<AttributedPosition>,
}

#[derive(Serialize)]
pub(super) struct FragmentInput<'a> {
    pub(super) source: &'a SourceFragment,
    pub(super) author: &'a Author,
    pub(super) text: &'a str,
}
