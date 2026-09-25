//! Decode timing retained across attempts within a model step.

use std::time::Duration;

use serde_json::Value;

use crate::chat::GenerationSample;

#[derive(Default)]
pub(crate) struct GenerationTracker {
    step: Option<(u64, u64)>,
    first_token: Option<u64>,
}

impl GenerationTracker {
    pub(crate) fn apply(&mut self, event: &Value) -> Option<GenerationSample> {
        let data = &event["data"];
        let kind = event["type"].as_str()?;

        if matches!(kind, "step/end" | "turn/start" | "turn/end") {
            *self = Self::default();

            return None;
        }

        if !matches!(
            kind,
            "step/start" | "assistant/attempt" | "assistant/message"
        ) {
            return None;
        }

        let step = (data["turn"].as_u64()?, data["step"].as_u64()?);

        if self.step != Some(step) || kind == "step/start" {
            self.step = Some(step);
            self.first_token = None;
        }

        if self.first_token.is_none() {
            self.first_token = data["stream"]
                .as_array()
                .and_then(|stream| stream.iter().find_map(first_token_time));
        }

        if kind != "assistant/message" {
            return None;
        }

        let started = self.first_token.take()?;

        Some(GenerationSample {
            response_id: format!("{}:{}", step.0, step.1),
            output_tokens: data["usage"]["outputTokens"].as_u64()?,
            elapsed: Duration::from_millis(event["time"].as_u64()?.saturating_sub(started)),
            estimated: false,
        })
    }
}

fn first_token_time(record: &Value) -> Option<u64> {
    let kind = record["type"].as_str()?;

    if kind == "chunk" {
        let chunk = &record["chunk"];

        let has_output = match chunk["type"].as_str()? {
            "text-delta" | "reasoning-delta" => {
                chunk["text"].as_str().is_some_and(|text| !text.is_empty())
            }
            "tool-call-delta" => {
                chunk["name"].as_str().is_some()
                    || chunk["argumentsDelta"]
                        .as_str()
                        .is_some_and(|text| !text.is_empty())
            }
            _ => false,
        };

        return has_output.then(|| record["time"].as_u64()).flatten();
    }

    let fragments = match kind {
        "text-chunks" | "reasoning-chunks" => &record["texts"],
        "tool-call-chunks" => {
            if record["name"].as_str().is_some() {
                return record["time0"].as_u64();
            }

            &record["args"]
        }
        _ => return None,
    };

    let mut time = record["time0"].as_u64()?;

    for (index, fragment) in fragments.as_array()?.iter().enumerate() {
        if index > 0 {
            time = time.checked_add(record["dt"][index - 1].as_u64()?)?;
        }

        if !fragment.as_str()?.is_empty() {
            return Some(time);
        }
    }

    None
}
