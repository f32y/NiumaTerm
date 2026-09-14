#[cfg(test)]
pub(super) use crate::agent_tab::transcript::code::prepared::VIRTUAL_TRANSCRIPT_MAX_SEGMENT_BYTES;
#[cfg(test)]
pub(super) use crate::agent_tab::transcript::code::prepared::should_virtualize_transcript;
#[cfg(test)]
pub(super) use crate::agent_tab::transcript::code::prepared::transcript_segments;

mod ansi;
mod prepared;
mod source;

#[cfg(test)]
mod tests;

use crate::agent_tab::settings::{AgentSettings, UI_RADIUS};
use crate::agent_tab::transcript::TranscriptView;
use crate::agent_tab::transcript::code::prepared::PreparedCode;
use crate::agent_tab::transcript::code::source::CodeSource;
use crate::agent_tab::transcript::render::TRANSCRIPT_LINE_HEIGHT;
use crate::agent_tab::transcript::render::text_style::{
    transcript_highlight_theme, transcript_text_style,
};
use crate::agent_tab::transcript::rows::RowSpec;
use gpui::prelude::*;
use gpui::{
    AsyncApp, Context, Entity, ListHorizontalSizingBehavior, Render, ScrollHandle, StyledText,
    Subscription, Task, UniformListScrollHandle, WeakEntity, Window, div, px, uniform_list,
};
use gpui_component::ActiveTheme as _;
use gpui_component::scroll::Scrollbar;
use gpui_component::spinner::Spinner;
use gpui_component::text::{TextView, TextViewState};
use nmt_agent::chat::Item;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

pub(crate) fn is_code_item(item: &Item) -> bool {
    match item {
        Item::CommandExecution { .. } | Item::FileChange { .. } => true,
        Item::Other { kind, .. } => !matches!(kind.as_str(), "TodoWrite" | "ExitPlanMode" | "Task"),
        _ => false,
    }
}

pub(crate) fn clean_output(text: &str) -> String {
    ansi::AnsiText::parse(text).text
}

#[derive(Default)]
pub(crate) struct CodeTranscriptCache {
    entries: HashMap<usize, CachedCode>,
}

struct CachedCode {
    dirty: bool,
    view: Entity<CodeView>,
    _observation: Subscription,
}

impl CodeTranscriptCache {
    pub(crate) fn ensure(
        &mut self,
        index: usize,
        item: &Item,
        cx: &mut Context<TranscriptView>,
    ) -> Entity<CodeView> {
        if let Some(cached) = self.entries.get_mut(&index) {
            if cached.dirty {
                let source = CodeSource::from_item(item).expect("code transcript item");

                cached
                    .view
                    .update(cx, |view, cx| view.set_source(source, cx));

                cached.dirty = false;
            }

            return cached.view.clone();
        }

        let source = CodeSource::from_item(item).expect("code transcript item");
        let view = cx.new(|cx| CodeView::new(source, cx));

        let observation = cx.observe(&view, move |transcript, _, cx| {
            // Asynchronous normalization can alter row height. Remeasure the
            // corresponding visible work row when its prepared text arrives.
            if let Some(row) = transcript.rows.iter().position(
                |row| matches!(row.spec, RowSpec::Work { index: item, .. } if item == index),
            ) {
                transcript.transcript_list.remeasure_items(row..row + 1);
            }

            cx.notify();
        });

        self.entries.insert(
            index,
            CachedCode {
                dirty: false,
                view: view.clone(),
                _observation: observation,
            },
        );

        view
    }

    pub(crate) fn invalidate(&mut self, index: usize) {
        if let Some(cached) = self.entries.get_mut(&index) {
            cached.dirty = true;
        }
    }

