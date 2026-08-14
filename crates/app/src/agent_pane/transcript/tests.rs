mod prompt_truncation_tests {
    use gpui::px;
    use nmt_agent_utils::chat::{Compaction, CompactionTrigger, Item as SessionItem};

    use crate::agent_pane::composer::{ComposerAction, composer_action};
    use crate::agent_pane::transcript::{
        AGENT_DISCLOSURE_DETAIL_INSET, AGENT_DISCLOSURE_GAP, AGENT_DISCLOSURE_PADDING,
        AGENT_DISCLOSURE_SLOT, AGENT_TEXT_MEASURE_REMS, AgentKind, Status, TurnSummary,
        USER_TEXT_MEASURE_REMS, VIRTUAL_TRANSCRIPT_MAX_SEGMENT_BYTES, command_execution_detail,
        command_execution_heading, compaction_accounting, compaction_label,
        compaction_row_is_expandable, elapsed_label, entry_copy_text, interrupted_status_label,
        is_work_row, should_show_jump_to_latest, should_virtualize_transcript, transcript_segments,
        truncated_user_prompt, turn_summary, worked_status_label, working_status_label,
    };

    /// The reading measures are expressed in rems but chosen as character
    /// counts, so the conversion is worth stating: a monospaced glyph averages
    /// about 0.6em, making a rem roughly 1.67 characters.
    #[test]
    fn the_reading_measures_are_the_intended_line_lengths() {
        const CHARS_PER_REM: f32 = 1.0 / 0.6;

        assert_eq!((AGENT_TEXT_MEASURE_REMS * CHARS_PER_REM).round(), 80.0);
        assert_eq!((USER_TEXT_MEASURE_REMS * CHARS_PER_REM).round(), 50.0);
        assert!(
            USER_TEXT_MEASURE_REMS < AGENT_TEXT_MEASURE_REMS,
            "a prompt reads as an aside to the reply beside it"
        );
    }

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
        assert_eq!(working_status_label(4, None), "Working for 4 s");
        assert_eq!(
            working_status_label(12, Some(1_250)),
            "Working for 12 s · 1.2k tokens"
        );
    }

    #[test]
    fn elapsed_time_reads_as_a_duration_rather_than_a_seconds_count() {
        assert_eq!(elapsed_label(0), "0 s");
        assert_eq!(elapsed_label(45), "45 s");
        assert_eq!(elapsed_label(125), "2 mins 5 s");
        assert_eq!(elapsed_label(3_721), "1 hour 2 mins 1 s");
        assert_eq!(elapsed_label(90_061), "1 day 1 hour 1 min 1 s");
        assert_eq!(elapsed_label(3_605), "1 hour 5 s");
        assert_eq!(elapsed_label(86_400), "1 day");

        assert_eq!(
            worked_status_label(3_721, Some(12_400)),
            "Worked for 1 hour 2 mins 1 s · 12k tokens"
        );
    }

    #[test]
    fn worked_status_keeps_the_final_output_tokens() {
        assert_eq!(worked_status_label(8, None), "Worked for 8 s");
        assert_eq!(
            worked_status_label(21, Some(12_400)),
            "Worked for 21 s · 12k tokens"
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

/// Two conversations rendered by the same component must not share view state.
/// The Agent pane's own conversation and a child agent's conversation are both
/// `TranscriptView`s, so anything held on the type rather than per instance
/// would leak one conversation's reading position into the other.
mod separate_view_state_tests {
    use gpui::{AppContext as _, TestAppContext};
    use nmt_agent_utils::chat::Item as SessionItem;

    use crate::agent_pane::transcript::{AgentKind, TranscriptView};

    fn message(id: &str, text: &str) -> SessionItem {
        SessionItem::AgentMessage {
            id: id.into(),
            text: Some(text.into()),
        }
    }

    #[gpui::test]
    fn expansion_and_turn_accounting_stay_per_conversation(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let parent = cx.new(|_| TranscriptView::new(AgentKind::Claude, None));
            let child = cx.new(|_| TranscriptView::new(AgentKind::Claude, None));

            parent.update(cx, |transcript, cx| {
                transcript.push(1, message("a", "parent reply"), cx);
                transcript.expanded_rows.insert(0);
                transcript.expanded_turns.insert(1);
                transcript.expanded_groups.insert(0);
                transcript.mark_interrupted(1);
            });

            child.update(cx, |transcript, cx| {
                transcript.push(1, message("b", "child reply"), cx);
                assert!(
                    transcript.expanded_rows.is_empty(),
                    "row expansion belongs to one conversation"
                );
                assert!(transcript.expanded_turns.is_empty());
                assert!(transcript.expanded_groups.is_empty());
                assert!(
                    !transcript.was_interrupted(1),
                    "turn accounting belongs to one conversation"
                );
            });

            // The parent keeps everything it had after the child was touched.
            parent.update(cx, |transcript, _| {
                assert!(transcript.expanded_rows.contains(&0));
                assert!(transcript.was_interrupted(1));
                assert!(!transcript.is_empty());
            });
        });
    }

    #[gpui::test]
    fn each_conversation_measures_its_own_rows(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let parent = cx.new(|_| TranscriptView::new(AgentKind::Codex, None));
            let child = cx.new(|_| TranscriptView::new(AgentKind::Codex, None));

            parent.update(cx, |transcript, cx| {
                for index in 0..4 {
                    transcript.push(1, message(&format!("p{index}"), "row"), cx);
                }
                transcript.sync_transcript_list(transcript.build_row_specs(false));
            });
            child.update(cx, |transcript, cx| {
                transcript.push(1, message("c0", "row"), cx);
                transcript.sync_transcript_list(transcript.build_row_specs(false));
            });

            // A shared list state would report one conversation's row count for
            // both, which is what makes measured heights unusable across them.
            assert_eq!(parent.read(cx).transcript_list.item_count(), 4);
            assert_eq!(child.read(cx).transcript_list.item_count(), 1);
        });
    }

    #[gpui::test]
    fn clearing_one_conversation_leaves_the_other_intact(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let parent = cx.new(|_| TranscriptView::new(AgentKind::Claude, None));
            let child = cx.new(|_| TranscriptView::new(AgentKind::Claude, None));

            parent.update(cx, |transcript, cx| {
                transcript.push(1, message("a", "parent"), cx)
            });
            child.update(cx, |transcript, cx| {
                transcript.push(1, message("b", "child"), cx)
            });

            child.update(cx, |transcript, _| transcript.clear());

            assert!(child.read(cx).is_empty());
            assert!(!parent.read(cx).is_empty());
        });
    }
}

