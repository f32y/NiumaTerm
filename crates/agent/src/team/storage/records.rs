use std::collections::BTreeMap;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use crate::team::attempt::Attempt;
use crate::team::budget::Budget;
use crate::team::content::{PublicMessage, Summary, UserInput};
use crate::team::discussion::Discussion;
use crate::team::identity::OperationId;
use crate::team::member::Member;
use crate::team::room::{Room, RoomControls};
use crate::team::storage::{StorageError, VERSION};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Checkpoint {
    pub version: u32,
    pub sequence: u64,
    pub digest: String,
    pub room: Room,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct JournalRecord {
    pub version: u32,
    pub sequence: u64,
    pub previous: String,
    pub changes: RoomDelta,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Checked<T> {
    payload: T,
    digest: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RoomDelta {
    members: Vec<Member>,
    discussions: Vec<Discussion>,
    messages: Vec<PublicMessage>,
    summaries: Vec<Summary>,
    input_history: Vec<UserInput>,
    controls: RoomControls,
    attempts: Vec<Attempt>,
    direct_allowances: BTreeMap<OperationId, Budget>,
}

impl RoomDelta {
    pub fn between(previous: &Room, next: &Room) -> Result<Self, StorageError> {
        if previous.id != next.id || previous.workspace != next.workspace {
            return Err(StorageError::Invalid("room identity or workspace changed"));
        }

        if !next.messages.starts_with(&previous.messages)
            || !next.summaries.starts_with(&previous.summaries)
            || !next.input_history.starts_with(&previous.input_history)
            || previous
                .members
                .iter()
                .any(|old| next.member(old.id).is_none())
            || previous
                .discussions
                .iter()
                .any(|old| !next.discussions.iter().any(|new| new.id == old.id))
            || previous
                .attempts
                .iter()
                .any(|old| !next.attempts.iter().any(|new| new.id == old.id))
        {
            return Err(StorageError::Invalid(
                "retained history was removed or replaced",
            ));
        }

        for member in &previous.members {
            let new = next
                .member(member.id)
                .ok_or(StorageError::Invalid("member is missing"))?;

            if member.conversation == new.conversation && member.roots != new.roots {
                return Err(StorageError::Invalid("existing conversation roots changed"));
            }
        }

        for old in &previous.attempts {
            let new = next
                .attempts
                .iter()
                .find(|attempt| attempt.id == old.id)
                .ok_or(StorageError::Invalid("attempt is missing"))?;

            if old.intent != new.intent
                || old
                    .provider_turn
                    .as_ref()
                    .is_some_and(|id| new.provider_turn.as_ref() != Some(id))
            {
                return Err(StorageError::Invalid(
                    "attempt input or accepted provider identity changed",
                ));
            }
        }

        Ok(Self {
            members: next
                .members
                .iter()
                .filter(|member| !previous.members.contains(member))
                .cloned()
                .collect(),
            discussions: next
                .discussions
                .iter()
                .filter(|run| !previous.discussions.contains(run))
                .cloned()
                .collect(),
            messages: next.messages[previous.messages.len()..].to_vec(),
            summaries: next.summaries[previous.summaries.len()..].to_vec(),
            input_history: next.input_history[previous.input_history.len()..].to_vec(),
            controls: next.controls.clone(),
            attempts: next
                .attempts
                .iter()
                .filter(|attempt| !previous.attempts.contains(attempt))
                .cloned()
                .collect(),
            direct_allowances: next.direct_allowances.clone(),
        })
    }

    pub fn apply(self, room: &mut Room) {
        for member in self.members {
            if let Some(existing) = room.members.iter_mut().find(|old| old.id == member.id) {
                *existing = member;
            } else {
                room.members.push(member);
            }
        }

        for run in self.discussions {
            if let Some(existing) = room.discussions.iter_mut().find(|old| old.id == run.id) {
                *existing = run;
            } else {
                room.discussions.push(run);
            }
        }

        room.messages.extend(self.messages);
        room.summaries.extend(self.summaries);
        room.input_history.extend(self.input_history);
        room.controls = self.controls;

        for attempt in self.attempts {
            if let Some(existing) = room.attempts.iter_mut().find(|old| old.id == attempt.id) {
                *existing = attempt;
            } else {
                room.attempts.push(attempt);
            }
        }

        room.direct_allowances = self.direct_allowances;
    }
}

pub(super) fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(super) fn encode<T: Serialize>(payload: &T) -> Result<Vec<u8>, StorageError> {
    Ok(serde_json::to_vec(&Checked {
        digest: digest(&serde_json::to_vec(payload)?),
        payload,
    })?)
}

pub(super) fn decode<T: Serialize + DeserializeOwned>(bytes: &[u8]) -> Result<T, StorageError> {
    let raw: Value = serde_json::from_slice(bytes)?;

    let version = raw
        .pointer("/payload/version")
        .and_then(Value::as_u64)
        .ok_or(StorageError::Invalid("missing record version"))?;

    if version != u64::from(VERSION) {
        return Err(StorageError::UnsupportedVersion(version));
    }

    let checked: Checked<T> = serde_json::from_slice(bytes)?;

    if digest(&serde_json::to_vec(&checked.payload)?) != checked.digest {
        return Err(StorageError::Invalid("record checksum mismatch"));
    }

    Ok(checked.payload)
}
