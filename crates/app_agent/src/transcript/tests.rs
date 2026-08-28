mod prompt_truncation_tests {
    use gpui::{FontFallbacks, px};
    use nmt_agent_utils::chat::{Compaction, CompactionTrigger, Item as SessionItem};

    use crate::composer::{ComposerAction, composer_action, prompt_with_response_annotations};
    use crate::settings::AgentSettings;
    use crate::transcript::{
        AGENT_DISCLOSURE_DETAIL_INSET, AGENT_DISCLOSURE_GAP, AGENT_DISCLOSURE_PADDING,
        AGENT_DISCLOSURE_SLOT, AgentKind, Status, TranscriptView, TurnSummary,
        VIRTUAL_TRANSCRIPT_MAX_SEGMENT_BYTES, command_execution_detail, command_execution_heading,
        compaction_accounting, compaction_label, compaction_row_is_expandable, elapsed_label,
        entry_copy_text, interrupted_status_label, is_work_row, last_response_label,
        should_show_jump_to_latest, should_virtualize_transcript, transcript_segments,
        truncated_user_prompt, turn_summary, worked_status_label, working_status_label,
    };

    #[test]
    fn transcript_code_style_uses_configured_font_and_size() {
        let settings = AgentSettings {
            font_fallbacks: FontFallbacks::from_fonts(vec!["Microsoft YaHei".into()]),
            ..AgentSettings::default()
        };
        let font = settings.font_with_fallbacks("JetBrains Mono".into());
        let style = TranscriptView::transcript_code_block_style(font, 12.5);
        let fallbacks = style
            .text
            .font_fallbacks
            .expect("transcript font should retain the application fallback");

        assert_eq!(style.text.font_family.as_deref(), Some("JetBrains Mono"));
        assert_eq!(style.text.font_size, Some(px(12.5).into()));
        assert_eq!(fallbacks.fallback_list(), ["Microsoft YaHei"]);
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
    fn copying_an_annotated_user_message_omits_hidden_context() {
        let submitted =
            prompt_with_response_annotations("Explain this", &["selected response text".into()]);
        let item = SessionItem::UserMessage {
            text: Some(submitted),
        };

        assert_eq!(entry_copy_text(&item), "Explain this");
    }

    #[test]
    fn interruption_replaces_the_elapsed_turn_summary() {
        assert_eq!(turn_summary(true, Some(12)), Some(TurnSummary::Interrupted));
        assert_eq!(turn_summary(false, Some(12)), Some(TurnSummary::Worked(12)));
        assert_eq!(turn_summary(false, None), None);
    }

    #[test]
    fn working_status_adds_compact_live_output_tokens() {
        assert_eq!(working_status_label(4, None, None), "Working for 4 s");
        assert_eq!(
            working_status_label(12, Some(1_250), None),
            "Working for 12 s · 1.2k tokens"
        );
    }

    #[test]
    fn a_reported_activity_leads_the_working_row() {
        // The elapsed time reads the same every second, so what changed is
        // what belongs first.
        assert_eq!(
            working_status_label(12, Some(1_250), Some("Retrying 1/2 after 429 rate limited")),
            "Retrying 1/2 after 429 rate limited · Working for 12 s · 1.2k tokens"
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
        assert!(segments.iter().filter(|range| !range.is_empty()).count() > 3);
    }

    #[test]
    fn virtual_transcript_keeps_one_segment_per_short_logical_row() {
        let source = "row\n".repeat(10_000);
        let segments = transcript_segments(&source);

        assert_eq!(segments.len(), 10_000);
        assert!(segments.iter().all(|range| &source[range.clone()] == "row"));
    }

    #[test]
    fn a_last_response_reading_speaks_a_turn_duration() {
        // Same units as "Worked for", so the two clocks in the pane agree.
        assert_eq!(last_response_label(0), "Last response: 0 s ago");
        assert_eq!(last_response_label(45), "Last response: 45 s ago");
        assert_eq!(last_response_label(90), "Last response: 1 min 30 s ago");
        assert_eq!(
            last_response_label(3_599),
            "Last response: 59 mins 59 s ago"
        );

        // Past an hour the reading stops counting: "it has been sitting" is
        // the whole answer, and the label never changes again.
        assert_eq!(
            last_response_label(3_600),
            "Last response: more than 1 hour ago"
        );
        assert_eq!(
            last_response_label(90_061),
            "Last response: more than 1 hour ago"
        );
    }
}

mod read_gutter_tests {
    use crate::transcript::{file_extension_lang, strip_read_gutter};

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
    use crate::transcript::{detect_output_language, fenced_code_block_as};

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
    use nmt_config::agent::CollapseRows;

    use crate::transcript::{AgentKind, TranscriptView};

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
                transcript.push(1, message("a", "parent reply"), Vec::new(), cx);
                transcript.expanded_rows.insert(0);
                transcript.expanded_annotations.insert(0);
                transcript.toggled_turns.insert(1);
                transcript.expanded_groups.insert(0);
                transcript.mark_interrupted(1);
            });

            child.update(cx, |transcript, cx| {
                transcript.push(1, message("b", "child reply"), Vec::new(), cx);
                assert!(
                    transcript.expanded_rows.is_empty(),
                    "row expansion belongs to one conversation"
                );
                assert!(transcript.expanded_annotations.is_empty());
                assert!(transcript.toggled_turns.is_empty());
                assert!(transcript.expanded_groups.is_empty());
                assert!(
                    !transcript.was_interrupted(1),
                    "turn accounting belongs to one conversation"
                );
            });

            // The parent keeps everything it had after the child was touched.
            parent.update(cx, |transcript, _| {
                assert!(transcript.expanded_rows.contains(&0));
                assert!(transcript.expanded_annotations.contains(&0));
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
                    transcript.push(1, message(&format!("p{index}"), "row"), Vec::new(), cx);
                }
                transcript.sync_transcript_list(transcript.build_row_specs(CollapseRows::Off));
            });
            child.update(cx, |transcript, cx| {
                transcript.push(1, message("c0", "row"), Vec::new(), cx);
                transcript.sync_transcript_list(transcript.build_row_specs(CollapseRows::Off));
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
                transcript.push(1, message("a", "parent"), Vec::new(), cx)
            });
            child.update(cx, |transcript, cx| {
                transcript.push(1, message("b", "child"), Vec::new(), cx)
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
    use nmt_config::agent::CollapseRows;

    use crate::transcript::{AgentKind, RowSpec, TranscriptView};

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
            .build_row_specs(CollapseRows::WorkAndToolCalls)
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
                transcript.push(1, prompt("open the turn"), Vec::new(), cx);
                transcript.push(1, reply("a"), Vec::new(), cx);
                transcript.push(1, prompt("steered mid-turn"), Vec::new(), cx);
                transcript.push(1, reply("b"), Vec::new(), cx);
                settle(transcript, 1, 12);

                // Folded: the work between prompt and answer is hidden, while
                // the user's own steered words stay readable above the reply.
                // The disclosure heads the rows it hides; the summary closes
                // the turn below the reply.
                assert_eq!(order(transcript), vec!["0", "fold(1)", "2", "3", "summary"]);

                transcript.toggled_turns.insert(1);
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
                transcript.push(1, prompt("ask"), Vec::new(), cx);
                transcript.push(1, reply("answer"), Vec::new(), cx);
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
                transcript.push(1, prompt("ask"), Vec::new(), cx);
                transcript.push(1, reply("a"), Vec::new(), cx);
                transcript.push(1, reply("b"), Vec::new(), cx);

                // What a resumed conversation looks like: the turn is over, but
                // the transcript file recorded no wall time for it.
                transcript.settled_turns.insert(1);

                // It folds like any settled turn, and closes after its reply
                // rather than stating a duration the session never reported.
                assert_eq!(order(transcript), vec!["0", "fold(1)", "2"]);

                transcript.toggled_turns.insert(1);
                assert_eq!(order(transcript), vec!["0", "fold(1)", "1", "2"]);
            });
        });
    }

    #[gpui::test]
    fn a_running_turn_stays_chronological(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let transcript = cx.new(|_| TranscriptView::new(AgentKind::Claude, None));

            transcript.update(cx, |transcript, cx| {
                transcript.push(1, prompt("ask"), Vec::new(), cx);
                transcript.push(1, reply("a"), Vec::new(), cx);
                transcript.push(1, reply("b"), Vec::new(), cx);

                // Nothing is hidden while the work is still happening.
                assert_eq!(order(transcript), vec!["0", "1", "2"]);
            });
        });
    }
}