    pub(crate) fn invalidate_from(&mut self, first: usize) {
        for (index, cached) in &mut self.entries {
            if *index >= first {
                cached.dirty = true;
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn invalidate_all(&mut self) {
        for cached in self.entries.values_mut() {
            cached.dirty = true;
        }
    }

    pub(crate) fn drop_row(&mut self, index: usize) {
        self.entries.remove(&index);
    }

    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }
}

pub(crate) struct CodeView {
    source: Arc<CodeSource>,
    revision: u64,
    prepared: Option<Arc<PreparedCode>>,
    text_view: Entity<TextViewState>,
    scroll: ScrollHandle,
    virtual_scroll: UniformListScrollHandle,
    parse_task: Option<Task<()>>,
}

impl CodeView {
    fn new(source: CodeSource, cx: &mut Context<Self>) -> Self {
        let size = source.output.len() + source.command.as_ref().map_or(0, String::len);

        // Short content has its final layout on the first frame. Larger
        // output starts with a fixed-height viewport while it is prepared.
        let prepared = (size < 16 * 1024).then(|| Arc::new(PreparedCode::new(&source)));

        let mut view = Self {
            source: Arc::new(source),
            revision: 0,
            prepared,
            text_view: cx.new(|cx| TextViewState::plain("", cx)),
            scroll: ScrollHandle::default(),
            virtual_scroll: UniformListScrollHandle::default(),
            parse_task: None,
        };

        view.start_parse(cx);

        view
    }

    fn set_source(&mut self, source: CodeSource, cx: &mut Context<Self>) {
        if self.source.as_ref() == &source {
            return;
        }

        self.source = Arc::new(source);
        self.revision += 1;

        if self.parse_task.is_none() {
            self.start_parse(cx);
        }
    }

    fn start_parse(&mut self, cx: &mut Context<Self>) {
        // One worker per expanded card coalesces streamed updates. A newer
        // revision supersedes an in-flight result without starting another
        // concurrent parse of the same growing output.
        self.parse_task = Some(cx.spawn(Self::parse_loop));
    }

    async fn parse_loop(view: WeakEntity<Self>, cx: &mut AsyncApp) {
        loop {
            cx.background_executor()
                .timer(Duration::from_millis(24))
                .await;

            let Ok((revision, source)) =
                view.read_with(cx, |view, _| (view.revision, view.source.clone()))
            else {
                break;
            };

            let parsing = source.clone();

            let prepared = cx
                .background_spawn(async move {
                    let mut prepared = PreparedCode::new(&parsing);

                    prepared.parse_syntax();

                    prepared
                })
                .await;

            let Ok(done) = view.update(cx, |view, cx| {
                let done = view.revision == revision;

                // A completed prefix remains useful during sustained output.
                // Replacements must never display an older result, while
                // append-only streams can show progress before they pause.
                if done
                    || (view.source.command == source.command
                        && view.source.language == source.language
                        && view.source.strip_gutter == source.strip_gutter
                        && view.source.output.starts_with(&source.output))
                {
                    view.prepared = Some(Arc::new(prepared));

                    cx.notify();
                }

                if done {
                    view.parse_task = None;
                }

                done
            }) else {
                break;
            };

            if done {
                break;
            }
        }
    }
}

impl Render for CodeView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(prepared) = self.prepared.clone() else {
            return div()
                .w_full()
                .h(px(256.))
                .p_3()
                .child(Spinner::new())
                .into_any_element();
        };

        let theme = transcript_highlight_theme(cx);
        let foreground = cx.theme().foreground;
        let background = *cx.theme().tokens.muted;

        if prepared.virtualized {
            let settings = cx.global::<AgentSettings>();
            let font_size = px(settings.transcript_font_size);
            let line_height = font_size * TRANSCRIPT_LINE_HEIGHT;
            let source = prepared.clone();

            return div()
                .id("code-output")
                .w_full()
                .h(px(256.))
                .relative()
                .overflow_hidden()
                .rounded(UI_RADIUS)
                .bg(background)
                .text_color(foreground)
                .font(settings.transcript_font())
                .text_size(font_size)
                .occlude()
                .child(
                    uniform_list("code-lines", prepared.segments.len(), move |rows, _, _| {
                        rows.filter_map(|row| {
                            let range = source.segments.get(row)?.clone();

                            let styles =
                                source.styles(range.clone(), &theme, foreground, background);

                            Some(
                                div()
                                    .h(line_height)
                                    .flex_none()
                                    .line_height(line_height)
                                    .whitespace_nowrap()
                                    .child(
                                        StyledText::new(source.text[range].to_string())
                                            .with_highlights(styles),
                                    ),
                            )
                        })
                        .collect::<Vec<_>>()
                    })
                    .with_width_from_item(Some(prepared.widest_segment))
                    .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
                    .track_scroll(&self.virtual_scroll)
                    .size_full()
                    .p_3(),
                )
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .right_0()
                        .bottom_0()
                        .w(px(16.))
                        .child(Scrollbar::vertical(&self.virtual_scroll)),
                )
                .child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .bottom_0()
                        .h(px(16.))
                        .child(Scrollbar::horizontal(&self.virtual_scroll)),
                )
                .into_any_element();
        }

        let styles = prepared.styles(0..prepared.text.len(), &theme, foreground, background);

        self.text_view.update(cx, |view, cx| {
            view.set_highlighted_code(prepared.text.clone(), styles, cx)
        });

        div()
            .w_full()
            .relative()
            .child(
                div()
                    .id("code-output")
                    .w_full()
                    .max_h(px(256.))
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .occlude()
                    .text_color(foreground)
                    .child(
                        TextView::new(&self.text_view)
                            .style(transcript_text_style(cx))
                            .selectable(true),
                    ),
            )
            .child(
                div()
                    .absolute()
                    .top_0()
                    .right_0()
                    .bottom_0()
                    .w(px(16.))
                    .child(Scrollbar::vertical(&self.scroll)),
            )
            .into_any_element()
    }
}