/// A settled turn leads with the prompt that opened it. Claude never echoes a
/// message steered into a running turn, so the pane publishes it from its own
/// queue partway through the turn; row order has to keep it where it happened
/// rather than lifting it to the head of the turn it interrupted.
mod steered_prompt_rows_tests {
    use gpui::{AppContext as _, TestAppContext};
    use nmt_agent_utils::chat::Item as SessionItem;

    use crate::agent_pane::transcript::{AgentKind, RowSpec, TranscriptView};

    fn reply(id: &str) -> SessionItem {
        SessionItem::AgentMessage {
            id: id.into(),
            text: Some(format!("reply {id}")),
        }
    }

    fn prompt(text: &str) -> SessionItem {
        SessionItem::UserMessage {
            text: Some(text.into()),
        }
    }

    /// Settle a turn the way a turn completed in this process does: finished,
    /// with the duration the session reported for it.
    fn settle(transcript: &mut TranscriptView, turn: u64, seconds: u64) {
        transcript.settled_turns.insert(turn);
        transcript.completed_turn_seconds.insert(turn, seconds);
    }

    /// Render order as entry indexes, with the turn's two chrome rows named.
    fn order(transcript: &TranscriptView) -> Vec<String> {
        transcript
            .build_row_specs(false)
            .into_iter()
            .map(|spec| match spec {
                RowSpec::Entry { index, .. } | RowSpec::Work { index, .. } => index.to_string(),
                RowSpec::TurnFold { row_count, .. } => format!("fold({row_count})"),
                RowSpec::TurnSummary { .. } => "summary".to_string(),
                _ => "?".to_string(),
            })
            .collect()
    }

    #[gpui::test]
    fn a_steered_prompt_keeps_its_place_in_the_turn(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let transcript = cx.new(|_| TranscriptView::new(AgentKind::Claude, None));

            transcript.update(cx, |transcript, cx| {
                transcript.push(1, prompt("open the turn"), cx);
                transcript.push(1, reply("a"), cx);
                transcript.push(1, prompt("steered mid-turn"), cx);
                transcript.push(1, reply("b"), cx);
                settle(transcript, 1, 12);

                // Folded: the work between prompt and answer is hidden, while
                // the user's own steered words stay readable above the reply.
                // The disclosure heads the rows it hides; the summary closes
                // the turn below the reply.
                assert_eq!(order(transcript), vec!["0", "fold(1)", "2", "3", "summary"]);

                transcript.expanded_turns.insert(1);
                assert_eq!(
                    order(transcript),
                    vec!["0", "fold(1)", "1", "2", "3", "summary"],
                    "expanding inserts the work below the disclosure, above the reply"
                );
            });
        });
    }

    #[gpui::test]
    fn a_turn_with_nothing_to_hide_gets_no_disclosure(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let transcript = cx.new(|_| TranscriptView::new(AgentKind::Claude, None));

            transcript.update(cx, |transcript, cx| {
                transcript.push(1, prompt("ask"), cx);
                transcript.push(1, reply("answer"), cx);
                settle(transcript, 1, 3);

                // A control that would disclose nothing is not rendered; the
                // summary still closes the turn.
                assert_eq!(order(transcript), vec!["0", "1", "summary"]);
            });
        });
    }

    #[gpui::test]
    fn a_replayed_turn_folds_without_claiming_a_duration(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let transcript = cx.new(|_| TranscriptView::new(AgentKind::Claude, None));

            transcript.update(cx, |transcript, cx| {
                transcript.push(1, prompt("ask"), cx);
                transcript.push(1, reply("a"), cx);
                transcript.push(1, reply("b"), cx);

                // What a resumed conversation looks like: the turn is over, but
                // the transcript file recorded no wall time for it.
                transcript.settled_turns.insert(1);

                // It folds like any settled turn, and closes after its reply
                // rather than stating a duration the session never reported.
                assert_eq!(order(transcript), vec!["0", "fold(1)", "2"]);

                transcript.expanded_turns.insert(1);
                assert_eq!(order(transcript), vec!["0", "fold(1)", "1", "2"]);
            });
        });
    }

    #[gpui::test]
    fn a_running_turn_stays_chronological(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let transcript = cx.new(|_| TranscriptView::new(AgentKind::Claude, None));

            transcript.update(cx, |transcript, cx| {
                transcript.push(1, prompt("ask"), cx);
                transcript.push(1, reply("a"), cx);
                transcript.push(1, reply("b"), cx);

                // Nothing is hidden while the work is still happening.
                assert_eq!(order(transcript), vec!["0", "1", "2"]);
            });
        });
    }
}
