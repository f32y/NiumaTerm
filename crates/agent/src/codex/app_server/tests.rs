use std::sync::mpsc::channel;

use crate::codex::app_server::compaction::{
    CompactionState, compaction_completed, compaction_started,
};
use crate::codex::app_server::protocol::{
    command_purpose, initial_thread_request, parse_context_window_usage, parse_item,
    turn_start_params,
};
use crate::codex::app_server::*;
use crate::workspace::AgentWorkspace;

/// Replayed conversation with its turn grouping flattened away, for the tests
/// that assert what a thread replays rather than how it is divided.
fn replayed_items(turns: &Value) -> Vec<Item> {
    parse_replay(turns)
        .into_iter()
        .flat_map(|turn| turn.items)
        .map(|entry| entry.item)
        .collect()
}

pub(super) fn disconnected_session() -> Session {
    Session {
        host: None,
        conversation: ConversationState::default(),
        registration_id: 0,
        deliver: Arc::new(|_| {}),
        detached: false,
        control: ControlState::default(),
        next_title_generation_id: 0,
        title_generation: None,
        history_cursor: None,
        history_scope: SessionScope::default(),
        skill_refresh: SkillRefreshState::default(),
        thread_profile: ThreadProfile::default(),
        workspace: AgentWorkspace::default(),
        initial_resume: None,
        suppress_resume_replay: false,
        background: CodexTasks::default(),
        team: None,
    }
}

#[test]
fn disconnected_controls_reject_without_consuming_approval_or_switching_state() {
    let mut session = disconnected_session();

    session.conversation.thread_id = Some("parent".into());
    session.conversation.current_turn = Some("turn".into());
    session.conversation.pending_approval = Some(42);

    assert!(!session.interrupt());
    assert!(!session.respond_approval("accept"));
    assert_eq!(session.conversation.pending_approval, Some(42));
    assert!(!session.resume_thread("other"));
    assert!(!session.request_fork_checkpoints());
    assert!(
        session
            .fork_thread(&ForkAnchor::CodexThrough("turn".into()))
            .is_err()
    );
    assert_eq!(session.thread_id(), Some("parent"));
    assert!(session.control.is_empty());
}

#[test]
fn history_refresh_rejects_old_pages_and_keeps_the_latest_cursor() {
    let mut session = disconnected_session();
    let old = session.control.next_id();

    session.request_history(SessionScope::default());

    let current = session.control.next_id();

    session.request_history(SessionScope::AllDirectories);

    assert_eq!(session.history_scope, SessionScope::AllDirectories);
    assert_ne!(old, current);
    assert!(
        session
            .process(json!({"id": old, "result": {"data": [], "nextCursor": "old"}}))
            .is_empty()
    );
    assert!(session.history_cursor.is_none());
    assert!(matches!(
        session
            .process(json!({"id": current, "result": {"data": [], "nextCursor": "next"}}))
            .as_slice(),
        [Event::History(_)]
    ));

    let page = session.control.next_id();

    session.request_more_history();

    assert!(session.history_cursor.is_none());

    let refresh = session.control.next_id();

    session.request_history(SessionScope::default());

    assert!(
        session
            .process(json!({"id": page, "error": {"message": "old page"}}))
            .is_empty()
    );
    assert!(
        session
            .process(json!({"id": current, "result": {"nextCursor": "duplicate"}}))
            .is_empty()
    );

    session.process(json!({"id": refresh, "result": {"data": [], "nextCursor": "fresh"}}));

    assert_eq!(session.history_cursor.as_deref(), Some("fresh"));
}

