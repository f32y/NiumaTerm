use std::time::Duration;

use serde_json::json;

use crate::chat::Event;
use crate::dsh::mapping::{EventTracker, map_frame};

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
        let events = map_frame(&frame, "speed", &mut EventTracker::default());

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
fn incomplete_timing_does_not_invent_a_sample() {
    for data in [
        json!({"turn":1,"step":1,"usage":{"outputTokens":100}}),
        json!({"turn":1,"step":1,"stream":[{"type":"text-chunks","time0":1000,"texts":["text"]}]}),
    ] {
        let frame = json!({"payload":{"type":"session/event","sessionId":"speed", "event":{
            "type":"assistant/message","time":3000,"data":data
        }}});

        let events = map_frame(&frame, "speed", &mut EventTracker::default());

        assert!(
            !events
                .iter()
                .any(|event| matches!(event, Event::GenerationCompleted(_)))
        );
    }
}

#[test]
fn retries_retain_first_output_and_history_restores_the_same_samples() {
    use crate::dsh::history;

    let mut tracker = EventTracker::default();

    let records = vec![
        json!({"type":"turn/start","time":0,"data":{"turn":1}}),
        json!({"type":"step/start","time":0,"data":{"turn":1,"step":1}}),
        json!({"type":"assistant/attempt","time":2000,"data":{"turn":1,"step":1,
            "stream":[{"type":"text-chunks","time0":1000,"dt":[],"texts":["partial"]}]}}),
        json!({"type":"llm/retry","time":3000,"data":{"turn":1,"step":1}}),
        json!({"type":"assistant/message","time":7000,"data":{"turn":1,"step":1,
            "stream":[{"type":"text-chunks","time0":5000,"dt":[],"texts":["done"]}],
            "usage":{"outputTokens":120},"message":{"content":[{"type":"text","text":"done"}]}}}),
        json!({"type":"step/end","time":7000,"data":{"turn":1,"step":1}}),
        json!({"type":"turn/end","time":7000,"data":{"turn":1}}),
    ];

    let samples: Vec<_> = records
        .iter()
        .flat_map(|event| {
            let frame =
                json!({"payload":{"type":"session/event","sessionId":"speed","event":event}});

            map_frame(&frame, "speed", &mut tracker)
        })
        .filter_map(|event| match event {
            Event::GenerationCompleted(sample) => Some(sample),
            _ => None,
        })
        .collect();

    assert_eq!(samples.len(), 1);
    assert_eq!(samples[0].elapsed, Duration::from_secs(6));
    assert_eq!(samples[0].output_tokens, 120);

    let page = json!({"records":records.into_iter().map(|event| json!({"event":event})).collect::<Vec<_>>()});
    let replay = history::replay(&page);

    assert_eq!(replay[0].generation_samples, samples);
}

#[test]
fn zero_usage_and_clock_skew_match_recorded_decode_totals() {
    use crate::dsh::generation::GenerationTracker;
    use crate::transcript::turns::GenerationStats;

    let mut tracker = GenerationTracker::default();
    let mut stats = GenerationStats::default();

    for (step, start, end, tokens) in [(1, 1000, 3000, 0), (2, 4000, 3000, 100)] {
        let sample = tracker
            .apply(&json!({
                "type":"assistant/message","time":end,"data":{"turn":1,"step":step,
                "stream":[{"type":"text-chunks","time0":start,"dt":[],"texts":["text"]}],
                "usage":{"outputTokens":tokens}}
            }))
            .unwrap();

        assert!(stats.record(sample));
    }

    assert_eq!(stats.speed().unwrap().tokens_per_second, 50.0);
}

#[test]
fn empty_running_turn_does_not_restore_the_previous_turn_speed() {
    use crate::dsh::history;

    let page = json!({"records":[
        {"event":{"type":"turn/start","time":0,"data":{"turn":1}}},
        {"event":{"type":"assistant/message","time":3000,"data":{"turn":1,"step":1,
            "stream":[{"type":"text-chunks","time0":1000,"dt":[],"texts":["done"]}],
            "usage":{"outputTokens":100},"message":{"content":[{"type":"text","text":"done"}]}}}},
        {"event":{"type":"turn/end","time":3000,"data":{"turn":1}}},
        {"event":{"type":"turn/start","time":4000,"data":{"turn":2}}}
    ]});

    let turns = history::replay(&page);

    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].generation_samples.len(), 1);
    assert!(turns[1].generation_samples.is_empty());
}

#[test]
fn newer_session_decode_totals_survive_an_older_history_baseline() {
    use crate::dsh::projections::ProjectionTracker;

    let mut tracker = ProjectionTracker::default();

    let current = json!({"payload":{"type":"session/projection","sessionId":"speed",
        "key":"sessionStats","seq":20,"value":{"decodeTokens":600,"decodeMs":10000}}});

    tracker.apply(&current, "speed");

    assert!(
        tracker
            .apply_baseline(
                &json!({"sessionStats":{"decodeTokens":100,"decodeMs":5000}}),
                Some(10)
            )
            .is_empty()
    );

    let totals = tracker.session_stats().unwrap();

    assert_eq!(totals.decode_tokens, 600);
    assert_eq!(totals.decode_ms, 10000);
}
