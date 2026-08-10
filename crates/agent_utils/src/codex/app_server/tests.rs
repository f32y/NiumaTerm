use crate::codex::app_server::*;

#[test]
fn turn_output_usage_tracks_growth_across_model_responses() {
    let mut usage = TurnOutputUsage::default();

    assert_eq!(usage.observe(80, 10, false), None);
    usage.begin_turn();
    assert_eq!(usage.observe(87, 7, true), Some(7));
    assert_eq!(usage.observe(92, 5, true), Some(12));
    usage.finish_turn();
    assert_eq!(usage.observe(100, 8, false), None);
}

#[test]
fn turn_output_usage_infers_a_new_thread_baseline() {
    let mut usage = TurnOutputUsage::default();

    usage.begin_turn();
    assert_eq!(usage.observe(7, 7, true), Some(7));
    assert_eq!(usage.observe(12, 5, true), Some(12));
}

fn context_usage(used_tokens: u64, max_tokens: Option<u64>) -> ContextWindowUsage {
    ContextWindowUsage {
        current: TokenUsageBreakdown::total_only(used_tokens),
        cumulative: None,
        max_tokens,
    }
}

#[test]
fn context_usage_preserves_current_and_thread_breakdowns() {
    let usage = parse_context_window_usage(&json!({
        "last": {
            "totalTokens": 41_000,
            "inputTokens": 38_000,
            "cachedInputTokens": 27_000,
            "cacheWriteInputTokens": 2_000,
            "outputTokens": 3_000,
            "reasoningOutputTokens": 1_200
        },
        "total": {
            "totalTokens": 180_000,
            "inputTokens": 167_000,
            "cachedInputTokens": 120_000,
            "cacheWriteInputTokens": 8_000,
            "outputTokens": 13_000,
            "reasoningOutputTokens": 5_000
        },
        "modelContextWindow": 258_400
    }))
    .expect("complete Codex token usage should parse");

    assert_eq!(
        usage,
        ContextWindowUsage {
            current: TokenUsageBreakdown {
                total_tokens: 41_000,
                input_tokens: Some(38_000),
                cache_read_input_tokens: Some(27_000),
                cache_write_input_tokens: Some(2_000),
                output_tokens: Some(3_000),
                reasoning_output_tokens: Some(1_200),
            },
            cumulative: Some(ScopedTokenUsage {
                scope: ContextUsageScope::Thread,
                breakdown: TokenUsageBreakdown {
                    total_tokens: 180_000,
                    input_tokens: Some(167_000),
                    cache_read_input_tokens: Some(120_000),
                    cache_write_input_tokens: Some(8_000),
                    output_tokens: Some(13_000),
                    reasoning_output_tokens: Some(5_000),
                },
            }),
            max_tokens: Some(258_400),
        }
    );
}

#[test]
fn context_usage_accepts_older_sparse_breakdowns() {
    let usage = parse_context_window_usage(&json!({
        "last": {"totalTokens": 9_000, "inputTokens": 8_500},
        "total": {"totalTokens": 21_000},
        "modelContextWindow": null
    }))
    .expect("sparse Codex token usage should parse");

    assert_eq!(usage.current.total_tokens, 9_000);
    assert_eq!(usage.current.input_tokens, Some(8_500));
    assert_eq!(usage.current.cache_write_input_tokens, None);
    assert_eq!(
        usage.cumulative.map(|scoped| scoped.breakdown),
        Some(TokenUsageBreakdown::total_only(21_000))
    );
    assert_eq!(usage.max_tokens, None);
    assert_eq!(
        parse_context_window_usage(&json!({"last": {"totalTokens": 0}})),
        None
    );
}

#[test]
fn skill_list_requests_and_refresh_state_coalesce_invalidations() {
    assert_eq!(
        skills_list_request(10, false),
        json!({
            "jsonrpc": "2.0",
            "id": 10,
            "method": "skills/list",
            "params": {},
        })
    );
    assert_eq!(
        skills_list_request(11, true)["params"],
        json!({"forceReload": true})
    );

    let mut refresh = SkillRefreshState::default();
    assert!(!refresh.queue_if_in_flight(false));
    refresh.start(10);
    assert!(refresh.queue_if_in_flight(true));
    assert!(refresh.queue_if_in_flight(true));
    assert_eq!(refresh.finish(9), None);
    assert_eq!(refresh.finish(10), Some(true));
    refresh.start(11);
    assert_eq!(refresh.finish(11), Some(false));
}

