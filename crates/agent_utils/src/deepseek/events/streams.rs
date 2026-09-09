//! Decode logical streams into the adapter's local delivery messages.

use std::collections::HashMap;

use serde_json::{Value, json};

use crate::deepseek::api::ApiClient;

pub(super) struct Streams {
    session_id: String,
    client_id: Option<String>,
    control_ready: bool,
    snapshot: Option<Value>,
    pending: HashMap<String, &'static str>,
}

impl Streams {
    pub(super) fn new(session_id: &str) -> Self {
        Self {
            session_id: session_id.to_string(),
            client_id: None,
            control_ready: false,
            snapshot: None,
            pending: HashMap::new(),
        }
    }

    pub(super) fn ready_snapshot(&self) -> Option<&Value> {
        if self.client_id.is_some() && self.control_ready {
            self.snapshot.as_ref()
        } else {
            None
        }
    }

    pub(super) fn process(
        &mut self,
        frame: Value,
        client: &ApiClient,
        deliver: &dyn Fn(Value),
    ) -> Result<(), String> {
        let id = frame["streamId"].as_str().unwrap_or_default();
        match frame["type"].as_str() {
            Some("error") => {
                return Err(format!(
                    "{id}: {}",
                    frame["error"]["message"]
                        .as_str()
                        .unwrap_or("stream failed")
                ));
            }
            Some("end") => return Err(format!("the harness ended the {id} stream")),
            Some("item") => {}
            _ => return Err("the harness sent an invalid stream message".to_string()),
        }
        let value = &frame["value"];
        match id {
            "events" => self.event(value, client, deliver)?,
            "control" => self.control(value, deliver),
            "follow" => match value["type"].as_str() {
                Some("snapshot") => {
                    deliver(json!({ "payload": {
                        "type": "nmt/replay", "sessionId": self.session_id, "page": value,
                    } }));
                    self.snapshot = Some(value.clone());
                }
                Some("event") => deliver(json!({ "payload": {
                    "type": "session/event", "sessionId": self.session_id, "event": value["event"],
                } })),
                _ => {}
            },
            _ => {}
        }
        Ok(())
    }

    fn control(&mut self, value: &Value, deliver: &dyn Fn(Value)) {
        match value["type"].as_str() {
            Some("baseline") => {
                let baseline = &value["value"];
                self.queue(&baseline["queues"][&self.session_id], deliver);
                let projections = &baseline["projections"][&self.session_id];
                for (key, value) in projections["values"].as_object().into_iter().flatten() {
                    self.projection(key, value, &projections["asOfSeq"], deliver);
                }
                self.control_ready = true;
            }
            Some("queue") if value["sessionId"] == self.session_id => {
                self.queue(&value["items"], deliver)
            }
            Some("projection") if value["sessionId"] == self.session_id => {
                if let Some(key) = value["key"].as_str() {
                    self.projection(key, &value["value"], &value["seq"], deliver);
                }
            }
            _ => {}
        }
    }

    fn queue(&self, items: &Value, deliver: &dyn Fn(Value)) {
        deliver(json!({ "payload": {
            "type": "session/queue", "sessionId": self.session_id, "items": items,
        } }));
    }

    fn projection(&self, key: &str, value: &Value, seq: &Value, deliver: &dyn Fn(Value)) {
        deliver(json!({ "payload": {
            "type": "session/projection", "sessionId": self.session_id,
            "key": key, "value": value, "seq": seq,
        } }));
    }

    fn event(
        &mut self,
        value: &Value,
        client: &ApiClient,
        deliver: &dyn Fn(Value),
    ) -> Result<(), String> {
        match value["type"].as_str() {
            Some("ready") => {
                self.client_id = Some(
                    value["clientId"]
                        .as_str()
                        .ok_or("the harness event stream supplied no generation id")?
                        .to_string(),
                );
            }
            Some("emit")
                if value["event"] == "api-session/error" && value["args"][0] == self.session_id =>
            {
                deliver(json!({ "payload": {
                    "type": "host/agent-error", "sessionId": self.session_id, "message": value["args"][1],
                } }));
            }
            Some("waterfall") => {
                let client_id = self
                    .client_id
                    .as_deref()
                    .ok_or("an interaction arrived before event readiness")?;
                let event_id = value["eventId"]
                    .as_str()
                    .ok_or("an interaction arrived without an event id")?;
                let kind = match value["event"].as_str() {
                    Some("approval/request") => Some(("approval/requested", "approval/resolved")),
                    Some("user-questions/request") => {
                        Some(("question/requested", "question/resolved"))
                    }
                    _ => None,
                };
                if value["agentId"] != self.session_id || kind.is_none() {
                    client
                        .respond_event(client_id, event_id, json!({ "kind": "next" }))
                        .map_err(|error| error.message().to_string())?;
                    return Ok(());
                }
                let (requested, resolved) = kind.unwrap();
                let mut payload = value["request"].clone();
                payload["type"] = json!(requested);
                payload["sessionId"] = json!(self.session_id);
                // The pane holds one card of each kind. Successful answers do
                // not echo a cancellation to their sender, so replace the old
                // identity when the next card arrives instead of retaining it.
                self.pending.retain(|_, kind| *kind != resolved);
                self.pending.insert(event_id.to_string(), resolved);
                deliver(json!({ "clientId": client_id, "eventId": event_id, "payload": payload }));
            }
            Some("cancel") => {
                if let Some(event_id) = value["eventId"].as_str()
                    && let Some(kind) = self.pending.remove(event_id)
                {
                    deliver(json!({ "eventId": event_id, "payload": {
                        "type": kind, "sessionId": self.session_id,
                    } }));
                }
            }
            _ => {}
        }
        Ok(())
    }
}
