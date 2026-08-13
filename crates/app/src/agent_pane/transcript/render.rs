use nmt_i18n::i18n;

use crate::agent_pane::transcript::disclosure_row::{
    AGENT_DISCLOSURE_DETAIL_INSET, AGENT_TEXT_MEASURE_REMS, AgentDisclosureRow,
    USER_TEXT_MEASURE_REMS,
};
use crate::agent_pane::transcript::format::{interrupted_status_label, worked_status_label};
use crate::agent_pane::transcript::{
    code_transcript_format, command_execution_detail, command_execution_heading,
    compact_token_count, compaction_accounting, compaction_label, compaction_row_is_expandable,
    compaction_trigger_label, entry_copy_text, fenced_code_block_as, is_work_row,
    should_virtualize_transcript, strip_read_gutter, truncated_user_prompt, working_label,
};
use crate::agent_pane::*;

impl TranscriptView {
    /// Build the element for one visible row. Row indices come from the list
    /// element during layout/paint, resolved through the spec snapshot taken
    /// in the current render pass.
    pub(in crate::agent_pane) fn render_row(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(spec) = self.row_specs.get(ix).cloned() else {
            return div().into_any_element();
        };

        let row = match spec {
            RowSpec::Entry { index, .. } => self.render_entry_row(index, window, cx),
            RowSpec::Work { index, .. } => self.render_work_row(index, window, cx),
            RowSpec::FoldHeader {
                turn,
                seconds,
                output_tokens,
                folded,
            } => self.render_fold_header(turn, seconds, output_tokens, folded, cx),
            RowSpec::Interrupted { output_tokens, .. } => {
                self.render_interrupted_row(output_tokens, cx)
            }
            RowSpec::RunToggle {
                run_start,
                tool_count,
                expanded,
            } => self.render_run_toggle(run_start, tool_count, expanded, cx),
            RowSpec::Working { compacting } => self.render_working_row(compacting, cx),
        };

        // The pre-virtualization container spaced rows with `gap_2` inside a
        // `p_3` body; each row now carries its own horizontal inset and
        // bottom gap.
        div().w_full().px_3().pb_2().child(row).into_any_element()
    }

    /// The live progress line. While the backend is compacting it names that
    /// explicitly and spins: compaction produces no streamed output, so a bare
    /// seconds counter would read as a hung turn for as long as a minute.
    pub(in crate::agent_pane) fn render_working_row(
        &self,
        compacting: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(started) = self.working_started else {
            return div().into_any_element();
        };

        if compacting {
            let accent = cx.theme().info;

            return h_flex()
                .w_full()
                .gap_2()
                .items_center()
                .px_1()
                .child(
                    Spinner::new()
                        .icon(IconName::LoaderCircle)
                        .with_size(px(12.))
                        .color(accent),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(accent)
                        .child(i18n("agent-transcript-compacting")),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground.opacity(0.55))
                        .child(working_label(started, self.working_output_tokens)),
                )
                .into_any_element();
        }