#[test]
fn latest_thread_selection_wins_over_late_resume_and_fork_results() {
    let mut session = disconnected_session();

    session.conversation.thread_id = Some("parent".into());

    let resume = session.control.next_id();

    session.send_query(QueryKind::Resume, json!({"method": "thread/resume"}));

    let fork = session.control.next_id();

    session.send_query(QueryKind::Fork, json!({"method": "thread/fork"}));

    let latest = session.control.next_id();

    session.send_query(QueryKind::Resume, json!({"method": "thread/resume"}));

    for id in [resume, fork] {
        assert!(
            session
                .process(json!({"id": id, "result": {"thread": {"id": "stale", "turns": []}}}))
                .is_empty()
        );
        assert!(
            session
                .process(json!({"id": id, "error": {"message": "stale"}}))
                .is_empty()
        );
        assert_eq!(session.thread_id(), Some("parent"));
    }

    session.process(json!({"id": latest, "result": {"thread": {"id": "chosen", "turns": []}}}));

    assert_eq!(session.thread_id(), Some("chosen"));
    assert!(
        session
            .process(json!({"id": latest, "error": {"message": "duplicate"}}))
            .is_empty()
    );
}

#[test]
fn checkpoint_refresh_and_thread_switch_invalidate_old_results() {
    let mut session = disconnected_session();

    session.conversation.thread_id = Some("parent".into());

    let old = session.control.next_id();

    session.send_query(QueryKind::Checkpoints, json!({"method": "thread/read"}));

    let current = session.control.next_id();

    session.send_query(QueryKind::Checkpoints, json!({"method": "thread/read"}));

    assert!(
        session
            .process(json!({"id": old, "error": {"message": "stale"}}))
            .is_empty()
    );

    let resume = session.control.next_id();

    session.send_query(QueryKind::Resume, json!({"method": "thread/resume"}));
    session.process(json!({"id": resume, "result": {"thread": {"id": "new", "turns": []}}}));

    assert!(
        session
            .process(json!({"id": current, "result": {"thread": {"turns": []}}}))
            .is_empty()
    );
}

#[test]
fn rejected_initial_requests_keep_their_fatal_error_semantics() {
    for kind in [QueryKind::Start, QueryKind::Resume] {
        let mut session = disconnected_session();

        session.initial_resume = Some("retained".into());

        let (tx, rx) = channel();

        session.deliver = Arc::new(move |message| {
            let _ = tx.send(message);
        });

        let request = initial_thread_request(
            session.initial_resume.as_deref(),
            &session.thread_profile,
            &session.workspace,
        );

        session.send_query(kind, request);

        assert!(matches!(
            session.process(rx.try_recv().unwrap()).as_slice(),
            [Event::Error { fatal: true, .. }]
        ));
    }
}

#[test]
fn control_responses_complete_once_and_ignore_unknown_ids() {
    let mut session = disconnected_session();
    let id = session.alloc_rpc_id();

    session
        .control
        .track(id, ControlOperation::Command("compact".into()));

    session.conversation.compaction.request_manual();

    let response = json!({"id": id, "error": {"message": "busy"}});
    let events = session.process(response.clone());

    assert!(
        matches!(events.as_slice(), [Event::SlashCommandResult { name, outcome: SlashCommandOutcome::Rejected { .. } }] if name == "compact")
    );
    assert!(!session.has_active_operation());
    assert!(session.process(response).is_empty());
    assert!(
        session
            .process(json!({"id": id + 500, "error": {"message": "unknown"}}))
            .is_empty()
    );
}

#[test]
fn thread_switch_retires_commands_but_keeps_catalog_responses() {
    for kind in [QueryKind::Resume, QueryKind::Fork] {
        let mut session = disconnected_session();

        session.conversation.thread_id = Some("old".into());
        session.conversation.pending_approval = Some(42);

        let command = session.alloc_rpc_id();

        session
            .control
            .track(command, ControlOperation::Command("compact".into()));

        session.conversation.compaction.request_manual();

        let catalog = session.alloc_rpc_id();

        session.control.track(catalog, ControlOperation::Other);
        session.skill_refresh.start(catalog);

        let turn_request = session.alloc_rpc_id();

        let (id, operation) = ControlState::outgoing(
            &json!({"id": turn_request, "method": "turn/steer", "params": {"threadId": "old"}}),
        )
        .unwrap();

        session.control.track(id, operation);

        let transition = session.alloc_rpc_id();

        session.control.track_query(transition, kind);

        session
            .process(json!({"id": transition, "result": {"thread": {"id": "new", "turns": []}}}));

        assert_eq!(session.thread_id(), Some("new"));
        assert!(!session.has_active_operation());
        assert!(
            session
                .process(json!({"id": turn_request, "error": {"message": "old turn"}}))
                .is_empty()
        );

        session.conversation.compaction.request_manual();

        assert!(
            session
                .process(json!({"id": command, "error": {"message": "old failure"}}))
                .is_empty()
        );

        let completion = compaction_completed(
            &mut session.conversation.compaction,
            &json!({"id": "new-compaction"}),
        );

        assert!(completion.iter().any(|event| matches!(event, Event::ItemCompleted(Item::Compaction { detail, .. }) if detail.trigger == Some(CompactionTrigger::Manual))));

        let events = session.process(json!({"id": catalog, "result": {"data": []}}));

        assert!(matches!(events.as_slice(), [Event::Skills(_)]));
        assert!(session.skill_refresh.in_flight.is_none());
    }
}