#[test]
fn skill_catalog_preserves_duplicate_names_disabled_state_and_errors() {
    let catalog = parse_skill_catalog(&json!({
        "data": [{
            "cwd": "C:\\repo",
            "skills": [
                {
                    "name": "review",
                    "description": "User review",
                    "path": "C:\\skills\\user\\SKILL.md",
                    "scope": "user",
                    "enabled": true,
                    "interface": {"displayName": "Review changes"}
                },
                {
                    "name": "review",
                    "description": "Repo review",
                    "path": "C:\\repo\\.codex\\skills\\review\\SKILL.md",
                    "scope": "repo",
                    "enabled": false
                }
            ],
            "errors": [{"path": "C:\\broken\\SKILL.md", "message": "invalid frontmatter"}]
        }]
    }));

    assert_eq!(catalog.skills.len(), 2);
    assert_eq!(catalog.skills[0].name, catalog.skills[1].name);
    assert_ne!(catalog.skills[0].path, catalog.skills[1].path);
    assert!(catalog.skills[0].enabled);
    assert!(!catalog.skills[1].enabled);
    assert_eq!(
        catalog.skills[0].display_name.as_deref(),
        Some("Review changes")
    );
    assert!(catalog.errors[0].contains("invalid frontmatter"));
}

#[test]
fn skill_catalog_rpc_errors_are_nonfatal_catalog_state() {
    let catalog = skill_catalog_from_response(&json!({
        "error": {"code": -32601, "message": "Method not found"}
    }));

    assert!(catalog.skills.is_empty());
    assert_eq!(catalog.errors.len(), 1);
    assert!(catalog.errors[0].contains("Method not found"));
}

#[test]
fn structured_skill_input_extends_the_original_text_shape() {
    assert_eq!(
        codex_user_input("plain text", None),
        json!([{"type": "text", "text": "plain text"}])
    );

    let skill = SkillReference {
        name: "browser:control".into(),
        path: "C:\\skills\\browser\\SKILL.md".into(),
    };
    assert_eq!(
        codex_user_input("$browser:control inspect", Some(&skill)),
        json!([
            {"type": "text", "text": "$browser:control inspect"},
            {
                "type": "skill",
                "name": "browser:control",
                "path": "C:\\skills\\browser\\SKILL.md"
            }
        ])
    );
}

#[test]
fn codex_advertises_the_picker_but_not_plugin_management() {
    let commands = Session::adapter_commands();
    let skills = commands
        .iter()
        .find(|command| command.name == "skills")
        .unwrap();

    assert_eq!(skills.arguments, SlashCommandArguments::Skills);
    assert!(!commands.iter().any(|command| command.name == "plugins"));
    assert!(codex_command_request(12, "thread", "skills").is_none());
}

#[test]
fn commands_render_as_string_or_joined_argv() {
    assert_eq!(stringify_command(&json!("pytest -q")), "pytest -q");
    assert_eq!(
        stringify_command(&json!(["cargo", "check", "-p", "app"])),
        "cargo check -p app"
    );
}

#[test]
fn model_catalog_keeps_visible_models_and_their_tiers() {
    let result = json!({
        "data": [
            {
                "model": "gpt-a",
                "displayName": "GPT A",
                "hidden": false,
                "serviceTiers": [{"id": "priority", "name": "Fast"}],
                "defaultServiceTier": null
            },
            {"model": "gpt-b", "displayName": "GPT B", "hidden": true}
        ]
    });

    let models = parse_models(&result, None);

    assert_eq!(models.len(), 1);
    assert_eq!(models[0].model, "gpt-a");
    assert_eq!(models[0].tiers, vec![("priority".into(), "Fast".into())]);
}

#[test]
fn thread_start_injects_profile_model_and_provider_without_a_secret() {
    let profile = ThreadProfile {
        model: Some("vendor/custom-model".into()),
        provider: Some(CodexProviderConfig {
            id: "niumaterm-a1".into(),
            name: "Proxy".into(),
            base_url: "https://proxy.example.com/v1".into(),
            api_key_env: Some("OPENAI_API_KEY".into()),
        }),
    };

    let mut expected = json!({
        "model": "vendor/custom-model",
        "modelProvider": "niumaterm-a1",
        "config": {
            "model_providers.niumaterm-a1": {
                "name": "Proxy",
                "base_url": "https://proxy.example.com/v1",
                "env_key": "OPENAI_API_KEY"
            }
        }
    });
    expected["config"]["model_providers.niumaterm-a1"][PROVIDER_API_FIELD] = json!("responses");

    assert_eq!(thread_start_params(&profile), expected);
}