        h_flex()
            .w_full()
            .gap_2()
            .items_center()
            .px_1()
            .child(WorkingIndicator::new(cx.theme().muted_foreground))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground.opacity(0.7))
                    .child(working_label(started, self.working_output_tokens)),
            )
            .into_any_element()
    }

    pub(in crate::agent_pane) fn render_entry_row(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let entry = &self.items[index];

        match &entry.item {
            SessionItem::UserMessage { text: Some(text) } => self.render_user_row(index, text, cx),
            SessionItem::AgentMessage {
                text: Some(text), ..
            } => self.render_agent_row(index, text.clone(), cx),
            SessionItem::Error { text } => self.render_error_row(index, text.clone(), cx),
            SessionItem::Compaction { detail, .. } => {
                let detail = detail.clone();

                self.render_compaction_row(index, detail, window, cx)
            }
            item if is_work_row(item) => self.render_work_row(index, window, cx),
            _ => div().into_any_element(),
        }
    }

    /// Hover-revealed timestamp; the row declares `.group("entry")`.
    pub(in crate::agent_pane) fn hover_stamp(&self, index: usize, cx: &mut Context<Self>) -> Div {
        div()
            .flex_none()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .invisible()
            .group_hover("entry", |this| this.visible())
            .child(self.items[index].at.clone())
    }

    pub(in crate::agent_pane) fn copy_menu(
        pane: gpui::WeakEntity<Self>,
        index: usize,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        move |menu, _, cx| {
            // Full transcript payloads can be very large. Resolve and clone the
            // text only after a right click opens the menu, keeping ordinary
            // list layout independent of the hidden message size.
            let copy_text = pane
                .read_with(cx, |pane, _| {
                    pane.items
                        .get(index)
                        .map(|entry| entry_copy_text(&entry.item))
                })
                .ok()
                .flatten();

            match copy_text {
                Some(copy_text) => menu.item(
                    PopupMenuItem::new(i18n("agent-transcript-copy")).on_click(move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copy_text.clone()));
                    }),
                ),
                None => menu,
            }
        }
    }

    /// User prompt: right-aligned quiet bubble (muted surface, no border).
    ///
    /// Oversized prompts (huge pastes) collapse to their head by default:
    /// a visible row re-lays-out its full text every frame, so an unbounded
    /// prompt would make every frame O(paste size). Expansion is an explicit
    /// per-row choice, and the right-click Copy always carries the full text.
    pub(in crate::agent_pane) fn render_user_row(
        &self,
        index: usize,
        text: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let head_len = truncated_user_prompt(text).map(str::len);
        let expanded = head_len.is_some() && self.expanded_rows.contains(&index);
        let shown = match (head_len, expanded) {
            (Some(len), false) => text[..len].to_string(),
            _ => text.to_string(),
        };
        let toggle = head_len.is_some().then(|| {
            div()
                .mt_1()
                .text_xs()
                .text_color(cx.theme().primary)
                .cursor_pointer()
                .child(if expanded {
                    i18n("agent-transcript-show-less").to_string()
                } else {
                    i18n("agent-transcript-show-full-message").to_string()
                })
                .id(("user-expand", index))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !this.expanded_rows.insert(index) {
                        this.expanded_rows.remove(&index);
                    }
                    cx.notify();
                }))
        });

        h_flex()
            .id(("entry", index))
            .group("entry")
            .w_full()
            .justify_end()
            .items_end()
            .gap_2()
            .context_menu(Self::copy_menu(cx.entity().downgrade(), index))
            .child(self.hover_stamp(index, cx))
            .child(
                div()
                    // The bubble tracks its text rather than the pane, so a
                    // long prompt wraps at the prompt measure instead of
                    // stretching a tinted block across the transcript.
                    .max_w(rems(USER_TEXT_MEASURE_REMS))
                    // The measure is a cap, not a floor: a container narrower
                    // than it — the side panels showing a child or workflow
                    // agent conversation — must still wrap the bubble. Without
                    // this the item keeps its content width and, being
                    // right-aligned, overflows past the container's left edge.
                    .min_w_0()
                    .px_3()
                    .py_2()
                    .rounded(UI_RADIUS)
                    .bg(cx.theme().muted)
                    // Plain, not markdown: the prompt is user-authored text and
                    // must render verbatim, but stays drag-selectable.
                    .child(text::TextView::plain(("user-text", index), shown).selectable(true))
                    .children(toggle),
            )
            .into_any_element()
    }

    /// Assistant reply: full-width bare markdown — no bubble, no border;
    /// alignment and surface carry the distinction.
    /// Presentation for assistant Markdown. Prose wraps at the reading
    /// measure, and the blocks that paint their own surface are held to the
    /// same width: a code block or table left full-width would run its
    /// background past the text above it, so the tint would mark a column the
    /// reader is not reading along.
    fn agent_text_style() -> TextViewStyle {
        let measure = rems(AGENT_TEXT_MEASURE_REMS);
        // Only the ceiling is set: these blocks keep their own width behaviour
        // below it, so a short snippet still fills the measure rather than
        // shrinking to a box the width of its longest line.
        let bounded = || {
            let mut style = StyleRefinement::default();
            style.max_size.width = Some(measure.into());
            style
        };

        TextViewStyle::default()
            .prose_max_width(measure)
            .code_block(bounded())
            .table(bounded())
    }

    fn markdown_view(
        id: impl Into<ElementId>,
        markdown: impl Into<SharedString>,
        cwd: Option<String>,
    ) -> text::TextView {
        text::TextView::markdown(id, markdown).on_link_click(move |target, _, cx| {
            links::open(target, cwd.as_deref().map(Path::new), cx);
        })
    }

    pub(in crate::agent_pane) fn render_agent_row(
        &self,
        index: usize,
        text: String,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        h_flex()
            .id(("entry", index))
            .group("entry")
            .w_full()
            .items_end()
            .gap_2()
            .context_menu(Self::copy_menu(cx.entity().downgrade(), index))
            .child(
                // The body stops at the reading measure plus its own padding,
                // so the timestamp that follows it sits beside the text rather
                // than being pushed out to the pane's right edge.
                div()
                    .flex_1()
                    .min_w_0()
                    .max_w(rems(AGENT_TEXT_MEASURE_REMS + 0.5))
                    .px_1()
                    .child(
                        Self::markdown_view(("agent-md", index), text, self.cwd.clone())
                            .style(Self::agent_text_style())
                            .selectable(true),
                    ),
            )
            .child(self.hover_stamp(index, cx))
            .into_any_element()
    }

    pub(in crate::agent_pane) fn render_error_row(
        &self,
        index: usize,
        text: String,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        h_flex()
            .id(("entry", index))
            .w_full()
            .context_menu(Self::copy_menu(cx.entity().downgrade(), index))
            .child(
                div()
                    .max_w(relative(0.9))
                    .px_3()
                    .py_2()
                    .rounded(UI_RADIUS)
                    .bg(cx.theme().danger.opacity(0.15))
                    .text_color(cx.theme().danger)
                    .text_sm()
                    .child(text),
            )
            .into_any_element()
    }

    /// One work-log line: chevron · type icon · heading · status.
    /// Rows with detail (command output, reasoning text) expand on click into
    /// a bounded transcript surface with its own scroll position.
    pub(in crate::agent_pane) fn render_work_row(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let cwd = self.cwd.clone();
        let (icon, heading, status, detail) = match &self.items[index].item {
            SessionItem::CommandExecution {
                command,
                purpose,
                aggregated_output,
                status,
                exit_code,
                ..
            } => {
                // Belt and braces: a non-zero exit code is a failure even if
                // the provider reported the execution as completed.
                let state = status.as_deref().unwrap_or("inProgress");
                let failed = matches!(state, "failed" | "declined")
                    || exit_code.is_some_and(|code| code != 0);
                let state = if failed { "failed" } else { state };
                let detail = command_execution_detail(command, aggregated_output.as_deref());

                (
                    IconName::SquareTerminal,
                    command_execution_heading(purpose.as_deref()).to_string(),
                    Some(state.to_string()),
                    Some(Cow::Owned(detail)),
                )
            }
            SessionItem::FileChange {
                paths,
                diff,
                status,
                ..
            } => (
                IconName::File,
                i18n("agent-transcript-edit-paths").replace("{paths}", paths),
                Some(status.as_deref().unwrap_or("inProgress").to_string()),
                diff.as_deref()
                    .filter(|diff| !diff.trim().is_empty())
                    .map(Cow::Borrowed),
            ),
            SessionItem::Other {
                kind,
                title,
                output,
                status,
                ..
            } => (
                if kind == "webSearch" {
                    IconName::Globe
                } else {
                    IconName::Settings2
                },
                if title.trim().is_empty() {
                    kind.clone()
                } else {
                    format!("{kind} {title}")
                },
                Some(status.as_deref().unwrap_or("inProgress").to_string()),
                output
                    .as_deref()
                    .filter(|output| !output.trim().is_empty())
                    .map(Cow::Borrowed),
            ),
            SessionItem::Reasoning { summary, .. } => (
                IconName::Bot,
                i18n("agent-transcript-thinking").to_string(),
                None,
                summary
                    .as_deref()
                    .filter(|text| !text.trim().is_empty())
                    .map(Cow::Borrowed),
            ),
            _ => return div().into_any_element(),
        };

        let expandable = detail.is_some();
        let expanded = expandable && self.expanded_rows.contains(&index);
        let status_label = match status.as_deref() {
            Some("failed") => i18n("agent-transcript-status-failed"),
            Some("declined") => i18n("agent-transcript-status-declined"),
            Some("completed") => i18n("agent-transcript-status-completed"),
            Some("inProgress") => i18n("agent-transcript-status-in-progress"),
            Some(status) => status,
            None => i18n("agent-transcript-no-status"),
        };
        let status_glyph = status.as_ref().map(|state| {
            let (name, color) = match state.as_str() {
                "failed" | "declined" => (IconName::CircleX, cx.theme().danger),
                "completed" => (IconName::Check, cx.theme().muted_foreground),
                _ => (IconName::Minus, cx.theme().muted_foreground.opacity(0.6)),
            };

            Icon::new(name)
                .size_3()
                .text_color(color)
                .into_any_element()
        });

        let accessible_label = format!(
            "{}. {}{}",
            heading,
            status_label,
            if expandable {
                if expanded {
                    i18n("agent-transcript-accessibility-expanded")
                } else {
                    i18n("agent-transcript-accessibility-collapsed")
                }
            } else {
                ""
            }
        );
        let mut header = AgentDisclosureRow::new(("wl-head", index), heading)
            .type_icon(icon)
            .trailing(status_glyph)
            .accessible_label(accessible_label);
        if expandable {
            header = header.expanded(expanded);
        }
        let header = header.render(cx).when(expandable, |this| {
            this.on_click(cx.listener(move |this, _, _, cx| {
                if !this.expanded_rows.insert(index) {
                    this.expanded_rows.remove(&index);
                    this.virtual_transcripts.remove(&index);
                }
                cx.notify();
            }))
        });

        v_flex()
            .id(("entry", index))
            .w_full()
            .context_menu(Self::copy_menu(cx.entity().downgrade(), index))
            .child(header)
            .children(detail.as_deref().filter(|_| expanded).map(|detail| {
                let code_format = code_transcript_format(&self.items[index].item, detail);
                let virtualized = should_virtualize_transcript(code_format.is_some(), detail);

                let body = if virtualized {
                    let (_, strip_gutter) = code_format.as_ref().expect("code format");
                    let (source, segments, widest_segment, scroll) = {
                        let state = self
                            .virtual_transcripts
                            .entry(index)
                            .or_insert_with(|| VirtualTranscriptState::new(detail, *strip_gutter));
                        state.sync(detail, *strip_gutter);
                        (
                            state.source.clone(),
                            state.segments.clone(),
                            state.widest_segment,
                            state.scroll.clone(),
                        )
                    };
                    let segment_count = segments.len();
                    let line_height = px(f32::from(cx.theme().mono_font_size) * 1.5);
                    let text_color = cx.theme().muted_foreground;
                    let list_source = source.clone();
                    let list_segments = segments.clone();

                    div()
                        .id(("wl-out", index))
                        .w_full()
                        .h(px(256.))
                        .relative()
                        .overflow_hidden()
                        .rounded(UI_RADIUS)
                        .bg(cx.theme().tokens.muted)
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_size(cx.theme().mono_font_size)
                        // The outer transcript list is behind this viewport in
                        // hit-test order. Occlusion prevents wheel input at the
                        // virtual list's limits from moving the conversation.
                        .occlude()
                        .context_menu(Self::copy_menu(cx.entity().downgrade(), index))
                        .child(
                            uniform_list(
                                SharedString::from(format!("wl-virtual-{index}")),
                                segment_count,
                                move |range, _, _| {
                                    range
                                        .filter_map(|segment_index| {
                                            let range = list_segments.get(segment_index)?.clone();
                                            Some(
                                                div()
                                                    .h(line_height)
                                                    .flex_none()
                                                    .line_height(line_height)
                                                    .whitespace_nowrap()
                                                    .text_color(text_color)
                                                    .child(list_source[range].to_string()),
                                            )
                                        })
                                        .collect::<Vec<_>>()
                                },
                            )
                            .with_width_from_item(Some(widest_segment))
                            .with_horizontal_sizing_behavior(
                                ListHorizontalSizingBehavior::Unconstrained,
                            )
                            .track_scroll(&scroll)
                            .size_full()
                            .p_3(),
                        )
                        .child(
                            div()
                                .absolute()
                                .top_0()
                                .right_0()
                                .bottom_0()
                                .w(px(16.0))
                                .child(
                                    Scrollbar::vertical(&scroll)
                                        .id(("wl-virtual-v-scrollbar", index))
                                        .scrollbar_show(ScrollbarShow::Always),
                                ),
                        )
                        .child(
                            div()
                                .absolute()
                                .left_0()
                                .right_0()
                                .bottom_0()
                                .h(px(16.0))
                                .child(
                                    Scrollbar::horizontal(&scroll)
                                        .id(("wl-virtual-h-scrollbar", index))
                                        .scrollbar_show(ScrollbarShow::Always),
                                ),
                        )
                        .into_any_element()
                } else {
                    self.virtual_transcripts.remove(&index);

                    // Small technical transcripts retain syntax highlighting;
                    // Markdown-native tool output and reasoning retain their
                    // rich formatting. The expensive fence and gutter copy are
                    // therefore bounded by the virtualization thresholds.
                    let markdown = match code_format {
                        Some((language, strip_gutter)) => {
                            let normalized = if strip_gutter {
                                strip_read_gutter(detail)
                                    .map(Cow::Owned)
                                    .unwrap_or(Cow::Borrowed(detail))
                            } else {
                                Cow::Borrowed(detail)
                            };
                            fenced_code_block_as(&normalized, language.as_ref())
                        }
                        None => detail.to_owned(),
                    };
                    let detail_scroll = window
                        .use_keyed_state(("wl-scroll", index), cx, |_, _| ScrollHandle::default())
                        .read(cx)
                        .clone();

                    div()
                        .w_full()
                        .relative()
                        .child(
                            div()
                                .id(("wl-out", index))
                                .w_full()
                                .max_h(px(256.))
                                .overflow_y_scroll()
                                .track_scroll(&detail_scroll)
                                // The virtual conversation list handles wheel input
                                // before child bubble listeners run. Occluding its
                                // earlier hitbox makes this viewport the only scroll
                                // target under the pointer, even at either limit.
                                .occlude()
                                .context_menu(Self::copy_menu(cx.entity().downgrade(), index))
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(
                                    Self::markdown_view(("wl-md", index), markdown, cwd.clone())
                                        .selectable(true),
                                ),
                        )
                        .child(
                            div()
                                .absolute()
                                .top_0()
                                .right_0()
                                .bottom_0()
                                .w(px(16.0))
                                .child(
                                    Scrollbar::vertical(&detail_scroll)
                                        .id(("wl-scrollbar", index))
                                        .scrollbar_show(ScrollbarShow::Always),
                                ),
                        )
                        .into_any_element()
                };

                div()
                    // Expanded transcript content starts at the same horizontal
                    // position as its title; inner padding would add a redundant
                    // indentation level inside an already grouped tool call.
                    .ml(px(AGENT_DISCLOSURE_DETAIL_INSET))
                    .mt_1()
                    // Expanded technical content uses the same readable
                    // measure as assistant prose instead of stretching across
                    // the remaining window width.
                    .max_w(rems(AGENT_TEXT_MEASURE_REMS))
                    .relative()
                    .child(body)
            }))
            .into_any_element()
    }

    /// A context-compaction boundary. Rendered as a divider rather than a card
    /// because it is a structural break in the conversation: the rows above it
    /// are no longer what the model sees. Expanding reveals the accounting and
    /// the summary the thread continued from when the provider exposes it.
    pub(in crate::agent_pane) fn render_compaction_row(
        &self,
        index: usize,
        detail: Compaction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let label = compaction_label(&detail);
        let preview = compaction_accounting(&detail).join(" · ");
        let expandable = compaction_row_is_expandable(self.kind);
        let expanded = expandable && self.expanded_rows.contains(&index);
        let accent = cx.theme().info;

        let mut header_row = AgentDisclosureRow::new(("compaction-head", index), label)
            .type_icon(IconName::Minimize)
            .preview(preview.clone())
            .accent(accent);
        if expandable {
            header_row = header_row.expanded(expanded);
        }
        let accessible_label = if expandable {
            format!(
                "{label}. {preview}. {}",
                if expanded {
                    i18n("agent-transcript-expanded")
                } else {
                    i18n("agent-transcript-collapsed")
                }
            )
        } else {
            format!("{label}. {preview}")
        };
        let mut header = header_row.accessible_label(accessible_label).render(cx);
        if expandable {
            header = header.on_click(cx.listener(move |this, _, _, cx| {
                if !this.expanded_rows.insert(index) {
                    this.expanded_rows.remove(&index);
                }
                cx.notify();
            }));
        }

        v_flex()
            .id(("entry", index))
            .w_full()
            .gap_1()
            .context_menu(Self::copy_menu(cx.entity().downgrade(), index))
            // The rule sits above the heading: it closes off the conversation
            // that the summary below replaced.
            .child(div().w_full().h(px(1.)).bg(accent.opacity(0.35)))
            .child(header)
            .children(expanded.then(|| self.render_compaction_detail(index, &detail, window, cx)))
            .into_any_element()
    }

    /// Expanded compaction body: the accounting as labelled rows, the manual
    /// instructions when the user gave any, and the summary itself in a bounded
    /// scroll surface (summaries routinely run to several kilobytes).
    pub(in crate::agent_pane) fn render_compaction_detail(
        &self,
        index: usize,
        detail: &Compaction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let accounting_rows = [
            (
                i18n("agent-transcript-before"),
                detail.pre_tokens.map(compact_token_count),
            ),
            (
                i18n("agent-transcript-after"),
                detail.post_tokens.map(compact_token_count),
            ),
            (
                i18n("agent-transcript-messages-summarized"),
                detail.messages_summarized.map(|count| count.to_string()),
            ),
            (
                i18n("agent-transcript-trigger"),
                detail
                    .trigger
                    .map(|trigger| compaction_trigger_label(trigger).to_string()),
            ),
            (
                i18n("agent-transcript-instructions"),
                detail.user_context.clone(),
            ),
        ];
        let summary_scroll = window
            .use_keyed_state(("compaction-scroll", index), cx, |_, _| {
                ScrollHandle::default()
            })
            .read(cx)
            .clone();

        v_flex()
            .ml(px(AGENT_DISCLOSURE_DETAIL_INSET))
            .max_w(rems(AGENT_TEXT_MEASURE_REMS))
            .gap_1()
            .text_xs()
            .children(accounting_rows.into_iter().filter_map(|(name, value)| {
                let value = value?;

                Some(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .items_start()
                        .child(
                            div()
                                .flex_none()
                                .w(px(148.))
                                .text_color(cx.theme().muted_foreground.opacity(0.6))
                                .child(name),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_color(cx.theme().muted_foreground)
                                .child(value),
                        ),
                )
            }))
            .children(detail.summary.clone().map(|summary| {
                div()
                    .w_full()
                    .mt_1()
                    .relative()
                    .child(
                        div()
                            .id(("compaction-summary", index))
                            .w_full()
                            .max_h(px(320.))
                            .overflow_y_scroll()
                            .track_scroll(&summary_scroll)
                            // The virtual conversation list handles wheel input
                            // before child listeners run. Occluding its earlier
                            // hitbox makes this the only scroll target under the
                            // pointer, even at either limit.
                            .occlude()
                            .context_menu(Self::copy_menu(cx.entity().downgrade(), index))
                            .px_3()
                            .py_2()
                            .rounded(UI_RADIUS)
                            .bg(cx.theme().tokens.muted)
                            .text_color(cx.theme().muted_foreground)
                            .child(
                                Self::markdown_view(
                                    ("compaction-md", index),
                                    summary,
                                    self.cwd.clone(),
                                )
                                .selectable(true),
                            ),
                    )
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .right_0()
                            .bottom_0()
                            .w(px(16.0))
                            .child(
                                Scrollbar::vertical(&summary_scroll)
                                    .id(("compaction-scrollbar", index))
                                    .scrollbar_show(ScrollbarShow::Always),
                            ),
                    )
            }))
            .into_any_element()
    }

    /// The settled turn's "Worked for Ns" disclosure line, doubling as a
    /// section divider (bottom hairline).
    pub(in crate::agent_pane) fn render_fold_header(
        &self,
        turn: u64,
        seconds: u64,
        output_tokens: Option<u64>,
        folded: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let label = worked_status_label(seconds, output_tokens);

        v_flex()
            .w_full()
            .gap_1()
            .child(
                AgentDisclosureRow::new(("turn-fold", turn as usize), label.clone())
                    .expanded(!folded)
                    .accessible_label(format!(
                        "{label}. {}",
                        if folded {
                            i18n("agent-transcript-collapsed")
                        } else {
                            i18n("agent-transcript-expanded")
                        }
                    ))
                    .render(cx)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.expanded_turns.insert(turn) {
                            this.expanded_turns.remove(&turn);
                        }
                        cx.notify();
                    })),
            )
            .child(div().w_full().h(px(1.)).bg(cx.theme().border.opacity(0.6)))
            .into_any_element()
    }

    pub(in crate::agent_pane) fn render_interrupted_row(
        &self,
        output_tokens: Option<u64>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        v_flex()
            .w_full()
            .gap_1()
            .child(
                div()
                    .px_1()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground.opacity(0.7))
                    .child(interrupted_status_label(output_tokens)),
            )
            .child(div().w_full().h(px(1.)).bg(cx.theme().border.opacity(0.6)))
            .into_any_element()
    }

    /// The "+N tool calls" / "Show fewer tool calls" toggle for a work run.
    pub(in crate::agent_pane) fn render_run_toggle(
        &self,
        run_start: usize,
        tool_count: usize,
        expanded: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let label = if expanded {
            i18n("agent-transcript-show-fewer-tool-calls").to_string()
        } else {
            i18n("agent-transcript-tool-calls").replace("{count}", &tool_count.to_string())
        };

        AgentDisclosureRow::new(("wl-run", run_start), label.clone())
            .expanded(expanded)
            .without_type_icon_slot()
            .accessible_label(format!(
                "{label}. {}",
                if expanded {
                    i18n("agent-transcript-expanded")
                } else {
                    i18n("agent-transcript-collapsed")
                }
            ))
            .render(cx)
            .on_click(cx.listener(move |this, _, _, cx| {
                if !this.expanded_groups.insert(run_start) {
                    this.expanded_groups.remove(&run_start);
                }
                cx.notify();
            }))
            .into_any_element()
    }
}
