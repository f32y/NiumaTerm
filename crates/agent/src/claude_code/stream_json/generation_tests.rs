use serde_json::json;

use crate::chat::Event;
use crate::claude_code::stream_json::transcript::TranscriptState;

#[test]
fn generation_includes_tool_arguments_and_settles_once_per_message() {
    let mut state = TranscriptState::default();

    state.begin_turn();
    state.on_stream_event(&json!({"event":{"type":"message_start","message":{"id":"response","usage":{"input_tokens":20,"output_tokens":1}}}}));
    state.on_stream_event(&json!({"event":{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{}"}}}));
    state.on_stream_event(&json!({"event":{"type":"message_delta","usage":{"output_tokens":120}}}));

    let events = state.on_stream_event(&json!({"event":{"type":"message_stop"}}));

    let Some(Event::GenerationCompleted(sample)) = events.first() else {
        panic!("message stop did not produce a generation sample");
    };

    assert_eq!(sample.response_id, "response");
    assert_eq!(sample.output_tokens, 120);
    assert!(!sample.estimated);
    assert!(
        state
            .on_stream_event(&json!({"event":{"type":"message_stop"}}))
            .is_empty()
    );

    state.on_stream_event(&json!({"event":{"type":"message_start","message":{"id":"next","usage":{"input_tokens":20,"output_tokens":1}}}}));
    state.on_stream_event(&json!({"parent_tool_use_id":"child","event":{"type":"content_block_delta","index":0,"delta":{"text":"child output"}}}));

    assert!(
        state
            .on_stream_event(&json!({"event":{"type":"message_stop"}}))
            .is_empty()
    );
}
