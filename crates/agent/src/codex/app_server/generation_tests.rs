use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::chat::Event;
use crate::codex::app_server::conversation::ThreadState;

fn usage(total: u64, last: u64) -> Value {
    json!({"turnId":"turn", "tokenUsage": {
        "total":{"totalTokens":total,"outputTokens":total},
        "last":{"totalTokens":last,"outputTokens":last}
    }})
}

#[test]
fn generation_ignores_tool_output_and_repeated_usage() {
    let mut state = ThreadState::default();

    state.on_notification("turn/started", &json!({"turn":{"id":"turn"}}));

    state.on_notification(
        "item/reasoning/summaryTextDelta",
        &json!({"itemId":"r","delta":"thinking"}),
    );

    assert!(state.generation_span.is_some());

    let first = Instant::now() - Duration::from_secs(20);

    state.generation_span = Some((first, first + Duration::from_secs(2)));

    state.on_notification(
        "item/commandExecution/outputDelta",
        &json!({"itemId":"tool","delta":"output"}),
    );

    let events = state.on_notification("thread/tokenUsage/updated", &usage(120, 120));

    let Some(Event::GenerationCompleted(sample)) = events.last() else {
        panic!("usage did not close the generated response");
    };

    assert_eq!(sample.elapsed, Duration::from_secs(2));
    assert_eq!(sample.output_tokens, 120);
    assert!(sample.estimated);

    state.on_notification(
        "item/agentMessage/delta",
        &json!({"itemId":"next","delta":"answer"}),
    );

    let span = state.generation_span;
    let repeated = state.on_notification("thread/tokenUsage/updated", &usage(120, 120));

    assert!(
        !repeated
            .iter()
            .any(|event| matches!(event, Event::GenerationCompleted(_)))
    );
    assert_eq!(state.generation_span, span);

    state.generation_span = Some((first, first + Duration::from_secs(1)));

    let events = state.on_notification("thread/tokenUsage/updated", &usage(150, 30));

    let Some(Event::GenerationCompleted(sample)) = events.last() else {
        panic!("next response did not produce its own sample");
    };

    assert_eq!(sample.output_tokens, 30);
    assert_eq!(sample.elapsed, Duration::from_secs(1));
}

#[test]
fn retries_and_new_turns_drop_unfinished_generation() {
    for boundary in ["error", "turn/started", "turn/completed"] {
        let mut state = ThreadState::default();

        state.on_notification("turn/started", &json!({"turn":{"id":"turn"}}));

        state.on_notification(
            "item/agentMessage/delta",
            &json!({"itemId":"old","delta":"unfinished"}),
        );

        state.on_notification(boundary, &json!({"turn":{"id":"turn"}}));

        let events = state.on_notification("thread/tokenUsage/updated", &usage(120, 120));

        assert!(
            !events
                .iter()
                .any(|event| matches!(event, Event::GenerationCompleted(_)))
        );
    }
}