#[test]
fn failed_thread_switch_preserves_pending_command() {
    let mut session = disconnected_session();
    let id = session.alloc_rpc_id();

    session
        .control
        .track(id, ControlOperation::Command("review".into()));

    let transition = session.alloc_rpc_id();

    session.control.track_query(transition, QueryKind::Resume);
    session.process(json!({"id": transition, "error": {"message": "missing"}}));

    assert!(session.has_active_operation());
    assert!(
        matches!(session.process(json!({"id": id, "result": {}})).as_slice(), [Event::SlashCommandResult { name, .. }] if name == "review")
    );
}

#[test]
fn host_exit_closes_requests_and_prevents_late_revival() {
    let mut session = disconnected_session();
    let id = session.alloc_rpc_id();

    session.control.track(id, ControlOperation::Other);
    session.skill_refresh.start(id);

    let events =
        session.process(json!({"method": HOST_EXIT_METHOD, "params": {"message": "stopped"}}));

    assert!(matches!(events.as_slice(), [Event::HostExited { .. }]));
    assert!(session.control.is_empty());
    assert!(session.skill_refresh.in_flight.is_none());
    assert!(
        session
            .process(json!({"id": id, "result": {"data": []}}))
            .is_empty()
    );
    assert!(
        session
            .process(json!({"id": 2, "result": {"thread": {"id": "late"}}}))
            .is_empty()
    );
    assert!(
        session
            .process(json!({"method": "turn/started", "params": {"turn": {"id": "late"}}}))
            .is_empty()
    );
    assert!(!session.has_active_operation());
}

#[test]
fn rejected_name_write_reports_failure_without_pending_state() {
    let mut session = disconnected_session();

    session.conversation.thread_id = Some("parent".into());

    let (tx, rx) = channel();

    session.deliver = Arc::new(move |message| {
        let _ = tx.send(message);
    });

    assert!(!session.rename_thread("chosen name"));
    assert!(session.control.is_empty());
    assert!(rx.try_recv().is_err());
}

#[test]
fn rejected_background_requests_settle_their_pending_state() {
    let mut session = disconnected_session();
    let (tx, rx) = channel();

    session.deliver = Arc::new(move |message| {
        let _ = tx.send(message);
    });

    session.request_skills(false);

    assert!(session.skill_refresh.in_flight.is_some());

    let events = session.process(rx.try_recv().unwrap());

    assert!(matches!(events.as_slice(), [Event::Skills(catalog)] if !catalog.errors.is_empty()));
    assert!(session.skill_refresh.in_flight.is_none());

    session.conversation.thread_id = Some("parent".into());

    assert!(matches!(
        session.execute_slash_command("compact", ""),
        SlashCommandOutcome::Rejected { .. }
    ));
    assert!(!session.control.has_command());
    assert!(!session.has_active_operation());
}

