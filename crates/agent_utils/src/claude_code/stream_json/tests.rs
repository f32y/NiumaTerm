use std::collections::HashMap;
use std::thread;

use crate::claude_code::stream_json::*;

#[test]
fn turn_output_usage_accumulates_model_responses() {
    let mut usage = TurnOutputUsage::default();

    assert_eq!(usage.start_response(0), 0);
    assert_eq!(usage.update_response(120), 120);
    assert_eq!(usage.start_response(0), 120);
    assert_eq!(usage.update_response(35), 155);

    usage.reset();
    assert_eq!(usage.update_response(9), 9);
}

#[test]
fn claude_usage_normalizes_cache_categories_into_total_input() {
    let usage = parse_claude_usage(&json!({
        "input_tokens": 8_500,
        "cache_creation_input_tokens": 5_000,
        "cache_read_input_tokens": 2_000,
        "output_tokens": 1_200
    }))
    .expect("Claude token usage should parse");

    assert_eq!(
        usage,
        TokenUsageBreakdown {
            total_tokens: 16_700,
            input_tokens: Some(15_500),
            cache_read_input_tokens: Some(2_000),
            cache_write_input_tokens: Some(5_000),
            output_tokens: Some(1_200),
            reasoning_output_tokens: None,
        }
    );
}

#[test]
fn claude_context_updates_output_and_labels_last_turn_usage() {
    let mut current = parse_claude_usage(&json!({
        "input_tokens": 9_000,
        "cache_creation_input_tokens": null,
        "cache_read_input_tokens": 1_000,
        "output_tokens": 0
    }));
    update_claude_output(&mut current, 750);
    let last_turn = parse_claude_usage(&json!({
        "input_tokens": 20_000,
        "cache_creation_input_tokens": 4_000,
        "cache_read_input_tokens": 11_000,
        "output_tokens": 2_000
    }));

    let snapshot = context_window_usage(current, last_turn, Some(200_000))
        .expect("current Claude usage should produce a context snapshot");

    assert_eq!(snapshot.current.total_tokens, 10_750);
    assert_eq!(snapshot.current.output_tokens, Some(750));
    assert_eq!(snapshot.max_tokens, Some(200_000));
    assert_eq!(
        snapshot.cumulative.map(|usage| usage.scope),
        Some(ContextUsageScope::LastTurn)
    );
    assert_eq!(
        snapshot
            .cumulative
            .map(|usage| usage.breakdown.total_tokens),
        Some(37_000)
    );
}

#[test]
fn post_compaction_total_clears_category_detail() {
    let snapshot = context_window_usage(
        Some(TokenUsageBreakdown::total_only(17_000)),
        None,
        Some(200_000),
    )
    .expect("post-compaction total should remain visible");

    assert_eq!(snapshot.current.total_tokens, 17_000);
    assert_eq!(snapshot.current.input_tokens, None);
    assert_eq!(snapshot.current.cache_read_input_tokens, None);
    assert_eq!(snapshot.current.output_tokens, None);
    assert_eq!(snapshot.cumulative, None);
}

#[test]
fn every_claude_process_enables_sdk_file_checkpointing() {
    let mut command = Command::new("claude");
    command.env(FILE_CHECKPOINTING_ENV, "false");

    enable_file_checkpointing(&mut command);

    let value = command
        .get_envs()
        .find(|(name, _)| *name == FILE_CHECKPOINTING_ENV)
        .and_then(|(_, value)| value)
        .and_then(|value| value.to_str());
    assert_eq!(value, Some("true"));
}

#[test]
fn rewind_is_an_idle_ui_command_not_a_provider_slash_turn() {
    let commands = Session::adapter_commands();
    let rewind = commands
        .iter()
        .find(|command| command.name == "rewind")
        .expect("Claude rewind metadata");

    assert_eq!(rewind.source, SlashCommandSource::Adapter);
    assert_eq!(rewind.arguments, SlashCommandArguments::None);
    assert_eq!(rewind.run_policy, SlashCommandRunPolicy::IdleOnly);
    assert!(ui_owns_slash_command("rewind"));
    assert!(ui_owns_slash_command("/ReWiNd"));
    assert!(ui_owns_slash_command("/resume"));
    assert!(!ui_owns_slash_command("compact"));
}