/// The collapse setting has to reach a conversation the tab restored, not only
/// the turns it watched happen: a resumed turn arrives already settled, which
/// is the state the setting decides the folding of.
mod resumed_collapse_tests {
    use gpui::{AppContext as _, TestAppContext};
    use nmt_agent_utils::chat::{Item as SessionItem, ReplayItem, ReplayTurn};
    use nmt_config::agent::CollapseRows;

    use crate::transcript::{AgentKind, RowSpec, TranscriptView};

    fn replayed(items: Vec<SessionItem>) -> ReplayTurn {
        ReplayTurn {
            items: items
                .into_iter()
                .map(|item| ReplayItem { item, at: None })
                .collect(),
            seconds: None,
            output_tokens: None,
            interrupted: false,
        }
    }

    fn row_count(transcript: &TranscriptView, collapse: CollapseRows) -> usize {
        transcript
            .build_row_specs(collapse)
            .into_iter()
            .filter(|spec| matches!(spec, RowSpec::Entry { .. } | RowSpec::Work { .. }))
            .count()
    }

    #[gpui::test]
    fn a_resumed_turn_answers_to_the_collapse_setting(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let transcript = cx.new(|_| TranscriptView::new(AgentKind::Claude, None));

            transcript.update(cx, |transcript, cx| {
                transcript.append_replay(
                    1,
                    replayed(vec![
                        SessionItem::UserMessage {
                            text: Some("ask".into()),
                        },
                        SessionItem::CommandExecution {
                            id: "c1".into(),
                            command: "ls".into(),
                            purpose: None,
                            aggregated_output: None,
                            status: Some("completed".into()),
                            exit_code: Some(0),
                        },
                        SessionItem::CommandExecution {
                            id: "c2".into(),
                            command: "cat".into(),
                            purpose: None,
                            aggregated_output: None,
                            status: Some("completed".into()),
                            exit_code: Some(0),
                        },
                        SessionItem::AgentMessage {
                            id: "a".into(),
                            text: Some("answer".into()),
                        },
                    ]),
                    cx,
                );

                // Folded away: only the prompt and the answer remain.
                assert_eq!(row_count(transcript, CollapseRows::WorkAndToolCalls), 2);
                // The work returns, with its two commands behind one run line.
                assert_eq!(row_count(transcript, CollapseRows::ToolCalls), 2);
                // Every row on its own line.
                assert_eq!(row_count(transcript, CollapseRows::Off), 4);

                // Reading work inline is the point of "only tool calls", so
                // the turn carries no fold disclosure to hide it again.
                assert!(
                    !transcript
                        .build_row_specs(CollapseRows::ToolCalls)
                        .iter()
                        .any(|spec| matches!(spec, RowSpec::TurnFold { .. }))
                );
            });
        });
    }
}