#[test]
fn initial_resume_never_creates_an_orphan_thread() {
    let profile = ThreadProfile::default();
    let resumed = initial_thread_request(Some("thr_retained"), &profile);
    assert_eq!(resumed["method"], "thread/resume");
    assert_eq!(resumed["id"], THREAD_RESUME_RPC_ID);
    assert_eq!(resumed["params"]["threadId"], "thr_retained");

    let fresh = initial_thread_request(None, &profile);
    assert_eq!(fresh["method"], "thread/start");
    assert_eq!(fresh["id"], THREAD_START_RPC_ID);
}

#[test]
fn in_place_resume_suppresses_transcript_replay_but_still_becomes_ready() {
    let result = json!({
        "thread": {
            "id": "thr_retained",
            "turns": [{"items": [{"type": "userMessage", "content": [{"type": "text", "text": "already visible"}]}]}]
        },
        "model": "gpt-5"
    });
    let suppressed = resumed_thread_events(&result, true);
    assert_eq!(suppressed.len(), 1);
    assert!(
        matches!(&suppressed[0], Event::Ready(settings) if settings.model.as_deref() == Some("gpt-5"))
    );

    let normal = resumed_thread_events(&result, false);
    assert!(matches!(&normal[0], Event::Replay(_)));
    assert!(matches!(&normal[1], Event::Ready(_)));
}

#[test]
fn resume_without_profile_model_restores_the_persisted_model_and_provider() {
    let profile = ThreadProfile {
        model: None,
        provider: Some(CodexProviderConfig {
            id: "niumaterm-a1".into(),
            name: "Proxy".into(),
            base_url: "https://proxy.example.com/v1".into(),
            api_key_env: None,
        }),
    };

    let params = thread_resume_params("thr_123", &profile);

    assert_eq!(params["threadId"], "thr_123");
    assert!(params.get("model").is_none());
    assert!(params.get("modelProvider").is_none());
    assert_eq!(
        params["config"]["model_providers.niumaterm-a1"]["base_url"],
        "https://proxy.example.com/v1"
    );
}

#[test]
fn custom_profile_filters_history_and_adds_an_unknown_selected_model() {
    let profile = ThreadProfile {
        model: Some("vendor/custom-model".into()),
        provider: Some(CodexProviderConfig {
            id: "niumaterm-a1".into(),
            ..CodexProviderConfig::default()
        }),
    };
    assert_eq!(
        thread_list_params(&profile, Some("next"))["modelProviders"],
        json!(["niumaterm-a1"])
    );

    let models = parse_models(
        &json!({
            "data": [{
                "model": "gpt-default",
                "displayName": "GPT Default",
                "hidden": false
            }]
        }),
        profile.model.as_deref(),
    );

    assert_eq!(models[0].model, "vendor/custom-model");
    assert_eq!(models[1].model, "gpt-default");
}

#[test]
fn thread_summaries_skip_own_thread_and_fall_back_to_id_titles() {
    let result = json!({
        "data": [
            {"id": "thr_live", "preview": "current"},
            {"id": "thr_a", "name": "Fix tests\nacross workspace", "recencyAt": 1730831111,
             "gitInfo": {"branch": "dev"}},
            {"id": "thr_b", "preview": "", "updatedAt": 1730750000}
        ],
        "nextCursor": null
    });

    let summaries = parse_thread_summaries(&result, Some("thr_live"));

    assert_eq!(summaries.len(), 2);
    assert_eq!(summaries[0].id, "thr_a");
    assert_eq!(summaries[0].title, "Fix tests across workspace");
    assert_eq!(summaries[0].branch.as_deref(), Some("dev"));
    assert_eq!(
        summaries[0].last_active,
        UNIX_EPOCH + Duration::from_secs(1730831111)
    );
    // Empty preview falls back to an id-prefix title.
    assert_eq!(summaries[1].title, "thr_b");
}