#[cfg(windows)]
#[test]
fn fake_stream_json_process_never_receives_rewind_as_a_user_turn() {
    use std::env;
    use std::path::Path;
    use std::sync::mpsc;
    use std::time::Duration;

    use uuid::Uuid;

    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude/fake-stream-json.cmd");
    let log = env::temp_dir().join(format!("niumaterm-fake-claude-{}.jsonl", Uuid::new_v4()));
    let launch = LaunchConfig {
        executable: fixture.to_string_lossy().into_owned(),
        env: vec![(
            "NMT_FAKE_STREAM_LOG".to_string(),
            log.to_string_lossy().into_owned(),
        )],
        ..LaunchConfig::default()
    };
    let (messages_tx, messages_rx) = mpsc::channel();
    let mut session = Session::spawn(
        &launch,
        None,
        None,
        move |message| {
            let _ = messages_tx.send(message);
        },
        |_| {},
    )
    .expect("fake Claude process starts");
    let init = messages_rx
        .recv_timeout(Duration::from_secs(3))
        .expect("fake Claude init");
    assert!(
        session
            .process(init)
            .iter()
            .any(|event| matches!(event, Event::Ready(_)))
    );

    assert!(matches!(
        session.execute_slash_command("rewind", ""),
        SlashCommandOutcome::Rejected { .. }
    ));

    for _ in 0..50 {
        if log.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    drop(session);
    let input = fs::read_to_string(&log).expect("fake process captured stdin");
    assert!(input.contains("initialize"));
    assert!(!input.contains("/rewind"));
    assert!(!input.contains("\"type\":\"user\""));
    fs::remove_file(log).unwrap();
}

#[cfg(windows)]
#[test]
fn resumed_session_id_is_available_before_the_first_init_event() {
    use std::env;
    use std::path::Path;
    use std::time::Duration;

    use uuid::Uuid;

    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude/fake-stream-json.cmd");
    let log = env::temp_dir().join(format!("niumaterm-resume-{}.jsonl", Uuid::new_v4()));
    let launch = LaunchConfig {
        executable: fixture.to_string_lossy().into_owned(),
        env: vec![(
            "NMT_FAKE_STREAM_LOG".to_string(),
            log.to_string_lossy().into_owned(),
        )],
        ..LaunchConfig::default()
    };
    let resume_id = "70000000-0000-4000-8000-000000000000".to_string();
    let session = Session::spawn(&launch, None, Some(resume_id.clone()), |_| {}, |_| {})
        .expect("fake resumed Claude process starts");

    let published_id = session.session_id().map(str::to_owned);
    drop(session);
    for _ in 0..50 {
        if log.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    if log.exists() {
        fs::remove_file(log).unwrap();
    }

    assert_eq!(published_id, Some(resume_id));
}

#[test]
fn file_rewind_request_matches_the_sdk_control_shape() {
    assert_eq!(
        file_rewind_request("user-message-1"),
        json!({
            "subtype": "rewind_files",
            "user_message_id": "user-message-1",
        })
    );
}

#[test]
fn file_rewind_control_response_is_correlated_by_request_id() {
    let mut pending = HashMap::from([("nmt-7".to_string(), PendingControlOperation::FileRewind)]);

    assert_eq!(
        resolve_pending_control_operation(
            &mut pending,
            &json!({"request_id": "other", "subtype": "success"})
        ),
        None
    );
    assert!(pending.contains_key("nmt-7"));
    assert_eq!(
        resolve_pending_control_operation(
            &mut pending,
            &json!({"request_id": "nmt-7", "subtype": "success"})
        ),
        Some(Event::FileRewindCompleted { error: None })
    );
    assert!(pending.is_empty());
}

#[test]
fn file_rewind_rejection_and_malformed_responses_are_nonfatal_results() {
    for (subtype, expected) in [
        ("error", "checkpoint expired"),
        (
            "unexpected",
            "Claude returned a malformed file restore response.",
        ),
    ] {
        let mut pending =
            HashMap::from([("nmt-8".to_string(), PendingControlOperation::FileRewind)]);
        let response = if subtype == "error" {
            json!({
                "request_id": "nmt-8",
                "subtype": subtype,
                "error": "checkpoint expired",
            })
        } else {
            json!({"request_id": "nmt-8", "subtype": subtype})
        };

        assert_eq!(
            resolve_pending_control_operation(&mut pending, &response),
            Some(Event::FileRewindCompleted {
                error: Some(expected.to_string()),
            })
        );
        assert!(pending.is_empty());
    }
}

#[test]
fn process_exit_fails_and_clears_pending_file_rewinds() {
    let mut pending = HashMap::from([("nmt-9".to_string(), PendingControlOperation::FileRewind)]);

    assert_eq!(
        fail_pending_control_operations(&mut pending, "Claude exited."),
        vec![Event::FileRewindCompleted {
            error: Some("Claude exited.".into()),
        }]
    );
    assert!(pending.is_empty());
}

#[test]
fn content_bearing_inputs_seed_the_card_detail() {
    let todos = input_detail(
        "TodoWrite",
        &json!({"todos": [
            {"content": "done thing", "status": "completed"},
            {"content": "next thing", "status": "pending"},
        ]}),
    );
    assert_eq!(todos.as_deref(), Some("- [x] done thing\n- [ ] next thing"));

    let plan = input_detail("ExitPlanMode", &json!({"plan": "1. do it"}));
    assert_eq!(plan.as_deref(), Some("1. do it"));

    assert_eq!(input_detail("Grep", &json!({"pattern": "x"})), None);
}

#[test]
fn edit_diff_prefixes_old_and_new_lines() {
    let diff = edit_diff("Edit", &json!({"old_string": "a\nb", "new_string": "c"}));
    assert_eq!(diff.as_deref(), Some("-a\n-b\n+c\n"));

    assert_eq!(edit_diff("Edit", &json!({})), None);
}

#[test]
fn bash_and_file_tools_map_to_dedicated_cards() {
    let bash = tool_item(
        "t1",
        "Bash",
        &json!({
            "command": "cargo check",
            "description": "Check the workspace"
        }),
    );

    assert_eq!(
        bash,
        Item::CommandExecution {
            id: "t1".into(),
            command: "cargo check".into(),
            purpose: Some("Check the workspace".into()),
            aggregated_output: None,
            status: Some("inProgress".into()),
            exit_code: None,
        }
    );

    let write = tool_item(
        "t2",
        "Write",
        &json!({"file_path": "C:\\a.txt", "content": "x"}),
    );

    assert_eq!(
        write,
        Item::FileChange {
            id: "t2".into(),
            paths: "C:\\a.txt".into(),
            diff: Some("+x\n".into()),
            status: Some("inProgress".into()),
        }
    );

    let grep = tool_item("t3", "Grep", &json!({"pattern": "foo.*bar"}));

    assert_eq!(
        grep,
        Item::Other {
            id: "t3".into(),
            kind: "Grep".into(),
            title: "foo.*bar".into(),
            output: None,
            status: Some("inProgress".into()),
        }
    );
}

#[test]
fn only_the_compaction_status_shapes_drive_progress_events() {
    let mut active = false;

    // Per-request and permission-mode notifications share the subtype.
    assert!(
        compaction_progress(&mut active, &json!({"status": "requesting"})).is_empty(),
        "an API request start is not compaction"
    );
    assert!(
        compaction_progress(
            &mut active,
            &json!({"status": null, "permissionMode": "acceptEdits"})
        )
        .is_empty(),
        "a permission-mode echo must not end a compaction"
    );
    assert!(!active);

    assert_eq!(
        compaction_progress(&mut active, &json!({"status": "compacting"})),
        vec![Event::CompactionStarted]
    );
    assert!(active);

    // The CLI re-announces the running compaction; the UI needs one edge.
    assert!(
        compaction_progress(&mut active, &json!({"status": "compacting"})).is_empty(),
        "repeat announcements are not new transitions"
    );
    // The summarization call itself reports as a request in flight.
    assert!(compaction_progress(&mut active, &json!({"status": "requesting"})).is_empty());
    assert!(active, "a request in flight must not end the compaction");

    assert_eq!(
        compaction_progress(
            &mut active,
            &json!({"status": null, "compact_result": "success"})
        ),
        vec![Event::CompactionFinished { error: None }]
    );
    assert!(!active);
}

#[test]
fn a_failed_compaction_reports_its_reason() {
    let mut active = true;

    assert_eq!(
        compaction_progress(
            &mut active,
            &json!({"status": null, "compact_result": "failed",
                "compact_error": "not enough messages to summarize"})
        ),
        vec![Event::CompactionFinished {
            error: Some("not enough messages to summarize".into())
        }]
    );
    assert!(!active);

    // A failure with no detail still has to surface as a failure.
    let mut active = true;
    let events = compaction_progress(&mut active, &json!({"compact_result": "failed"}));

    assert!(matches!(
        events.as_slice(),
        [Event::CompactionFinished { error: Some(_) }]
    ));
}

#[test]
fn initialize_model_catalog_maps_value_and_display_name() {
    let models = json!([
        {"value": "default", "displayName": "Default (recommended)", "description": "…",
         "supportedEffortLevels": ["low", "high"]},
        {"value": "opus[1m]", "displayName": "Opus with 1M context"},
        {"displayName": "no value — skipped"}
    ]);

    let parsed = parse_models(&models, None);

    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[0].model, "default");
    assert_eq!(parsed[0].display, "Default (recommended)");
    assert_eq!(parsed[0].efforts, vec!["low", "high"]);
    assert_eq!(parsed[1].model, "opus[1m]");
    assert!(parsed[1].efforts.is_empty());
}

#[test]
fn initialize_model_catalog_keeps_a_selected_custom_model() {
    let parsed = parse_models(
        &json!([{"value": "default", "displayName": "Default"}]),
        Some("claude-custom-model"),
    );

    assert_eq!(parsed[0].model, "claude-custom-model");
    assert_eq!(parsed[1].model, "default");
}

#[test]
fn initialize_uses_model_pinned_by_launch_environment() {
    let launch = LaunchConfig {
        executable: "claude".into(),
        env: vec![
            ("UNRELATED".into(), "value".into()),
            (
                ANTHROPIC_MODEL_ENV.into(),
                "claude-opus-4-8-v4-flash[1m]".into(),
            ),
        ],
        ..LaunchConfig::default()
    };

    let model = initial_ready_model(launch_model(&launch).as_deref());

    assert_eq!(model, "claude-opus-4-8-v4-flash[1m]");
}

#[test]
fn approval_descriptions_name_the_action() {
    assert_eq!(
        approval_description("Bash", &json!({"command": "rm -rf build"})),
        "Run command: `rm -rf build`"
    );
    assert_eq!(
        approval_description("Write", &json!({"file_path": "a.txt"})),
        "Edit file: a.txt"
    );
    assert_eq!(
        approval_description("mcp__github__search", &json!({"query": "is:open"})),
        "mcp__github__search: is:open"
    );
}

#[test]
fn dynamic_commands_accept_both_json_shapes_and_drop_invalid_duplicates() {
    let parsed = parse_slash_commands(&json!([
        "/Review",
        {"name": "compact", "description": "Compact it", "argumentHint": "[focus]",
         "aliases": ["summarize", "/shrink", "not valid"]},
        {"command": "/review"},
        "",
        "not valid"
    ]));

    assert_eq!(parsed.len(), 4);
    assert_eq!(parsed[0].name, "review");
    assert_eq!(parsed[1].name, "compact");
    assert_eq!(parsed[2].name, "summarize");
    assert_eq!(parsed[3].name, "shrink");
    assert_eq!(parsed[1].argument_hint.as_deref(), Some("[focus]"));
    assert_eq!(parsed[2].description, "Compact it");
    assert_eq!(parsed[1].arguments, SlashCommandArguments::Freeform);
    // A command the catalog gave no hint for still takes arguments. Skills
    // arrive this way, and rejecting them client-side made every one of them
    // unusable with input.
    assert_eq!(parsed[0].argument_hint, None);
    assert_eq!(parsed[0].arguments, SlashCommandArguments::Freeform);
    assert!(parse_slash_commands(&Value::Null).is_empty());
}

#[test]
fn initialize_commands_are_primary_and_legacy_catalogs_are_fallbacks() {
    let response = json!({
        "commands": [{"name": "plugin:review", "aliases": ["pr"]}],
        "slash_commands": ["legacy"]
    });
    let (commands, structured) = initialize_command_catalog(&response).unwrap();

    assert!(structured);
    assert_eq!(
        commands
            .iter()
            .map(|command| command.name.as_str())
            .collect::<Vec<_>>(),
        vec!["plugin:review", "pr"]
    );
    assert!(legacy_command_catalog(structured, &json!(["legacy"])).is_none());

    let (legacy, structured) =
        initialize_command_catalog(&json!({"slash_commands": ["legacy"]})).unwrap();
    assert!(!structured);
    assert_eq!(legacy[0].name, "legacy");
    assert_eq!(
        legacy_command_catalog(structured, &json!(["newer"])).unwrap()[0].name,
        "newer"
    );
    assert!(initialize_command_catalog(&json!({})).is_none());
}

#[test]
fn provider_command_text_is_not_an_ordinary_prompt_shape() {
    assert_eq!(slash_command_text("/compact", ""), "/compact");
    assert_eq!(
        slash_command_text("review", "  focus here  "),
        "/review focus here"
    );
    assert_eq!(
        claude_result_error(&json!({
            "type": "result",
            "subtype": "error_during_execution",
            "is_error": true,
            "result": "provider rejected command"
        })),
        Some("provider rejected command".into())
    );
    assert_eq!(
        claude_result_error(&json!({"subtype": "success", "is_error": false})),
        None
    );
}

#[test]
fn a_context_usage_response_becomes_a_composition_breakdown() {
    let response = json!({
        "request_id": "nmt-3",
        "subtype": "success",
        "response": {
            "categories": [
                {"name": "System prompt", "tokens": 3_200, "color": "#aabbcc"},
                {"name": "Messages", "tokens": 41_000, "color": "#ddeeff"},
                {"name": "Free space", "tokens": 0, "color": "#101010"},
                {"name": "Reserved", "tokens": 900, "color": "#202020", "isDeferred": true},
            ],
            "totalTokens": 45_100,
            "maxTokens": 155_000,
            "rawMaxTokens": 200_000,
            "autoCompactThreshold": 140_000,
        },
    });

    let mut pending = HashMap::from([(
        "nmt-3".to_string(),
        PendingControlOperation::ContextComposition,
    )]);
    let event = resolve_pending_control_operation(&mut pending, &response);

    let Some(Event::ContextCompositionUpdated(composition)) = event else {
        panic!("expected a composition update, got {event:?}");
    };
    assert_eq!(composition.used_tokens, 45_100);
    assert_eq!(composition.max_tokens, Some(155_000));
    assert_eq!(
        composition.raw_max_tokens,
        Some(200_000),
        "the model's own window is distinct from the one compaction leaves"
    );
    assert_eq!(composition.auto_compact_threshold, Some(140_000));
    assert_eq!(composition.segments.len(), 4);
    assert!(composition.segments[3].deferred);
    assert!(pending.is_empty(), "the request is no longer outstanding");
}

#[test]
fn a_failed_context_usage_request_leaves_the_previous_breakdown_alone() {
    let response = json!({
        "request_id": "nmt-3",
        "subtype": "error",
        "error": "context usage unavailable",
    });

    let mut pending = HashMap::from([(
        "nmt-3".to_string(),
        PendingControlOperation::ContextComposition,
    )]);

    // Nothing is waiting on this, and the accounting beside it is still
    // accurate, so a failure reports nothing rather than blanking the card.
    assert!(resolve_pending_control_operation(&mut pending, &response).is_none());
    assert!(pending.is_empty());
}

#[test]
fn a_composition_without_categories_is_not_published() {
    let response = json!({
        "request_id": "nmt-3",
        "subtype": "success",
        "response": {"totalTokens": 100, "categories": []},
    });

    let mut pending = HashMap::from([(
        "nmt-3".to_string(),
        PendingControlOperation::ContextComposition,
    )]);

    assert!(resolve_pending_control_operation(&mut pending, &response).is_none());
}

/// A resumed conversation replays nothing through the protocol, so no
/// assistant message has reported usage. Without the breakdown standing in,
/// the context indicator would stay hidden until the user sent a message.
#[test]
fn a_restored_window_is_filled_from_the_breakdown() {
    let composition = ContextComposition {
        segments: Vec::new(),
        used_tokens: 41_000,
        max_tokens: Some(155_000),
        raw_max_tokens: None,
        auto_compact_threshold: None,
    };

    let filled = window_from_composition(None, &composition).expect("the window is unknown");
    assert_eq!(filled.total_tokens, 41_000);
}

#[test]
fn live_accounting_is_never_replaced_by_the_breakdown() {
    let live = TokenUsageBreakdown {
        total_tokens: 12_345,
        input_tokens: Some(10_000),
        cache_read_input_tokens: Some(2_000),
        cache_write_input_tokens: None,
        output_tokens: Some(345),
        reasoning_output_tokens: None,
    };
    let composition = ContextComposition {
        segments: Vec::new(),
        used_tokens: 41_000,
        max_tokens: Some(155_000),
        raw_max_tokens: None,
        auto_compact_threshold: None,
    };

    assert!(
        window_from_composition(Some(live), &composition).is_none(),
        "a coarse total must not overwrite the per-category accounting"
    );
}

#[test]
fn an_empty_breakdown_reports_no_window() {
    let composition = ContextComposition {
        segments: Vec::new(),
        used_tokens: 0,
        max_tokens: Some(155_000),
        raw_max_tokens: None,
        auto_compact_threshold: None,
    };

    assert!(window_from_composition(None, &composition).is_none());
}

/// A resumed conversation reaches readiness through the initialize control
/// response, because the CLI withholds `system/init` until its next model
/// turn. If the breakdown were only requested from that later message, a
/// restored session would show no context until the user spoke.
#[cfg(windows)]
#[test]
fn a_resumed_session_asks_for_its_context_before_the_first_turn() {
    use std::env;
    use std::path::Path;

    use uuid::Uuid;

    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude/fake-stream-json.cmd");
    let log = env::temp_dir().join(format!("niumaterm-fake-resume-{}.jsonl", Uuid::new_v4()));
    let launch = LaunchConfig {
        executable: fixture.to_string_lossy().into_owned(),
        env: vec![(
            "NMT_FAKE_STREAM_LOG".to_string(),
            log.to_string_lossy().into_owned(),
        )],
        ..LaunchConfig::default()
    };
    let mut session = Session::spawn(
        &launch,
        None,
        Some("70000000-0000-4000-8000-000000000000".to_string()),
        |_| {},
        |_| {},
    )
    .expect("fake resumed Claude process starts");

    // The response a resumed process answers the handshake with; no
    // `system/init` has arrived and none will until a turn starts.
    let events = session.process(json!({
        "type": "control_response",
        "response": {
            "request_id": INIT_REQUEST_ID,
            "subtype": "success",
            "response": {"models": []},
        },
    }));

    assert!(events.iter().any(|event| matches!(event, Event::Ready(_))));
    // The request is recorded only once it has been written, so an
    // outstanding operation is what proves the session asked.
    assert!(
        session
            .pending_control_operations
            .values()
            .any(|operation| *operation == PendingControlOperation::ContextComposition),
        "a restored session must ask how full its window is"
    );

    drop(session);
    let _ = fs::remove_file(log);
}

/// The adapter forwards a command's arguments as its text, so an entry that
/// declares none rejects input the CLI itself accepts. These entries are only
/// a fallback for versions whose discovery payload omits the command, and a
/// fallback that is stricter than the real thing is a bug.
#[test]
fn adapter_commands_declare_the_arguments_the_cli_accepts() {
    let commands = Session::adapter_commands();
    let compact = commands
        .iter()
        .find(|command| command.name == "compact")
        .expect("compact is offered as a fallback");
    let rewind = commands
        .iter()
        .find(|command| command.name == "rewind")
        .expect("rewind is offered");

    assert_eq!(compact.arguments, SlashCommandArguments::Freeform);
    assert!(compact.argument_hint.is_some());
    assert_eq!(
        slash_command_text("compact", "focus on the API"),
        "/compact focus on the API",
        "instructions reach the CLI as part of the command"
    );

    // Rewind opens this application's own picker, so there is no text to
    // forward and nothing for arguments to mean.
    assert!(ui_owns_slash_command("rewind"));
    assert_eq!(rewind.arguments, SlashCommandArguments::None);
}

/// The catalog mixes commands worth offering with the CLI's own internal
/// entries, ones it has retired but still lists, and ones that drive its host
/// terminal session. Only the first group can be acted on from this palette.
#[test]
fn the_catalog_drops_internal_retired_and_host_owned_commands() {
    let parsed = parse_slash_commands(&json!([
        {"name": "caveman", "description": "A skill"},
        {"name": "compact", "description": "Free up context"},
        {"name": "__remote-workflow", "description": "Run the delivered workflow"},
        {"name": "agents", "description": "(removed) Ask Claude to manage subagents"},
        {"name": "extra-usage", "description": "Renamed to /usage-credits"},
        {"name": "context", "description": "Show current context usage"},
        {"name": "model", "description": "Set the AI model for Claude Code"},
        {"name": "clear", "description": "Start a new session with empty context"},
        {"name": "heapdump", "description": "Dump the JS heap to ~/Desktop"},
        {"name": "config", "description": "Set a setting by key"},
    ]));

    let names: Vec<&str> = parsed.iter().map(|command| command.name.as_str()).collect();
    assert_eq!(names, ["caveman", "compact"]);
}

#[test]
fn a_retired_marker_only_counts_at_the_start_of_a_description() {
    // A command that merely mentions the words still belongs in the palette.
    let parsed = parse_slash_commands(&json!([
        {"name": "notes", "description": "Explain why a command was (removed) upstream"},
    ]));

    assert_eq!(parsed.len(), 1);
}
