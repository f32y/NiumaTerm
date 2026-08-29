use gpui::{Font, ObjectFit, img};
use gpui_component::modern_menu::{ModernMenu, ModernMenuExt as _};
use nmt_i18n::i18n;

use crate::composer::attachments::MAX_ATTACHMENTS;
use crate::composer::{annotation_count_label, parse_annotated_prompt};
use crate::transcript::disclosure_row::{
    AGENT_CARD_DETAIL_SIZE, AGENT_CARD_PADDING_X, AGENT_CARD_PADDING_Y,
    AGENT_DISCLOSURE_DETAIL_INSET, AgentCardTone, AgentDisclosureRow, USER_BUBBLE_PADDING_X,
    USER_BUBBLE_PADDING_Y, USER_BUBBLE_RADIUS, USER_BUBBLE_TAIL_RADIUS, USER_BUBBLE_WIDTH_FRACTION,
    agent_card,
};
use crate::transcript::format::{interrupted_status_label, worked_status_label};
use crate::transcript::{
    code_transcript_format, command_execution_detail, command_execution_heading,
    command_failure_reason, compact_token_count, compaction_accounting, compaction_label,
    compaction_row_is_expandable, compaction_trigger_label, entry_copy_text, fenced_code_block_as,
    is_work_row, should_virtualize_transcript, strip_read_gutter, truncated_user_prompt,
    working_label,
};
use crate::*;

/// Edge of a transcript thumbnail, matching the composer strip so an image
/// does not change size when the message it belongs to is sent.
const TRANSCRIPT_THUMBNAIL: f32 = 56.0;

/// Share of the pane the conversation column takes, and the margin on each
/// side that leaves. Expressed as a share rather than as a fixed measure
/// because a fixed one reads as a narrow strip down the middle of a wide
/// display.
pub(crate) const TRANSCRIPT_COLUMN_FRACTION: f32 = 0.8;
pub(crate) fn transcript_column_margin() -> f32 {
    (1.0 - TRANSCRIPT_COLUMN_FRACTION) / 2.0
}
/// Space below one message group, and the tighter space between the steps
/// inside a single run of work. Ranking the two is what makes a turn read as
/// message / work / message rather than as one undifferentiated stack.
const TRANSCRIPT_GROUP_GAP: f32 = 24.0;
const TRANSCRIPT_STEP_GAP: f32 = 8.0;
/// Leading for transcript text, as a multiple of the font size. Conversation
/// prose is read in paragraphs rather than scanned line by line the way
/// terminal output is, so it is set looser than the chrome around it.
pub(super) const TRANSCRIPT_LINE_HEIGHT: f32 = 1.6;

impl TranscriptView {
    /// Build the element for one visible row. Row indices come from the list
    /// element during layout/paint, resolved through the spec snapshot taken
    /// in the current render pass.
    pub(crate) fn render_row(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(spec) = self.row_specs.get(ix).cloned() else {
            return div().into_any_element();
        };

        let gap = match &spec {
            RowSpec::Work { .. } | RowSpec::RunToggle { .. } | RowSpec::TurnFold { .. } => {
                TRANSCRIPT_STEP_GAP
            }
            _ => TRANSCRIPT_GROUP_GAP,
        };

        let row = match spec {
            RowSpec::Entry { index, .. } => self.render_entry_row(index, window, cx),
            RowSpec::Work { index, .. } => self.render_work_row(index, window, cx),
            RowSpec::TurnFold {
                turn,
                row_count,
                folded,
            } => self.render_turn_fold(turn, row_count, folded, cx),
            RowSpec::TurnSummary {
                seconds,
                output_tokens,
            } => self.render_turn_summary(seconds, output_tokens, cx),
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

        // Each row is laid out on its own by the virtual list, so the reading
        // column has to be re-established per row rather than once around the
        // conversation. The margin does that here rather than a centered inner
        // box: an inner box is one more level for a width to resolve through,
        // and a row whose width goes indefinite wraps its text at the minimum
        // — one glyph per line for CJK prose.
        div()
            .w_full()
            .px(relative(transcript_column_margin()))
            .pb(px(gap))
            .child(row)
            .into_any_element()
    }

    /// The live progress line. While the backend is compacting it names that
    /// explicitly and spins: compaction produces no streamed output, so a bare
    /// seconds counter would read as a hung turn for as long as a minute.
    pub(crate) fn render_working_row(
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
                        .child(working_label(
                            started,
                            self.working_output_tokens,
                            self.working_detail.as_deref(),
                        )),
                )
                .into_any_element();
        }

