use std::net::TcpListener;
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use serde_json::{Value, from_str, json};
use tokio_tungstenite::tungstenite::{Message, accept};

use crate::background_task::{BackgroundTaskKind, BackgroundTaskLoadState};
use crate::chat::{Event, Item};
use crate::dsh::api::ApiClient;
use crate::dsh::catalogs;
use crate::dsh::session::jobs::{
    JOB_TRANSCRIPT_FRAME, job_transcript_events, load_job_transcript, transcript,
};

fn job() -> Value {
    json!({"id": "pwsh-1", "kind": "pwsh", "label": "Write-Output hello",
        "status": "completed", "detail": "exit code: 0", "startedAt": 1000,
        "finishedAt": 2000})
}

fn output_records(id: &str, call_id: &str, text: &str, legacy: bool) -> Vec<Value> {
    let call = json!({"event": {"type": "tool/call", "time": 1500, "data": {
        "callId": call_id, "name": "job_output",
        "arguments": json!({"job_id": id}).to_string()
    }}});

    let content = json!([{"type": "text", "text": text}]);

    let message = if legacy {
        json!({"role": "user", "source": {"kind": "tool", "callId": call_id},
            "content": [{"type": "tool-result", "toolCallId": call_id, "content": content}]})
    } else {
        json!({"role": "tool", "toolCallId": call_id, "content": content})
    };

    vec![
        call,
        json!({"event": {"type": "tool/result", "time": 1600,
        "data": {"message": message}}}),
    ]
}

#[test]
fn job_details_include_command_status_and_only_its_recorded_output() {
    for legacy in [false, true] {
        let row = catalogs::job_rows(&json!([job()]), "parent", 1).remove(0);

        assert_eq!(row.kind, BackgroundTaskKind::Shell);

        let mut records = output_records("pwsh-1", "read-1", "hello\n[status: running]", legacy);

        records.extend(output_records(
            "pwsh-2",
            "other",
            "unrelated output",
            legacy,
        ));

        records.extend(output_records(
            "pwsh-1",
            "read-2",
            "done\n[status: completed]",
            legacy,
        ));

        let Event::BackgroundTaskTranscript { key, update } =
            transcript(&row, &json!({"records": records}))
        else {
            panic!("expected job details");
        };

        assert_eq!(key, row.key);
        assert_eq!(update.state, Some(BackgroundTaskLoadState::Ready));
        assert!(update.replace);

        let Item::CommandExecution {
            command,
            aggregated_output,
            status,
            ..
        } = &update.items[0]
        else {
            panic!("expected command details");
        };

        assert_eq!(command, "Write-Output hello");
        assert_eq!(status.as_deref(), Some("completed"));

        let output = aggregated_output.as_deref().unwrap();

        assert!(output.contains("exit code: 0"));
        assert!(output.contains("hello\n[status: running]"));
        assert!(output.contains("done\n[status: completed]"));
        assert!(!output.contains("unrelated output"));
    }
}

#[test]
fn job_without_recorded_output_still_has_readable_details() {
    let mut running = job();

    running["status"] = json!("running");
    running["detail"] = Value::Null;

    let row = catalogs::job_rows(&json!([running]), "parent", 1).remove(0);

    let Event::BackgroundTaskTranscript { update, .. } = transcript(&row, &Value::Null) else {
        panic!("expected job details");
    };

    assert_eq!(update.state, Some(BackgroundTaskLoadState::Ready));
    assert!(matches!(&update.items[0], Item::CommandExecution {
        status: Some(status), aggregated_output: Some(output), ..
    } if status == "inProgress" && output.contains("No output has been recorded")));
}

#[test]
fn a_reused_job_id_does_not_include_output_from_before_its_start() {
    let row = catalogs::job_rows(&json!([job()]), "parent", 1).remove(0);

    let mut records = output_records("pwsh-1", "old-read", "old process output", false);

    for record in &mut records {
        record["event"]["time"] = json!(500);
    }

    records.extend(output_records(
        "pwsh-1",
        "new-read",
        "current process output",
        false,
    ));

    let Event::BackgroundTaskTranscript { update, .. } =
        transcript(&row, &json!({"records": records}))
    else {
        panic!("expected job details");
    };

    assert!(matches!(&update.items[0], Item::CommandExecution {
        aggregated_output: Some(output), ..
    } if output.contains("current process output") && !output.contains("old process output")));
}

#[test]
fn reading_job_details_uses_the_parent_history_and_ignores_a_different_session() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();

    let client = nmt_platform::runtime()
        .block_on(ApiClient::new(format!(
            "http://{}",
            listener.local_addr().unwrap()
        )))
        .unwrap();

    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();

        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();

        let mut socket = accept(stream).unwrap();

        let request = socket.read().unwrap();
        let request: Value = from_str(request.to_text().unwrap()).unwrap();

        assert_eq!(request["endpoint"], "session/follow");
        assert_eq!(
            request["payload"]["args"]["request"]["address"],
            json!({"kind": "session", "sessionId": "parent"})
        );

        let records = output_records("pwsh-1", "read-1", "saved output", false);

        socket
            .send(Message::Text(
                json!({
                    "type": "item", "streamId": "snapshot",
                    "value": {"type": "snapshot", "records": records}
                })
                .to_string()
                .into(),
            ))
            .unwrap();

        assert!(matches!(socket.read().unwrap(), Message::Close(_)));
    });

    let (sender, receiver) = mpsc::channel();

    load_job_transcript(
        client,
        "parent".into(),
        "pwsh-1".into(),
        Arc::new(move |frame| {
            sender.send(frame).unwrap();
        }),
    );

    let frame = receiver.recv_timeout(Duration::from_secs(5)).unwrap();
    let payload = &frame["payload"];

    assert_eq!(payload["type"], JOB_TRANSCRIPT_FRAME);

    let rows = catalogs::job_rows(&json!([job()]), "parent", 1);

    assert!(job_transcript_events(payload, "another-session", &rows).is_empty());
    assert!(job_transcript_events(payload, "parent", &[]).is_empty());

    let events = job_transcript_events(payload, "parent", &rows);

    assert!(
        matches!(&events[0], Event::BackgroundTaskTranscript { update, .. }
        if matches!(&update.items[0], Item::CommandExecution {
            aggregated_output: Some(output), ..
        } if output.contains("saved output")))
    );

    server.join().unwrap();
}

#[test]
fn failed_history_read_reports_the_error_and_keeps_job_details_readable() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();

    let client = nmt_platform::runtime()
        .block_on(ApiClient::new(format!(
            "http://{}",
            listener.local_addr().unwrap()
        )))
        .unwrap();

    drop(listener);

    let (sender, receiver) = mpsc::channel();

    load_job_transcript(
        client,
        "parent".into(),
        "pwsh-1".into(),
        Arc::new(move |frame| {
            sender.send(frame).unwrap();
        }),
    );

    let frame = receiver.recv_timeout(Duration::from_secs(5)).unwrap();
    let rows = catalogs::job_rows(&json!([job()]), "parent", 1);
    let events = job_transcript_events(&frame["payload"], "parent", &rows);

    let Event::BackgroundTaskTranscript { update, .. } = &events[0] else {
        panic!("expected job details");
    };

    assert_eq!(update.state, Some(BackgroundTaskLoadState::Ready));
    assert!(
        matches!(&update.items[0], Item::CommandExecution { command, .. }
        if command == "Write-Output hello")
    );
    assert!(matches!(update.items.last(), Some(Item::Error { text })
        if text.starts_with("Recorded job output could not be read:")));
}
