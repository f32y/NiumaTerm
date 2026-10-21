#[cfg(test)]
#[path = "jobs_tests.rs"]
mod tests;

use std::sync::Arc;
use std::time::UNIX_EPOCH;

use serde::Deserialize;
use serde_json::{Value, from_str, json};

use crate::background_task::{
    BackgroundTaskState, BackgroundTaskSummary, BackgroundTaskTranscriptUpdate,
};
use crate::chat::{Event, Item};
use crate::dsh::api::ApiClient;
use crate::dsh::events::{session_address, snapshot};
use crate::dsh::frames;
use crate::dsh::mapping::{EventTracker, map_session_event};
use crate::dsh::session::REPLAY_MESSAGES;

pub(super) const JOB_TRANSCRIPT_FRAME: &str = "nmt/job-transcript";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct JobTranscriptFrame {
    session_id: String,
    job_id: String,
    #[serde(default)]
    page: Value,
    error: Option<String>,
}

/// Job output reads consume the agent's cursor. Reading its session log keeps
/// those reads unchanged and displays only output already recorded there.
pub(super) fn load_job_transcript(
    client: ApiClient,
    session_id: String,
    job_id: String,
    deliver: Arc<dyn Fn(Value) + Send + Sync>,
) {
    nmt_platform::runtime().spawn(async move {
        let mut payload = json!({
            "type": JOB_TRANSCRIPT_FRAME, "sessionId": session_id, "jobId": job_id,
        });

        match snapshot(&client, session_address(&session_id), REPLAY_MESSAGES).await {
            Ok(page) => payload["page"] = page,
            Err(error) => payload["error"] = json!(error.message()),
        }

        deliver(json!({"payload": payload}));
    });
}

pub(super) fn job_transcript_events(
    payload: &Value,
    session_id: &str,
    rows: &[BackgroundTaskSummary],
) -> Vec<Event> {
    let Some(frame) = frames::parse::<JobTranscriptFrame>(JOB_TRANSCRIPT_FRAME, payload) else {
        return Vec::new();
    };

    if frame.session_id != session_id {
        return Vec::new();
    }

    let Some(row) = rows.iter().find(|row| row.key.id == frame.job_id) else {
        return Vec::new();
    };

    let mut event = transcript(row, &frame.page);

    if let Some(message) = frame.error
        && let Event::BackgroundTaskTranscript { update, .. } = &mut event
    {
        update.items.push(Item::Error {
            text: format!("Recorded job output could not be read: {message}"),
        });
    }

    vec![event]
}

fn transcript(row: &BackgroundTaskSummary, page: &Value) -> Event {
    let (status, label) = match row.state {
        BackgroundTaskState::Starting => ("inProgress", "starting"),
        BackgroundTaskState::Working => ("inProgress", "running"),
        BackgroundTaskState::NeedsInput => ("inProgress", "waiting for input"),
        BackgroundTaskState::Done => ("completed", "completed"),
        BackgroundTaskState::Interrupted => ("failed", "interrupted"),
        BackgroundTaskState::Stopped => ("failed", "stopped"),
        BackgroundTaskState::Failed => ("failed", "failed"),
    };

    let mut output = format!("Job: {}\nStatus: {label}", row.key.id);

    if let Some(detail) = &row.status {
        output.push_str(&format!(" ({detail})"));
    }

    let recorded = recorded_output(row, page);

    if recorded.is_empty() {
        output.push_str("\n\nNo output has been recorded in the recent conversation yet. Output appears after the agent reads the job.");
    } else {
        output.push_str("\n\nRecorded output from the recent conversation (only output already read by the agent):\n\n");
        output.push_str(&recorded.join("\n\n"));
    }

    Event::BackgroundTaskTranscript {
        key: row.key.clone(),
        update: BackgroundTaskTranscriptUpdate::loaded(vec![Item::CommandExecution {
            id: format!("job:{}", row.key.id),
            command: row.display_label(),
            purpose: None,
            aggregated_output: Some(output),
            status: Some(status.to_string()),
            exit_code: None,
        }]),
    }
}

fn recorded_output(row: &BackgroundTaskSummary, page: &Value) -> Vec<String> {
    let mut tools = EventTracker::default();
    let mut output = Vec::new();

    let started = row
        .started_at
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok());

    for record in page["records"].as_array().into_iter().flatten() {
        let event = &record["event"];
        let data = &event["data"];

        // A restarted host can reuse a job id within the same conversation.
        if event["time"]
            .as_u64()
            .zip(started)
            .is_some_and(|(time, start)| u128::from(time) < start.as_millis())
        {
            continue;
        }

        match event["type"].as_str() {
            Some("tool/call") => {
                let arguments: Value = data["arguments"]
                    .as_str()
                    .and_then(|text| from_str(text).ok())
                    .unwrap_or_else(|| data["arguments"].clone());

                if data["name"] != "job_output" || arguments["job_id"] != row.key.id {
                    continue;
                }
            }
            Some("tool/result") => {}
            _ => continue,
        }

        for mapped in map_session_event(event, &Value::Null, &mut tools) {
            if let Event::ItemCompleted(Item::Other {
                output: Some(text), ..
            }) = mapped
            {
                output.push(text);
            }
        }
    }

    output
}
