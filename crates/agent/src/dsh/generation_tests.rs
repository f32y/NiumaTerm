use std::time::Duration;

use serde_json::json;

use crate::chat::Event;
use crate::dsh::mapping::{ToolTracker, map_frame};

#[test]
fn completed_response_uses_recorded_generation_time_and_actual_usage() {
    for stream in [
        json!([{"type":"reasoning-chunks","time0":1000,"dt":[250],"texts":["", "thinking"]}]),
        json!([{"type":"tool-call-chunks","time0":1250,"name":"shell","args":[""],"dt":[]}]),
        json!([{"type":"chunk","time":1250,"chunk":{"type":"tool-call-delta","argumentsDelta":"{}"}}]),
    ] {
        let event = json!({
            "type":"assistant/message", "time":3250,
            "data":{"turn":2,"step":3,"stream":stream,"usage":{"outputTokens":120},
                "message":{"content":[{"type":"text","text":"done"}]}}
        });

        let frame = json!({"payload":{"type":"session/event","sessionId":"speed","event":event}});
        let events = map_frame(&frame, "speed", &mut ToolTracker::default());

        let Some(Event::GenerationCompleted(sample)) = events.last() else {
            panic!("completed response did not report a generation sample");
        };

        assert_eq!(sample.response_id, "2:3");
        assert_eq!(sample.output_tokens, 120);
        assert_eq!(sample.elapsed, Duration::from_secs(2));
        assert!(!sample.estimated);
    }
}

#[test]
fn incomplete_or_reversed_timing_does_not_invent_a_sample() {
    for data in [
        json!({"turn":1,"step":1,"usage":{"outputTokens":100}}),
        json!({"turn":1,"step":1,"stream":[{"type":"text-chunks","time0":4000,"texts":["text"]}],"usage":{"outputTokens":100}}),
        json!({"turn":1,"step":1,"stream":[{"type":"text-chunks","time0":1000,"texts":["text"]}]}),
    ] {
        let frame = json!({"payload":{"type":"session/event","sessionId":"speed", "event":{
            "type":"assistant/message","time":3000,"data":data
        }}});

        let events = map_frame(&frame, "speed", &mut ToolTracker::default());

        assert!(
            !events
                .iter()
                .any(|event| matches!(event, Event::GenerationCompleted(_)))
        );
    }
}
