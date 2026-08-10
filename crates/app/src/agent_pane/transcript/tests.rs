mod prompt_truncation_tests {
    use gpui::px;
    use nmt_agent_utils::chat::{Compaction, CompactionTrigger, Item as SessionItem};

    use crate::agent_pane::composer::{ComposerAction, composer_action};
    use crate::agent_pane::transcript::{
        AGENT_DISCLOSURE_DETAIL_INSET, AGENT_DISCLOSURE_GAP, AGENT_DISCLOSURE_PADDING,
        AGENT_DISCLOSURE_SLOT, AgentKind, COMMAND_EXECUTION_HEADING, Status, TurnSummary,
        VIRTUAL_TRANSCRIPT_MAX_SEGMENT_BYTES, command_execution_detail, command_execution_heading,
        compaction_accounting, compaction_label, compaction_row_is_expandable, entry_copy_text,
        interrupted_status_label, is_work_row, should_show_jump_to_latest,
        should_virtualize_transcript, transcript_segments, truncated_user_prompt, turn_summary,
        worked_status_label, working_status_label,
    };

    #[test]
    fn disclosure_detail_matches_the_title_start() {
        assert_eq!(
            AGENT_DISCLOSURE_DETAIL_INSET,
            AGENT_DISCLOSURE_PADDING + AGENT_DISCLOSURE_SLOT * 2.0 + AGENT_DISCLOSURE_GAP * 2.0
        );
    }

    #[test]
    fn composer_replaces_send_with_stop_only_while_running() {
        assert_eq!(composer_action(Status::Running), ComposerAction::Stop);
        for status in [Status::Starting, Status::Idle, Status::Exited] {
            assert_eq!(composer_action(status), ComposerAction::Send);
        }
    }

    #[test]
    fn interruption_replaces_the_elapsed_turn_summary() {
        assert_eq!(turn_summary(true, Some(12)), Some(TurnSummary::Interrupted));
        assert_eq!(turn_summary(false, Some(12)), Some(TurnSummary::Worked(12)));
        assert_eq!(turn_summary(false, None), None);
    }

    #[test]
    fn working_status_adds_compact_live_output_tokens() {
        assert_eq!(working_status_label(4, None), "Working for 4s");
        assert_eq!(
            working_status_label(12, Some(1_250)),
            "Working for 12s · 1.2k tokens"
        );
    }

    #[test]
    fn worked_status_keeps_the_final_output_tokens() {
        assert_eq!(worked_status_label(8, None), "Worked for 8s");
        assert_eq!(
            worked_status_label(21, Some(12_400)),
            "Worked for 21s · 12k tokens"
        );
    }

    #[test]
    fn interrupted_status_only_adds_available_output_tokens() {
        assert_eq!(interrupted_status_label(None), "Interrupted");
        assert_eq!(
            interrupted_status_label(Some(1_250)),
            "Interrupted · 1.2k tokens"
        );
    }

    #[test]
    fn jump_to_latest_requires_hidden_content_below_the_viewport() {
        assert!(!should_show_jump_to_latest(false, None, px(0.)));
        assert!(!should_show_jump_to_latest(false, Some(true), px(200.)));
        assert!(should_show_jump_to_latest(false, Some(false), px(200.)));
        assert!(!should_show_jump_to_latest(true, Some(false), px(200.)));
        assert!(!should_show_jump_to_latest(true, None, px(200.)));
        assert!(should_show_jump_to_latest(false, None, px(200.)));
    }

    #[test]
    fn compaction_rows_name_the_trigger_and_report_only_known_numbers() {
        let full = Compaction {
            trigger: Some(CompactionTrigger::Automatic),
            pre_tokens: Some(154_000),
            post_tokens: Some(32_000),
            messages_summarized: Some(87),
            user_context: None,
            summary: None,
        };

        assert_eq!(compaction_label(&full), "Context auto-compacted");
        assert_eq!(
            compaction_accounting(&full),
            vec![
                "154k → 32k".to_string(),
                "122k freed".to_string(),
                "87 messages summarized".to_string(),
                "automatic".to_string(),
            ]
        );

        // A boundary the backend described only partially must not invent
        // zeroes for the fields it never reported.
        let sparse = Compaction {
            pre_tokens: Some(90_000),
            ..Compaction::default()
        };

        assert_eq!(compaction_label(&sparse), "Context compacted");
        assert_eq!(compaction_accounting(&sparse), vec!["from 90k".to_string()]);
        assert!(compaction_accounting(&Compaction::default()).is_empty());
    }

    #[test]
    fn a_compaction_row_is_a_divider_and_copies_its_summary() {
        let item = SessionItem::Compaction {
            id: "compaction-1".into(),
            detail: Compaction {
                trigger: Some(CompactionTrigger::Manual),
                pre_tokens: Some(120_000),
                post_tokens: Some(40_000),
                summary: Some("what happened so far".into()),
                ..Compaction::default()
            },
        };

        // Work rows collapse into "+N tool calls" runs; a structural
        // break must never be swallowed by one.
        assert!(!is_work_row(&item));
        assert_eq!(
            entry_copy_text(&item),
            "Context compacted\n120k → 40k · 80k freed · manual\n\nwhat happened so far"
        );
    }

    #[test]
    fn compaction_disclosure_matches_provider_capabilities() {
        assert!(!compaction_row_is_expandable(AgentKind::Codex));
        assert!(compaction_row_is_expandable(AgentKind::Claude));
    }

    #[test]
    fn command_tool_moves_the_full_command_and_output_into_detail() {
        assert_eq!(COMMAND_EXECUTION_HEADING, "Run Command");
        assert_eq!(
            command_execution_heading(Some("Inspect repository status")),
            "Inspect repository status"
        );
        assert_eq!(command_execution_heading(Some("  ")), "Run Command");
        assert_eq!(
            command_execution_detail("cargo test --workspace", Some("running 42 tests\nok")),
            "$ cargo test --workspace\n\nrunning 42 tests\nok"
        );
        assert_eq!(
            command_execution_detail("cargo check", None),
            "$ cargo check"
        );
    }

    #[test]
    fn shared_tool_items_keep_transcript_details_intact() {
        let item = SessionItem::Other {
            id: "tool-1".into(),
            kind: "Read".into(),
            title: "src/lib.rs".into(),
            output: Some("contents".into()),
            status: Some("completed".into()),
        };

        let SessionItem::Other {
            id,
            kind,
            title,
            output,
            status,
        } = item
        else {
            panic!("expected a tool item");
        };
        assert_eq!(id, "tool-1");
        assert_eq!(kind, "Read");
        assert_eq!(title, "src/lib.rs");
        assert_eq!(output.as_deref(), Some("contents"));
        assert_eq!(status.as_deref(), Some("completed"));
    }

    #[test]
    fn short_prompts_pass_through_and_long_ones_cut_at_boundaries() {
        assert_eq!(truncated_user_prompt("hello\nworld"), None);

        let four_lines = "line\n".repeat(4);
        let head = truncated_user_prompt(&four_lines).expect("over the line cap");
        assert_eq!(head.lines().count(), 3);
        assert!(head.ends_with('\n'));

        let exact_char_cap = "x".repeat(512);
        assert_eq!(truncated_user_prompt(&exact_char_cap), None);

        let giant_line = "\u{4f60}".repeat(3000);
        let head = truncated_user_prompt(&giant_line).expect("over the character cap");
        assert_eq!(head.chars().count(), 512);
        assert!(giant_line.is_char_boundary(head.len()));
    }

    #[test]
    fn long_code_transcripts_switch_to_virtual_rows() {
        let many_rows = "output\n".repeat(129);
        let large_single_row = "x".repeat(16 * 1024);

        assert!(should_virtualize_transcript(true, &many_rows));
        assert!(should_virtualize_transcript(true, &large_single_row));
        assert!(!should_virtualize_transcript(true, "short output"));
        assert!(!should_virtualize_transcript(false, &many_rows));
    }

    #[test]
    fn virtual_transcript_segments_preserve_rows_and_utf8_boundaries() {
        let source = format!("alpha\r\n\n{}\nend", "你".repeat(2_000));
        let segments = transcript_segments(&source);

        assert_eq!(&source[segments[0].clone()], "alpha");
        assert_eq!(&source[segments[1].clone()], "");
        assert_eq!(&source[segments.last().expect("final row").clone()], "end");
        assert!(
            segments
                .iter()
                .all(|range| source.is_char_boundary(range.start)
                    && source.is_char_boundary(range.end)
                    && range.len() <= VIRTUAL_TRANSCRIPT_MAX_SEGMENT_BYTES)
        );
        assert!(segments.iter().filter(|range| range.len() > 0).count() > 3);
    }

    #[test]
    fn virtual_transcript_keeps_one_segment_per_short_logical_row() {
        let source = "row\n".repeat(10_000);
        let segments = transcript_segments(&source);

        assert_eq!(segments.len(), 10_000);
        assert!(segments.iter().all(|range| &source[range.clone()] == "row"));
    }
}