#[test]
fn disconnected_submissions_are_rejected_without_requesting_a_title() {
    let mut session = disconnected_session();

    session.conversation.thread_id = Some("parent".into());

    for turn in [None, Some("active".to_string())] {
        session.conversation.current_turn = turn.clone();

        let next_rpc_id = session.control.next_id();

        let outcome = session.send_user_message_with_generated_title(
            "keep this draft",
            &ThreadSettings::default(),
            None,
            &[],
            "draft title",
        );

        assert!(
            matches!(outcome, SendOutcome::Rejected { message } if message.contains("not connected"))
        );
        assert_eq!(session.conversation.current_turn, turn);
        assert_eq!(
            session.control.next_id(),
            next_rpc_id + 1,
            "a rejected prompt must not queue a title request"
        );
        assert!(session.title_generation.is_none());
        assert!(session.control.is_empty());
    }
}

#[test]
fn routed_child_completion_does_not_finish_the_parent_turn() {
    let mut session = disconnected_session();

    session.conversation.thread_id = Some("parent".into());
    session.background.set_root("parent");

    session.process_notification(
        "turn/started",
        &json!({"threadId":"parent","turn":{"id":"parent-turn"}}),
    );

    let spawn_events = session.process_notification("item/completed", &json!({"threadId":"parent","item":{
        "type":"collabAgentToolCall","id":"spawn","tool":"spawnAgent","status":"completed",
        "senderThreadId":"parent","receiverThreadIds":["child"],"agentsStates":{"child":{"status":"running"}}
    }}));

    assert!(matches!(
        spawn_events.first(),
        Some(Event::ItemCompleted(_))
    ));
    assert!(matches!(
        spawn_events.last(),
        Some(Event::BackgroundTasks(_))
    ));

    let child_events = session.process_notification(
        "turn/completed",
        &json!({"threadId":"child","turn":{"id":"child-turn","status":"completed"}}),
    );

    assert!(
        !child_events
            .iter()
            .any(|event| matches!(event, Event::TurnCompleted { .. }))
    );
    assert_eq!(
        session.conversation.current_turn.as_deref(),
        Some("parent-turn")
    );
    assert!(
        session
            .process_notification(
                "turn/completed",
                &json!({"threadId":"unrelated","turn":{"id":"other","status":"completed"}})
            )
            .is_empty()
    );
    assert_eq!(
        session.conversation.current_turn.as_deref(),
        Some("parent-turn")
    );
}

#[test]
fn conversation_approval_resolution_requires_its_thread_and_request() {
    let mut state = ConversationState::default();

    state.thread_id = Some("parent".into());
    state.pending_approval = Some(8);

    assert!(
        state
            .process_notification(
                "serverRequest/resolved",
                &json!({"threadId":"other","requestId":8})
            )
            .is_empty()
    );
    assert!(
        state
            .process_notification(
                "serverRequest/resolved",
                &json!({"threadId":"parent","requestId":9})
            )
            .is_empty()
    );
    assert_eq!(state.pending_approval, Some(8));
    assert!(matches!(
        state
            .process_notification(
                "serverRequest/resolved",
                &json!({"threadId":"parent","requestId":8})
            )
            .as_slice(),
        [Event::ApprovalResolved]
    ));
    assert!(state.pending_approval.is_none());
}