        h_flex()
            .w_full()
            .gap_2()
            .items_center()
            .px_1()
            .child(
                Spinner::new()
                    .icon(IconName::LoaderCircle)
                    .with_size(px(12.))
                    .color(cx.theme().warning),
            )
            .child(
                div()
                    .text_size(px(AGENT_CARD_DETAIL_SIZE))
                    .text_color(cx.theme().muted_foreground)
                    .child(working_label(
                        started,
                        self.working_output_tokens,
                        self.working_detail.as_deref(),
                    )),
            )
            .into_any_element()
    }

    pub(crate) fn render_entry_row(
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
    pub(crate) fn hover_stamp(&self, index: usize, cx: &mut Context<Self>) -> Div {
        div()
            .flex_none()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .invisible()
            .group_hover("entry", |this| this.visible())
            .child(self.items[index].at.clone())
    }

    pub(crate) fn copy_menu(
        pane: gpui::WeakEntity<Self>,
        index: usize,
    ) -> impl Fn(ModernMenu, &mut Window, &mut App) -> ModernMenu + 'static {
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
                Some(copy_text) => menu
                    .item(i18n("agent-transcript-copy"), move |_, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copy_text.clone()));
                    })
                    .icon(IconName::Copy),
                None => menu,
            }
        }
    }

    /// The copy item plus the actions a prompt offers over the conversation
    /// it opened: branching in front of it, or returning to it.
    ///
    /// Which of the two appears follows the backend. Where a branch is a
    /// request the harness answers, the prompt names a cut and nothing else;
    /// where the conversation is a transcript file this side rewrites, the
    /// same cut also decides what happens to the files that turn touched, so
    /// the rewind actions are what the prompt leads to.
    fn user_row_menu(
        &self,
        index: usize,
        cx: &Context<Self>,
    ) -> impl Fn(ModernMenu, &mut Window, &mut App) -> ModernMenu + 'static {
        let copy = Self::copy_menu(cx.entity().downgrade(), index);
        let caps = self.kind.caps();
        // Resolved now rather than when the menu opens: a prompt's place among
        // the turns is a property of the transcript as it stands, and the rows
        // can move under a menu that is already up.
        let target = self
            .owner()
            .filter(|_| caps.session_fork || caps.file_rewind)
            .zip(self.prompt_target(index))
            .map(|(owner, target)| (owner.clone(), target));

        move |menu, window, cx| {
            let menu = copy(menu, window, cx);
            let Some((pane, target)) = target.clone() else {
                return menu;
            };

            if caps.session_fork {
                menu.separator()
                    .item(i18n("agent-transcript-fork-from-here"), move |_, cx| {
                        let target = target.clone();
                        pane.update(cx, |pane, cx| pane.fork_from_prompt(target, cx))
                            .ok();
                    })
                    .icon(IconName::GitBranch)
            } else {
                menu.separator()
                    .item(i18n("agent-transcript-rewind-to-here"), move |_, cx| {
                        let target = target.clone();
                        pane.update(cx, |pane, cx| pane.rewind_to_prompt(target, cx))
                            .ok();
                    })
                    .icon(IconName::Undo)
            }
        }
    }

    /// User prompt: right-aligned quiet bubble (muted surface, no border).
    ///
    /// Oversized prompts (huge pastes) collapse to their head by default:
    /// a visible row re-lays-out its full text every frame, so an unbounded
    /// prompt would make every frame O(paste size). Expansion is an explicit
    /// per-row choice, and the right-click Copy always carries the full text.
    pub(crate) fn render_user_row(
        &self,
        index: usize,
        text: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let parsed = parse_annotated_prompt(text);
        let text = parsed.as_ref().map_or(text, |parsed| parsed.prompt);
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

        let annotations_expanded = self.expanded_annotations.contains(&index);
        let annotations = parsed.as_ref().and_then(|parsed| {
            (!parsed.annotations.is_empty()).then(|| {
                let action_label = if annotations_expanded {
                    i18n("agent-transcript-annotations-collapse")
                } else {
                    i18n("agent-transcript-annotations-expand")
                };
                let content = annotations_expanded.then(|| {
                    v_flex()
                        .w_full()
                        .gap_2()
                        .border_t_1()
                        .border_color(cx.theme().border)
                        .px_2()
                        .py_2()
                        .children(parsed.annotations.iter().enumerate().map(
                            |(position, annotation)| {
                                h_flex()
                                    .w_full()
                                    .items_start()
                                    .gap_2()
                                    .child(
                                        div()
                                            .flex_none()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(format!("{}.", position + 1)),
                                    )
                                    .child(
                                        div().flex_1().min_w_0().child(
                                            text::TextView::plain(
                                                format!(
                                                    "entry-response-annotation-{index}-{position}"
                                                ),
                                                annotation.text.clone(),
                                            )
                                            .selectable(true),
                                        ),
                                    )
                            },
                        ))
                });

                v_flex()
                    // The bubble fills the column it shares with the prompt,
                    // which the column itself holds to the prompt measure.
                    .w_full()
                    .overflow_hidden()
                    .rounded(UI_RADIUS)
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().muted)
                    .child(
                        // The header is the whole bubble while it is collapsed,
                        // so it is built from the prompt bubble's own padding
                        // and inherited text size rather than a button size:
                        // the transcript's text size is a setting, and a
                        // control with a fixed height would stop matching the
                        // bubble below it as soon as that setting moves.
                        h_flex()
                            .id(("entry-response-annotations", index))
                            .role(gpui::Role::Button)
                            .aria_label(action_label)
                            .w_full()
                            .px_3()
                            .py_2()
                            .gap_2()
                            .items_center()
                            .cursor_pointer()
                            .hover(|style| style.bg(cx.theme().accent))
                            .child(Icon::new(IconName::TextSelect).xsmall())
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(annotation_count_label(parsed.annotations.len())),
                            )
                            .child(
                                Icon::new(if annotations_expanded {
                                    IconName::ChevronUp
                                } else {
                                    IconName::ChevronDown
                                })
                                .xsmall(),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if !this.expanded_annotations.insert(index) {
                                    this.expanded_annotations.remove(&index);
                                }
                                cx.notify();
                            })),
                    )
                    .children(content)
            })
        });
        let message = div()
            // The width cap lives on the column below, which has a definite
            // width to take a fraction of. A fraction here would resolve
            // against this bubble's own shrink-to-fit parent instead, wrapping
            // every prompt at a fraction of its natural single-line width.
            .min_w_0()
            .px(px(USER_BUBBLE_PADDING_X))
            .py(px(USER_BUBBLE_PADDING_Y))
            .rounded_tl(px(USER_BUBBLE_RADIUS))
            .rounded_tr(px(USER_BUBBLE_RADIUS))
            .rounded_bl(px(USER_BUBBLE_RADIUS))
            // The one square-ish corner faces the conversation the prompt was
            // sent into, which is what marks the bubble as this side of it.
            .rounded_br(px(USER_BUBBLE_TAIL_RADIUS))
            .bg(cx.theme().muted)
            // Plain, not markdown: the prompt is user-authored text and
            // must render verbatim, but stays drag-selectable.
            .child(text::TextView::plain(("user-text", index), shown).selectable(true))
            .children(toggle)
            .children(self.render_entry_images(index, cx));

        h_flex()
            .id(("entry", index))
            .group("entry")
            .w_full()
            .justify_end()
            .items_end()
            .gap_2()
            .modern_context_menu(self.user_row_menu(index, cx))
            .child(self.hover_stamp(index, cx))
            .child(
                v_flex()
                    // The annotation bubble is as wide as the prompt bubble
                    // because both fill this column, so the cap that keeps the
                    // prompt off the full width lives here rather than on
                    // either. The row above is `w_full`, so the fraction has a
                    // definite width to resolve against and tracks the pane.
                    .max_w(relative(USER_BUBBLE_WIDTH_FRACTION))
                    .min_w_0()
                    .items_end()
                    .gap_1()
                    .children(annotations)
                    .child(message),
            )
            .into_any_element()
    }

    /// The images a message carried, under its text. A reader who scrolls back
    /// should see what was sent, not the placeholder that stood in for it while
    /// the message was being written.
    fn render_entry_images(&self, index: usize, cx: &mut Context<Self>) -> Option<AnyElement> {
        let images = &self.items.get(index)?.images;
        if images.is_empty() {
            return None;
        }

        Some(
            h_flex()
                .mt_2()
                .gap_2()
                .flex_wrap()
                .justify_end()
                .children(images.iter().enumerate().map(|(position, image)| {
                    div()
                        .size(px(TRANSCRIPT_THUMBNAIL))
                        .flex_none()
                        .rounded(UI_RADIUS)
                        .overflow_hidden()
                        .border_1()
                        .border_color(cx.theme().border)
                        // Unique across rows: a row carries at most
                        // `MAX_ATTACHMENTS` images, so its band cannot overlap
                        // the next row's.
                        .id(("entry-image", index * MAX_ATTACHMENTS + position))
                        .child(img(image.clone()).size_full().object_fit(ObjectFit::Cover))
                }))
                .into_any_element(),
        )
    }

    /// Assistant reply: full-width bare markdown — no bubble, no border;
    /// alignment and surface carry the distinction.
    /// Presentation for assistant Markdown. Prose wraps at the reading
    /// measure, and the blocks that paint their own surface are held to the
    /// same width: a code block or table left full-width would run its
    /// background past the text above it, so the tint would mark a column the
    /// reader is not reading along.
    pub(super) fn transcript_code_block_style(font: Font, font_size: f32) -> StyleRefinement {
        StyleRefinement::default()
            .font(font)
            .text_size(px(font_size))
    }

    fn configured_transcript_code_block_style(cx: &App) -> StyleRefinement {
        let settings = cx.global::<AgentSettings>();

        Self::transcript_code_block_style(settings.transcript_font(), settings.transcript_font_size)
    }

    fn agent_text_style(cx: &App) -> TextViewStyle {
        // Assistant output takes the full transcript column: prose, code
        // blocks, and tables all reflow with the pane, so a wide window shows
        // long lines and wide tables without an inner scroll or a wrap the
        // reader has to undo mentally.
        TextViewStyle::default().code_block(Self::configured_transcript_code_block_style(cx))
    }

    fn work_detail_text_style(cx: &App) -> TextViewStyle {
        TextViewStyle::default().code_block(Self::configured_transcript_code_block_style(cx))
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

    pub(crate) fn render_agent_row(
        &self,
        index: usize,
        text: String,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        h_flex()
            .id(("entry", index))
            .group("entry")
            .relative()
            .w_full()
            .items_end()
            .modern_context_menu(Self::copy_menu(cx.entity().downgrade(), index))
            .child(
                div().flex_1().min_w_0().px_1().child(
                    Self::markdown_view(("agent-md", index), text, self.cwd.clone())
                        .style(Self::agent_text_style(cx))
                        .selectable(true),
                ),
            )
            .child(
                // A stamp in the flow would reserve its width on every row,
                // ending assistant output short of the pane by a strip that is
                // blank whenever the pointer is elsewhere. Out of the flow it
                // costs nothing until it appears, and the tinted chip keeps it
                // legible where it lands over the last line.
                self.hover_stamp(index, cx)
                    .absolute()
                    .right_1()
                    .bottom_0()
                    .px_1()
                    .rounded(UI_RADIUS)
                    .bg(cx.theme().muted),
            )
            .into_any_element()
    }

    pub(crate) fn render_error_row(
        &self,
        index: usize,
        text: String,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        h_flex()
            .id(("entry", index))
            .w_full()
            .modern_context_menu(Self::copy_menu(cx.entity().downgrade(), index))
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

    /// One step of the work log, as a card: icon block · heading · the
    /// technical text the heading names · outcome badge. Rows with detail
    /// (command output, reasoning text) expand on click into a bounded
    /// transcript surface with its own scroll position, drawn inside the same
    /// card so the detail stays visibly attached to the step it belongs to.
    pub(crate) fn render_work_row(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let cwd = self.cwd.clone();
        let (icon, heading, mono_detail, reason, status, detail) = match &self.items[index].item {
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
                    Some(command.clone()),
                    failed
                        .then(|| command_failure_reason(aggregated_output.as_deref()))
                        .flatten(),
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
                None,
                None,
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
                None,
                None,
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
                None,
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
        // A card carries its outcome as a stated badge rather than as a glyph:
        // the same slot then reads for a step that failed, one that succeeded
        // and one still running, without three symbols to learn first.
        let tone = match status.as_deref() {
            Some("failed" | "declined") => AgentCardTone::Failed,
            _ => AgentCardTone::Neutral,
        };
        let badge = status.as_ref().map(|state| {
            let color = match state.as_str() {
                "failed" | "declined" => cx.theme().danger,
                "completed" => cx.theme().success,
                _ => cx.theme().muted_foreground,
            };

            (status_label.to_string(), color)
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
            .tone(tone)
            .accessible_label(accessible_label);
        if let Some(mono_detail) = mono_detail {
            header = header.mono_detail(mono_detail);
        }
        if let Some((label, color)) = badge {
            header = header.badge(label, color);
        }
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

        agent_card(tone, cx)
            .id(("entry", index))
            .modern_context_menu(Self::copy_menu(cx.entity().downgrade(), index))
            .child(header)
            // Why a step failed belongs on the card rather than behind the
            // disclosure: it is what the reader decides their next move from,
            // and the full transcript below it is usually a stack trace.
            .children(reason.map(|reason| {
                div()
                    .w_full()
                    .pl(px(AGENT_DISCLOSURE_DETAIL_INSET))
                    .pr(px(AGENT_CARD_PADDING_X))
                    .pb(px(AGENT_CARD_PADDING_Y))
                    .text_size(px(AGENT_CARD_DETAIL_SIZE))
                    .text_color(cx.theme().danger.opacity(0.85))
                    .child(reason)
            }))
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
                    let settings = cx.global::<AgentSettings>();
                    let transcript_font = settings.transcript_font();
                    let transcript_font_size = px(settings.transcript_font_size);
                    let line_height = transcript_font_size * TRANSCRIPT_LINE_HEIGHT;
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
                        .font(transcript_font)
                        .text_size(transcript_font_size)
                        // The outer transcript list is behind this viewport in
                        // hit-test order. Occlusion prevents wheel input at the
                        // virtual list's limits from moving the conversation.
                        .occlude()
                        .modern_context_menu(Self::copy_menu(cx.entity().downgrade(), index))
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
                                        .id(("wl-virtual-v-scrollbar", index)),
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
                                        .id(("wl-virtual-h-scrollbar", index)),
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
                                .modern_context_menu(Self::copy_menu(
                                    cx.entity().downgrade(),
                                    index,
                                ))
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(
                                    Self::markdown_view(("wl-md", index), markdown, cwd.clone())
                                        .style(Self::work_detail_text_style(cx))
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
                                    Scrollbar::vertical(&detail_scroll).id(("wl-scrollbar", index)),
                                ),
                        )
                        .into_any_element()
                };

                div()
                    // Expanded content takes the card's own inset on both
                    // sides. The rule above it already says the detail belongs
                    // to the header, so indenting it as well would spend a
                    // third of a narrow card on saying it twice — and command
                    // output is exactly the content that needs the width.
                    .w_full()
                    .border_t_1()
                    .border_color(cx.theme().border.opacity(0.6))
                    .px(px(AGENT_CARD_PADDING_X))
                    .py(px(AGENT_CARD_PADDING_Y))
                    .relative()
                    .child(body)
            }))
            .into_any_element()
    }

    /// A context-compaction boundary. Rendered as a divider rather than a card
    /// because it is a structural break in the conversation: the rows above it
    /// are no longer what the model sees. Expanding reveals the accounting and
    /// the summary the thread continued from when the provider exposes it.
    pub(crate) fn render_compaction_row(
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
            .modern_context_menu(Self::copy_menu(cx.entity().downgrade(), index))
            // The rule sits above the heading: it closes off the conversation
            // that the summary below replaced.
            .child(div().w_full().h(px(1.)).bg(accent.opacity(0.35)))
            .child(
                agent_card(AgentCardTone::Neutral, cx)
                    .child(header)
                    .children(
                        expanded.then(|| self.render_compaction_detail(index, &detail, window, cx)),
                    ),
            )
            .into_any_element()
    }

    /// Expanded compaction body: the accounting as labelled rows, the manual
    /// instructions when the user gave any, and the summary itself in a bounded
    /// scroll surface (summaries routinely run to several kilobytes).
    pub(crate) fn render_compaction_detail(
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
            .w_full()
            .border_t_1()
            .border_color(cx.theme().border.opacity(0.6))
            .px(px(AGENT_CARD_PADDING_X))
            .py(px(AGENT_CARD_PADDING_Y))
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
                            .modern_context_menu(Self::copy_menu(cx.entity().downgrade(), index))
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
                                    .id(("compaction-scrollbar", index)),
                            ),
                    )
            }))
            .into_any_element()
    }

    /// The settled turn's work disclosure. It heads the rows it hides, so the
    /// chevron keeps its usual meaning: the content it reveals is below it.
    pub(crate) fn render_turn_fold(
        &self,
        turn: u64,
        row_count: usize,
        folded: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let label = if folded {
            i18n("agent-transcript-turn-work").replace("{count}", &row_count.to_string())
        } else {
            i18n("agent-transcript-turn-work-hide").to_string()
        };

        agent_card(AgentCardTone::Neutral, cx)
            .child(
                AgentDisclosureRow::new(("turn-fold", turn as usize), label.clone())
                    .expanded(!folded)
                    .type_icon(IconName::GalleryVerticalEnd)
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
                        if !this.toggled_turns.insert(turn) {
                            this.toggled_turns.remove(&turn);
                        }
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    /// The settled turn's closing "Worked for Ns" line, doubling as a section
    /// divider (bottom hairline). Reporting only: it accounts for work the
    /// disclosure above it owns, so making it clickable too would give one
    /// turn two controls over the same rows.
    pub(crate) fn render_turn_summary(
        &self,
        seconds: u64,
        output_tokens: Option<u64>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let label = worked_status_label(seconds, output_tokens);

        div()
            .w_full()
            .px_1()
            .text_size(px(AGENT_CARD_DETAIL_SIZE))
            .text_color(cx.theme().muted_foreground)
            .child(label)
            .into_any_element()
    }

    pub(crate) fn render_interrupted_row(
        &self,
        output_tokens: Option<u64>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .w_full()
            .px_1()
            .text_size(px(AGENT_CARD_DETAIL_SIZE))
            .text_color(cx.theme().muted_foreground)
            .child(interrupted_status_label(output_tokens))
            .into_any_element()
    }

    /// The "+N tool calls" / "Show fewer tool calls" toggle for a work run.
    pub(crate) fn render_run_toggle(
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

        agent_card(AgentCardTone::Neutral, cx)
            .child(
                AgentDisclosureRow::new(("wl-run", run_start), label.clone())
                    .expanded(expanded)
                    .type_icon(IconName::SquareTerminal)
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
                    })),
            )
            .into_any_element()
    }
}