#[test]
fn resumed_turns_replay_dialogue_and_preserve_activity_details() {
    let turns = json!([
        {"id": "turn1", "items": [
            {"id": "i1", "type": "userMessage",
             "content": [{"type": "text", "text": "question"}]},
            {"id": "i2", "type": "commandExecution", "command": "ls",
             "aggregatedOutput": "file.txt", "status": "completed", "exitCode": 0},
            {"id": "i3", "type": "reasoning", "summary": ["checked files"]},
            {"id": "i4", "type": "mcpToolCall", "server": "s", "tool": "t",
             "result": "match", "status": "completed"},
            {"id": "i5", "type": "agentMessage", "text": "answer"}
        ]},
        {"id": "turn2", "items": [
            {"id": "i6", "type": "agentMessage", "text": "follow-up"}
        ]}
    ]);

    assert_eq!(
        parse_replay(&turns),
        vec![
            Item::UserMessage {
                text: Some("question".into())
            },
            Item::CommandExecution {
                id: "i2".into(),
                command: "ls".into(),
                aggregated_output: Some("file.txt".into()),
                status: Some("completed".into()),
                exit_code: Some(0),
            },
            Item::Reasoning {
                id: "i3".into(),
                summary: Some("checked files".into()),
            },
            Item::Other {
                id: "i4".into(),
                kind: "mcpToolCall".into(),
                title: "s/t".into(),
                output: Some("match".into()),
                status: Some("completed".into()),
            },
            Item::AgentMessage {
                id: "i5".into(),
                text: Some("answer".into())
            },
            Item::AgentMessage {
                id: "i6".into(),
                text: Some("follow-up".into())
            },
        ]
    );
}

#[test]
fn unknown_items_become_titled_tool_cards() {
    let item = json!({
        "id": "call1",
        "type": "mcpToolCall",
        "server": "github",
        "tool": "search_issues",
        "status": "inProgress"
    });

    assert_eq!(
        parse_item(&item),
        Some(Item::Other {
            id: "call1".into(),
            kind: "mcpToolCall".into(),
            title: "github/search_issues".into(),
            output: None,
            status: Some("inProgress".into()),
        })
    );
}

#[test]
fn command_requests_use_dedicated_compact_and_inline_review_methods() {
    assert_eq!(
        codex_command_request(100, "thr_1", "compact"),
        Some(json!({
            "jsonrpc": "2.0",
            "id": 100,
            "method": "thread/compact/start",
            "params": {"threadId": "thr_1"},
        }))
    );
    assert_eq!(
        codex_command_request(101, "thr_1", "review"),
        Some(json!({
            "jsonrpc": "2.0",
            "id": 101,
            "method": "review/start",
            "params": {
                "threadId": "thr_1",
                "delivery": "inline",
                "target": {"type": "uncommittedChanges"},
            },
        }))
    );
    assert_eq!(codex_command_request(102, "thr_1", "unknown"), None);
    assert_eq!(
        codex_command_response("compact", None),
        SlashCommandOutcome::Accepted
    );
    assert_eq!(
        codex_command_response("review", None),
        SlashCommandOutcome::Accepted
    );
    assert_eq!(
        codex_command_response("review", Some("unsupported target")),
        SlashCommandOutcome::Rejected {
            message: "/review failed: unsupported target".into()
        }
    );
}

#[test]
fn automatic_compaction_reports_progress_and_reclaimed_context() {
    let mut state = CompactionState::default();
    state.update_usage(context_usage(230_000, Some(258_400)));

    assert_eq!(
        compaction_started(
            &mut state,
            &json!({"id": "compact-1", "type": "contextCompaction"})
        ),
        vec![Event::CompactionStarted]
    );

    state.update_usage(context_usage(17_000, Some(258_400)));

    assert_eq!(
        compaction_completed(
            &mut state,
            &json!({"id": "compact-1", "type": "contextCompaction"})
        ),
        vec![
            Event::CompactionFinished { error: None },
            Event::ItemCompleted(Item::Compaction {
                id: "compact-1".into(),
                detail: Compaction {
                    trigger: Some(CompactionTrigger::Automatic),
                    pre_tokens: Some(230_000),
                    post_tokens: Some(17_000),
                    ..Compaction::default()
                },
            }),
        ]
    );
}