#[test]
fn conversation_turns_and_output_baselines_are_independent() {
    let mut first = ConversationState::default();
    let mut second = ConversationState::default();

    for state in [&mut first, &mut second] {
        assert!(matches!(
            state
                .process_notification("turn/started", &json!({"turn":{"id":"turn"}}))
                .as_slice(),
            [Event::TurnStarted, Event::ProviderTurnAccepted { id }] if id == "turn"
        ));
    }

    let usage = |total, last| json!({"turnId":"turn","tokenUsage":{"total":{"outputTokens":total,"totalTokens":total},"last":{"outputTokens":last,"totalTokens":last}}});

    first.process_notification("thread/tokenUsage/updated", &usage(80, 10));

    let second_events = second.process_notification("thread/tokenUsage/updated", &usage(5, 5));

    assert!(
        second_events
            .iter()
            .any(|event| matches!(event, Event::TurnOutputTokensUpdated(5)))
    );

    let first_events = first.process_notification("thread/tokenUsage/updated", &usage(90, 10));

    assert!(
        first_events
            .iter()
            .any(|event| matches!(event, Event::TurnOutputTokensUpdated(20)))
    );

    first.process_notification(
        "turn/completed",
        &json!({"turn":{"id":"turn","status":"completed"}}),
    );

    assert!(first.current_turn.is_none());
    assert_eq!(second.current_turn.as_deref(), Some("turn"));
}

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
    let workspace = AgentWorkspace::single(Some("C:/Repo".into()));

    assert_eq!(
        skills_list_request(10, false, &workspace),
        json!({
            "jsonrpc": "2.0",
            "id": 10,
            "method": "skills/list",
            "params": {"cwds": ["C:/Repo"]},
        })
    );
    assert_eq!(
        skills_list_request(11, true, &workspace)["params"],
        json!({"forceReload": true, "cwds": ["C:/Repo"]})
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
        codex_user_input("plain text", None, &[]),
        json!([{"type": "text", "text": "plain text"}])
    );

    let skill = SkillReference {
        name: "browser:control".into(),
        path: "C:\\skills\\browser\\SKILL.md".into(),
    };

    assert_eq!(
        codex_user_input("$browser:control inspect", Some(&skill), &[]),
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
fn local_images_follow_the_text_in_the_order_the_message_names_them() {
    let images = [r"C:\tmp\one.png".into(), r"C:\tmp\two.png".into()];

    assert_eq!(
        codex_user_input("compare [Image #1] with [Image #2]", None, &images),
        json!([
            {"type": "text", "text": "compare [Image #1] with [Image #2]"},
            {"type": "localImage", "path": r"C:\tmp\one.png"},
            {"type": "localImage", "path": r"C:\tmp\two.png"}
        ])
    );

    // A message with no images produces exactly the request shape it did
    // before images existed.
    assert_eq!(
        codex_user_input("plain text", None, &[]),
        json!([{"type": "text", "text": "plain text"}])
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
fn turn_start_sends_the_selected_approval_reviewer() {
    let settings = ThreadSettings {
        model: Some("gpt-5.6-codex".into()),
        approval: Some("on-request".into()),
        approvals_reviewer: Some("auto_review".into()),
        sandbox: Some("workspaceWrite".into()),
        effort: Some("high".into()),
        tier: None,
    };

    assert_eq!(
        turn_start_params(
            "thr_123",
            json!([{"type": "text", "text": "continue"}]),
            &settings,
            &AgentWorkspace::single(Some("C:/A".into())),
        ),
        json!({
            "threadId": "thr_123",
            "input": [{"type": "text", "text": "continue"}],
            "model": "gpt-5.6-codex",
            "approvalPolicy": "on-request",
            "approvalsReviewer": "auto_review",
            "sandboxPolicy": {"type": "workspaceWrite"},
            "effort": "high",
            "summary": "detailed",
            "serviceTier": null
        })
    );
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
        "experimentalRawEvents": true,
        "cwd": "C:/A",
        "config": {
            "model_providers.niumaterm-a1": {
                "name": "Proxy",
                "base_url": "https://proxy.example.com/v1",
                "env_key": "OPENAI_API_KEY"
            }
        }
    });

    expected["config"]["model_providers.niumaterm-a1"][PROVIDER_API_FIELD] = json!("responses");

    assert_eq!(
        thread_start_params(&profile, &AgentWorkspace::single(Some("C:/A".into()))),
        expected
    );
}

#[test]
fn a_single_directory_thread_start_carries_an_explicit_cwd() {
    let profile = ThreadProfile::default();

    assert_eq!(
        thread_start_params(&profile, &AgentWorkspace::default()),
        json!({"experimentalRawEvents": true})
    );
    assert_eq!(
        thread_start_params(&profile, &AgentWorkspace::single(Some("C:/A".into()))),
        json!({"experimentalRawEvents": true, "cwd": "C:/A"})
    );
}

#[test]
fn a_multi_directory_thread_start_names_the_primary_and_every_root() {
    let params = thread_start_params(
        &ThreadProfile::default(),
        &AgentWorkspace::new(
            Some(r"C:\Work\api".into()),
            vec![r"C:\Work\web".into(), r"D:\Shared docs".into()],
        ),
    );

    assert_eq!(params["cwd"], json!(r"C:\Work\api"));
    assert_eq!(
        params["runtimeWorkspaceRoots"],
        json!([r"C:\Work\api", r"C:\Work\web", r"D:\Shared docs"])
    );
}

#[test]
fn workspace_write_turns_list_every_selected_root() {
    let settings = ThreadSettings {
        sandbox: Some("workspaceWrite".into()),
        ..ThreadSettings::default()
    };

    let workspace = AgentWorkspace::new(Some("C:/A".into()), vec!["C:/B".into(), "C:/C".into()]);

    let params = turn_start_params("thr_1", json!([]), &settings, &workspace);

    assert_eq!(
        params["sandboxPolicy"],
        json!({
            "type": "workspaceWrite",
            "writableRoots": ["C:/A", "C:/B", "C:/C"],
        })
    );
}

#[test]
fn narrower_and_broader_sandbox_modes_keep_their_own_meaning() {
    let workspace = AgentWorkspace::new(Some("C:/A".into()), vec!["C:/B".into()]);

    // Read-only grants no write anywhere and danger-full-access already
    // exceeds the selected roots, so neither is redefined by them.
    for mode in ["readOnly", "dangerFullAccess"] {
        let settings = ThreadSettings {
            sandbox: Some(mode.into()),
            ..ThreadSettings::default()
        };

        let params = turn_start_params("thr_1", json!([]), &settings, &workspace);

        assert_eq!(params["sandboxPolicy"], json!({"type": mode}));
    }

    // A single-directory workspace-write turn stays on the bare shape.
    let settings = ThreadSettings {
        sandbox: Some("workspaceWrite".into()),
        ..ThreadSettings::default()
    };

    let params = turn_start_params(
        "thr_1",
        json!([]),
        &settings,
        &AgentWorkspace::single(Some("C:/A".into())),
    );

    assert_eq!(params["sandboxPolicy"], json!({"type": "workspaceWrite"}));
}

#[test]
fn initial_resume_never_creates_an_orphan_thread() {
    let profile = ThreadProfile::default();
    let workspace = AgentWorkspace::new(Some("C:/A".into()), vec!["C:/B".into()]);
    let resumed = initial_thread_request(Some("thr_retained"), &profile, &workspace);

    assert_eq!(resumed["method"], "thread/resume");
    assert!(resumed.get("id").is_none());
    assert_eq!(resumed["params"]["threadId"], "thr_retained");

    // A resumed thread keeps the directory the server persisted for it; its
    // access follows the current snapshot through the turns that come next.
    assert!(resumed["params"]["runtimeWorkspaceRoots"].is_null());

    let fresh = initial_thread_request(None, &profile, &workspace);

    assert_eq!(
        fresh["params"]["runtimeWorkspaceRoots"],
        json!(["C:/A", "C:/B"])
    );
    assert_eq!(fresh["method"], "thread/start");
    assert!(fresh.get("id").is_none());
}

#[test]
fn in_place_resume_suppresses_transcript_replay_but_still_becomes_ready() {
    let result = json!({
        "thread": {
            "id": "thr_retained",
            "name": "Stored conversation title",
            "turns": [{"items": [{"type": "userMessage", "content": [{"type": "text", "text": "already visible"}]}]}]
        },
        "model": "gpt-5",
        "approvalsReviewer": "auto_review"
    });

    let suppressed = resumed_thread_events(&result, true);

    assert_eq!(
        suppressed,
        vec![
            Event::TitleUpdated("Stored conversation title".into()),
            Event::Ready(ThreadSettings {
                model: Some("gpt-5".into()),
                approvals_reviewer: Some("auto_review".into()),
                ..ThreadSettings::default()
            })
        ]
    );

    let normal = resumed_thread_events(&result, false);

    assert!(matches!(&normal[0], Event::Replay(_)));
    assert!(
        matches!(&normal[1], Event::TitleUpdated(title) if title == "Stored conversation title")
    );
    assert!(matches!(&normal[2], Event::Ready(_)));
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

    let workspace = AgentWorkspace::single(Some("C:/A".into()));

    let scoped = thread_list_params(
        &profile,
        Some("next"),
        SessionScope::CurrentDirectory,
        &workspace,
    );

    assert_eq!(scoped["modelProviders"], json!(["niumaterm-a1"]));

    // The exact-match filter is what keeps other projects out; asking for
    // every directory is expressed by leaving it off.
    assert_eq!(scoped["cwd"], json!("C:/A"));
    assert_eq!(
        thread_list_params(&profile, None, SessionScope::AllDirectories, &workspace,)["cwd"],
        Value::Null
    );

    let models = parse_models(&json!({"data": []}), profile.model.as_deref());

    assert_eq!(models[0].model, "vendor/custom-model");
    assert_eq!(models.len(), 1);
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
             "commandActions": [{"type": "listFiles", "command": "ls", "path": "."}],
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
        replayed_items(&turns),
        vec![
            Item::UserMessage {
                text: Some("question".into())
            },
            Item::CommandExecution {
                id: "i2".into(),
                command: "ls".into(),
                purpose: Some("List .".into()),
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
                text: Some("answer".into()),
                questions: None,
            },
            Item::AgentMessage {
                id: "i6".into(),
                text: Some("follow-up".into()),
                questions: None,
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
fn command_actions_become_compact_purpose_labels() {
    let actions = json!([
        {"type": "search", "command": "rg main src", "query": "main", "path": "src"},
        {"type": "read", "command": "Get-Content src/main.rs", "name": "src/main.rs",
         "path": "C:\\work\\src\\main.rs"},
        {"type": "read", "command": "Get-Content src/main.rs", "name": "src/main.rs",
         "path": "C:\\work\\src\\main.rs"}
    ]);

    assert_eq!(
        command_purpose(&actions).as_deref(),
        Some("Search main in src · Read src/main.rs")
    );
    assert_eq!(
        command_purpose(&json!([
            {"type": "read", "command": "Get-Content a", "name": "a", "path": "a"},
            {"type": "unknown", "command": "cargo check"}
        ])),
        None
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
        replayed_items(&turns),
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

#[test]
fn replay_keeps_each_turns_accounting_and_failure() {
    let turns = serde_json::json!([
        {"id": "turn1", "status": "completed", "startedAt": 1_786_516_127i64,
         "durationMs": 20_900, "error": null, "items": [
            {"id": "i1", "type": "userMessage",
             "content": [{"type": "text", "text": "question"}]},
            {"id": "i2", "type": "agentMessage", "text": "answer"}
        ]},
        {"id": "turn2", "status": "interrupted", "durationMs": 935_645, "items": [
            {"id": "i3", "type": "agentMessage", "text": "partial"}
        ]},
        {"id": "turn3", "status": "failed", "error": "model overloaded", "items": [
            {"id": "i4", "type": "userMessage",
             "content": [{"type": "text", "text": "retry"}]}
        ]}
    ]);

    let replay = parse_replay(&turns);

    assert_eq!(replay.len(), 3);
    assert_eq!(replay[0].seconds, Some(20));
    assert!(!replay[0].interrupted);
    assert_eq!(replay[0].items[0].at, Some(1_786_516_127));

    assert_eq!(replay[1].seconds, Some(935));
    assert!(replay[1].interrupted);

    // A failed turn reports its reason as a turn field, so replay has to
    // materialize the row the live path builds from the same failure.
    assert_eq!(
        replay[2].items.last().map(|entry| entry.item.clone()),
        Some(Item::Error {
            text: "model overloaded".into()
        })
    );
}

#[test]
fn branch_points_pair_each_prompt_with_the_turn_ahead_of_it() {
    let turns = serde_json::json!([
        {"id": "turn1", "status": "completed", "startedAt": 1_786_516_127i64, "items": [
            {"id": "i1", "type": "userMessage",
             "content": [{"type": "text", "text": "first"}]}
        ]},
        {"id": "turn2", "status": "completed", "startedAt": 1_786_516_227i64, "items": [
            {"id": "i2", "type": "userMessage",
             "content": [{"type": "text", "text": "second"}]}
        ]},
        {"id": "turn3", "status": "completed", "startedAt": 1_786_516_327i64, "items": [
            {"id": "i3", "type": "userMessage",
             "content": [{"type": "text", "text": "third"}]}
        ]}
    ]);

    let checkpoints = parse_fork_checkpoints(&turns);

    // Newest first, and the first prompt is absent: there is no turn ahead of
    // it to keep, so branching in front of it is a new conversation.
    assert_eq!(
        checkpoints,
        vec![
            ForkCheckpoint {
                prompt: "third".into(),
                timestamp: Some("2026-08-12T06:32:07Z".into()),
                anchor: ForkAnchor::CodexThrough("turn2".into()),
            },
            ForkCheckpoint {
                prompt: "second".into(),
                timestamp: Some("2026-08-12T06:30:27Z".into()),
                anchor: ForkAnchor::CodexThrough("turn1".into()),
            },
        ]
    );
}

#[test]
fn a_branch_is_never_anchored_on_a_turn_that_did_not_finish() {
    let turns = serde_json::json!([
        {"id": "turn1", "status": "completed", "items": [
            {"id": "i1", "type": "userMessage",
             "content": [{"type": "text", "text": "first"}]}
        ]},
        {"id": "turn2", "status": "interrupted", "items": [
            {"id": "i2", "type": "userMessage",
             "content": [{"type": "text", "text": "second"}]}
        ]},
        {"id": "turn3", "status": "completed", "items": [
            {"id": "i3", "type": "userMessage",
             "content": [{"type": "text", "text": "third"}]}
        ]}
    ]);

    // The server refuses a cut through an unfinished turn, so "third" — whose
    // only anchor is the interrupted turn2 — is not offered at all.
    assert_eq!(
        parse_fork_checkpoints(&turns),
        vec![ForkCheckpoint {
            prompt: "second".into(),
            timestamp: None,
            anchor: ForkAnchor::CodexThrough("turn1".into()),
        }]
    );
}

#[test]
fn a_thread_with_one_turn_offers_no_branch_point() {
    let turns = serde_json::json!([
        {"id": "turn1", "status": "completed", "items": [
            {"id": "i1", "type": "userMessage",
             "content": [{"type": "text", "text": "only"}]}
        ]}
    ]);

    assert!(parse_fork_checkpoints(&turns).is_empty());
    assert!(parse_fork_checkpoints(&serde_json::Value::Null).is_empty());
}

#[test]
fn raw_reasoning_tokens_outrank_the_generated_recap() {
    let recap_only = json!({
        "id": "r1",
        "type": "reasoning",
        "summary": ["Checked the parser", "Fixed the offset"],
    });

    assert_eq!(
        parse_item(&recap_only),
        Some(Item::Reasoning {
            id: "r1".into(),
            summary: Some("Checked the parser\nFixed the offset".into()),
        })
    );

    let both = json!({
        "id": "r2",
        "type": "reasoning",
        "summary": ["Checked the parser"],
        "content": ["Re-reading the offset math", "It is off by one"],
    });

    assert_eq!(
        parse_item(&both),
        Some(Item::Reasoning {
            id: "r2".into(),
            summary: Some("Re-reading the offset math\nIt is off by one".into()),
        })
    );

    // An open-weights model reports raw tokens and no recap at all.
    let raw_only = json!({"id": "r3", "type": "reasoning", "content": ["Thinking aloud"]});

    assert_eq!(
        parse_item(&raw_only),
        Some(Item::Reasoning {
            id: "r3".into(),
            summary: Some("Thinking aloud".into()),
        })
    );

    // An empty content array must leave a recap that is present untouched.
    let empty_content = json!({
        "id": "r4",
        "type": "reasoning",
        "summary": ["Recap"],
        "content": [],
    });

    assert_eq!(
        parse_item(&empty_content),
        Some(Item::Reasoning {
            id: "r4".into(),
            summary: Some("Recap".into()),
        })
    );
}