/// A prompt right-clicked in the transcript has to name the same branch point
/// the backend would, and the two lists are only counted from the newest end.
mod branch_point_targeting_tests {
    use gpui::{AppContext as _, ListOffset, TestAppContext, px};
    use nmt_agent_utils::chat::{Item as SessionItem, ReplayItem, ReplayTurn};
    use nmt_config::agent::CollapseRows;

    use crate::composer::{PromptTarget, checkpoint_at_depth};
    use crate::transcript::{AgentKind, TranscriptView};

    fn user(text: &str) -> ReplayItem {
        ReplayItem {
            item: SessionItem::UserMessage {
                text: Some(text.to_string()),
            },
            at: None,
        }
    }

    fn agent(text: &str) -> ReplayItem {
        ReplayItem {
            item: SessionItem::AgentMessage {
                id: text.to_string(),
                text: Some(text.to_string()),
            },
            at: None,
        }
    }

    /// A conversation of three turns, the middle one steered mid-flight, with
    /// its turn grouping intact. Transcript indices of "first", "second",
    /// "steered" and "third" are 0, 2, 3 and 5.
    fn transcript(cx: &mut gpui::App) -> gpui::Entity<TranscriptView> {
        let view = cx.new(|_| TranscriptView::new(AgentKind::Codex, None));
        view.update(cx, |view, cx| {
            for (turn, items) in [
                (1, vec![user("first"), agent("a")]),
                // "steered" shares turn 2 with the prompt that opened it, so
                // it names no cut of its own.
                (2, vec![user("second"), user("steered"), agent("b")]),
                (3, vec![user("third"), agent("c")]),
            ] {
                view.append_replay(
                    turn,
                    ReplayTurn {
                        items,
                        ..ReplayTurn::default()
                    },
                    cx,
                );
            }
        });
        view
    }