mod read_gutter_tests {
    use crate::agent_pane::transcript::{file_extension_lang, strip_read_gutter};

    #[test]
    fn gutter_strips_only_when_every_line_matches() {
        assert_eq!(
            strip_read_gutter("     1\u{2192}fn main() {\n     2\u{2192}}").as_deref(),
            Some("fn main() {\n}\n")
        );
        assert_eq!(strip_read_gutter("plain output"), None);
        assert_eq!(strip_read_gutter("     1\u{2192}ok\nno gutter"), None);
    }

    #[test]
    fn extension_is_the_language_tag() {
        assert_eq!(file_extension_lang("C:\\src\\main.RS"), "rs");
        assert_eq!(file_extension_lang("noext"), "");
    }
}

mod fence_tests {
    use crate::agent_pane::transcript::{detect_output_language, fenced_code_block_as};

    #[test]
    fn fence_outgrows_backtick_runs_and_sniffs_language() {
        assert_eq!(
            fenced_code_block_as("plain output", detect_output_language("plain output")),
            "```\nplain output\n```"
        );
        assert_eq!(
            fenced_code_block_as("{\"key\": 1}", detect_output_language("{\"key\": 1}")),
            "```json\n{\"key\": 1}\n```"
        );
        assert_eq!(detect_output_language("diff --git a/x b/x"), "diff");

        let tricky = "text with ```` four backticks";
        let fenced = fenced_code_block_as(tricky, "");
        assert!(
            fenced.starts_with("`````\n"),
            "fence must outgrow body runs"
        );
        assert!(fenced.ends_with("\n`````"));
    }
}