#[test]
fn compaction_omits_a_post_count_without_an_observed_drop() {
    let mut state = CompactionState::default();
    state.update_usage(context_usage(90_000, None));
    compaction_started(
        &mut state,
        &json!({"id": "compact-1", "type": "contextCompaction"}),
    );
    state.update_usage(context_usage(95_000, None));

    let events = compaction_completed(
        &mut state,
        &json!({"id": "compact-1", "type": "contextCompaction"}),
    );
    let Event::ItemCompleted(Item::Compaction { detail, .. }) = &events[1] else {
        panic!("completed compaction boundary missing");
    };

    assert_eq!(detail.pre_tokens, Some(90_000));
    assert_eq!(detail.post_tokens, None);
}

#[test]
fn manual_compaction_completes_only_from_the_item_lifecycle() {
    let mut state = CompactionState::default();
    state.request_manual();

    compaction_started(
        &mut state,
        &json!({"id": "compact-manual", "type": "contextCompaction"}),
    );
    let events = compaction_completed(
        &mut state,
        &json!({"id": "compact-manual", "type": "contextCompaction"}),
    );

    assert!(matches!(
        &events[1],
        Event::ItemCompleted(Item::Compaction {
            detail: Compaction {
                trigger: Some(CompactionTrigger::Manual),
                ..
            },
            ..
        })
    ));
    assert_eq!(
        events[2],
        Event::SlashCommandResult {
            name: "compact".into(),
            outcome: SlashCommandOutcome::Completed {
                message: Some("Conversation context compacted.".into())
            },
        }
    );
}

#[test]
fn incomplete_manual_compaction_cannot_mark_a_later_auto_run_manual() {
    let mut state = CompactionState::default();
    state.request_manual();
    compaction_started(
        &mut state,
        &json!({"id": "aborted", "type": "contextCompaction"}),
    );

    state.clear_incomplete();
    compaction_started(
        &mut state,
        &json!({"id": "automatic", "type": "contextCompaction"}),
    );
    let events = compaction_completed(
        &mut state,
        &json!({"id": "automatic", "type": "contextCompaction"}),
    );

    assert!(matches!(
        &events[1],
        Event::ItemCompleted(Item::Compaction {
            detail: Compaction {
                trigger: Some(CompactionTrigger::Automatic),
                ..
            },
            ..
        })
    ));
    assert_eq!(events.len(), 2);

    let mut rejected = CompactionState::default();
    rejected.request_manual();
    rejected.reject_manual_request();
    compaction_started(
        &mut rejected,
        &json!({"id": "after-rejection", "type": "contextCompaction"}),
    );
    let events = compaction_completed(
        &mut rejected,
        &json!({"id": "after-rejection", "type": "contextCompaction"}),
    );
    assert!(matches!(
        &events[1],
        Event::ItemCompleted(Item::Compaction {
            detail: Compaction {
                trigger: Some(CompactionTrigger::Automatic),
                ..
            },
            ..
        })
    ));
}

#[test]
fn replayed_compaction_ignores_non_protocol_summary_fields() {
    let turns = json!([{"id": "turn1", "items": [
        {"id": "compact-1", "type": "contextCompaction",
         "message": "manual compact context",
         "replacementHistory": [{"type": "compaction", "encryptedContent": "opaque"}]}
    ]}]);

    assert_eq!(
        parse_replay(&turns),
        vec![Item::Compaction {
            id: "compact-1".into(),
            detail: Compaction::default(),
        }]
    );
}

#[test]
fn compaction_is_structural_while_review_lifecycle_items_remain_tools() {
    assert!(is_legacy_compaction_notification("thread/compacted"));
    assert!(!is_legacy_compaction_notification("item/completed"));

    assert_eq!(
        parse_item(&json!({"id": "compact", "type": "contextCompaction"})),
        Some(Item::Compaction {
            id: "compact".into(),
            detail: Compaction::default(),
        })
    );

    for (kind, title) in [
        ("enteredReviewMode", "Entered review mode"),
        ("exitedReviewMode", "Exited review mode"),
    ] {
        assert_eq!(
            parse_item(&json!({"id": "item", "type": kind, "status": "completed"})),
            Some(Item::Other {
                id: "item".into(),
                kind: kind.into(),
                title: title.into(),
                output: None,
                status: Some("completed".into()),
            })
        );
    }
}