    /// Two turns where the first one ran a command, so its work folds behind
    /// a disclosure row and the prompts no longer sit at their entry indices.
    fn transcript_with_work(cx: &mut gpui::App) -> gpui::Entity<TranscriptView> {
        let view = cx.new(|_| TranscriptView::new(AgentKind::Codex, None));
        view.update(cx, |view, cx| {
            for (turn, items) in [
                (1, vec![user("first"), command("ls"), agent("a")]),
                (2, vec![user("second"), agent("b")]),
            ] {
                view.append_replay(
                    turn,
                    ReplayTurn {
                        items,
                        ..ReplayTurn::default()
                    },
                    cx,
                );
            }
        });
        view
    }

    fn command(command: &str) -> ReplayItem {
        ReplayItem {
            item: SessionItem::CommandExecution {
                id: command.to_string(),
                command: command.to_string(),
                purpose: None,
                aggregated_output: None,
                status: None,
                exit_code: Some(0),
            },
            at: None,
        }
    }

    #[gpui::test]
    fn a_prompt_is_located_by_how_many_turns_follow_it(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let view = transcript(cx);
            let index = [0, 2, 3, 5];

            view.read_with(cx, |view, _| {
                assert_eq!(
                    view.prompt_target(index[3]),
                    Some(PromptTarget {
                        prompt: "third".into(),
                        depth: 0,
                    })
                );
                assert_eq!(
                    view.prompt_target(index[1]),
                    Some(PromptTarget {
                        prompt: "second".into(),
                        depth: 1,
                    })
                );
                assert_eq!(
                    view.prompt_target(index[0]),
                    Some(PromptTarget {
                        prompt: "first".into(),
                        depth: 2,
                    })
                );
                // A message steered into a running turn shares that turn with
                // the prompt ahead of it and anchors nothing.
                assert_eq!(view.prompt_target(index[2]), None);
            })
        });
    }

    #[gpui::test]
    fn an_agent_message_names_no_branch_point(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let view = transcript(cx);

            view.read_with(cx, |view, _| assert_eq!(view.prompt_target(1), None));
        });
    }

    #[test]
    fn the_depth_indexes_the_backend_list_and_the_text_confirms_it() {
        // The branch points a backend would report for the same conversation,
        // newest first, and excluding the first prompt.
        let checkpoints = ["third".to_string(), "second".to_string()];
        let text = String::as_str;

        assert_eq!(
            checkpoint_at_depth(
                &checkpoints,
                &PromptTarget {
                    prompt: "third".into(),
                    depth: 0
                },
                text
            ),
            Some(&"third".to_string())
        );
        assert_eq!(
            checkpoint_at_depth(
                &checkpoints,
                &PromptTarget {
                    prompt: "second".into(),
                    depth: 1
                },
                text
            ),
            Some(&"second".to_string())
        );
        // The oldest prompt is past the end of a list that does not offer it.
        assert_eq!(
            checkpoint_at_depth(
                &checkpoints,
                &PromptTarget {
                    prompt: "first".into(),
                    depth: 2
                },
                text
            ),
            None
        );
    }

    /// The picker highlights a branch point; the transcript has to find the
    /// row showing it, which is not the entry's own index once folded work
    /// and disclosures sit between the prompts.
    #[gpui::test]
    fn a_branch_point_finds_the_row_showing_its_prompt(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let view = transcript_with_work(cx);

            view.update(cx, |view, _| {
                let specs = view.build_row_specs(CollapseRows::WorkAndToolCalls);
                view.sync_transcript_list(specs);

                // Rows: prompt, work disclosure, reply, prompt, reply.
                assert_eq!(
                    view.prompt_row(&PromptTarget {
                        prompt: "first".into(),
                        depth: 1,
                    }),
                    Some(0)
                );
                assert_eq!(
                    view.prompt_row(&PromptTarget {
                        prompt: "second".into(),
                        depth: 0,
                    }),
                    Some(3)
                );
                // A depth landing on a different prompt names a list the
                // transcript disagrees with, and moving to it would put the
                // user in front of a turn they did not point at.
                assert_eq!(
                    view.prompt_row(&PromptTarget {
                        prompt: "second".into(),
                        depth: 1,
                    }),
                    None
                );
            })
        });
    }

    /// Following the picker's highlight moves the transcript for the user;
    /// cancelling it has to give that position back.
    #[gpui::test]
    fn a_cancelled_picker_returns_the_reader_to_where_they_were(cx: &mut TestAppContext) {
        let first = PromptTarget {
            prompt: "first".into(),
            depth: 1,
        };

        cx.update(|cx| {
            let view = transcript_with_work(cx);

            view.update(cx, |view, cx| {
                let specs = view.build_row_specs(CollapseRows::WorkAndToolCalls);
                view.sync_transcript_list(specs);

                // Reading the live end: what is restored is the tail itself,
                // not the offset the tail happened to stand at.
                view.hold_for_picker();
                assert!(
                    view.reserve_below,
                    "a held transcript can scroll past its last row"
                );
                assert!(
                    !view.transcript_list.is_following_tail(),
                    "the hold pins the view before the reserve opens below it"
                );
                view.scroll_to_prompt(&first, false, cx);
                view.release_from_picker(cx);
                assert!(view.transcript_list.is_following_tail());
                assert!(!view.reserve_below);

                // Reading an earlier turn: that offset comes back.
                view.transcript_list.scroll_to(ListOffset {
                    item_ix: 3,
                    offset_in_item: px(0.),
                });
                view.hold_for_picker();
                view.scroll_to_prompt(&first, false, cx);
                assert_eq!(view.transcript_list.logical_scroll_top().item_ix, 0);
                view.release_from_picker(cx);
                assert_eq!(view.transcript_list.logical_scroll_top().item_ix, 3);

                // Nothing left stashed, so a later cancel cannot drag the
                // conversation back to a position from this one.
                view.release_from_picker(cx);
                assert_eq!(view.transcript_list.logical_scroll_top().item_ix, 3);
            })
        });
    }

    #[test]
    fn a_count_landing_on_another_prompt_names_nothing() {
        // The backend left a branch point out — a cut it cannot make — so the
        // depths no longer line up. The text comparison catches it rather than
        // letting the cut land a turn away from where the user pointed.
        let checkpoints = ["third".to_string(), "first".to_string()];

        assert_eq!(
            checkpoint_at_depth(
                &checkpoints,
                &PromptTarget {
                    prompt: "second".into(),
                    depth: 1
                },
                String::as_str
            ),
            None
        );
    }
}
