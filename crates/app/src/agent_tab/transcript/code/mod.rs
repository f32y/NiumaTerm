#[cfg(test)]
pub(super) use crate::agent_tab::transcript::code::prepared::VIRTUAL_TRANSCRIPT_MAX_SEGMENT_BYTES;
#[cfg(test)]
pub(super) use crate::agent_tab::transcript::code::prepared::should_virtualize_transcript;
#[cfg(test)]
pub(super) use crate::agent_tab::transcript::code::prepared::transcript_segments;

mod ansi;
mod parser;
mod prepared;
mod render_cache;
mod rows;
mod source;

#[cfg(test)]
mod streaming_tests;
#[cfg(test)]
mod tests;

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

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
use nmt_agent::transcript::conversation::{ContentChange, EntryChange};

use crate::agent_tab::settings::{AgentSettings, UI_RADIUS};
use crate::agent_tab::transcript::TranscriptView;
use crate::agent_tab::transcript::code::parser::CodeParser;
use crate::agent_tab::transcript::code::prepared::PreparedCode;
use crate::agent_tab::transcript::code::render_cache::CodeRenderCache;
use crate::agent_tab::transcript::code::source::CodeSource;
#[cfg(test)]
use crate::agent_tab::transcript::code::streaming_tests::record_copied;
use crate::agent_tab::transcript::render::TRANSCRIPT_LINE_HEIGHT;
use crate::agent_tab::transcript::render::text_style::{
    transcript_highlight_theme, transcript_text_style,
};
use crate::agent_tab::transcript::rows::RowSpec;

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
    dirty: Option<EntryChange>,
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
            if let Some(change) = cached.dirty.take() {
                cached.view.update(cx, |view, cx| {
                    if let EntryChange::Append { previous_bytes } = change
                        && view.output_bytes == previous_bytes
                        && let Some(output) = CodeSource::output(item).get(previous_bytes..)
                    {
                        view.append_output(output, cx);
                    } else {
                        view.set_source(
                            CodeSource::from_item(item).expect("code transcript item"),
                            cx,
                        );
                    }
                });
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
                dirty: None,
                view: view.clone(),
                _observation: observation,
            },
        );

        view
    }

    pub(crate) fn invalidate(&mut self, change: &ContentChange) {
        if let Some(first) = change.reset_from {
            self.invalidate_from(first);
        }

        for &(index, update) in &change.entries {
            if let Some(cached) = self.entries.get_mut(&index) {
                cached.dirty = Some(match (cached.dirty, update) {
                    (None, update) => update,
                    (
                        Some(EntryChange::Append {
                            previous_bytes: old,
                        }),
                        EntryChange::Append {
                            previous_bytes: new,
                        },
                    ) => EntryChange::Append {
                        previous_bytes: old.min(new),
                    },
                    _ => EntryChange::Replace,
                });
            }
        }
    }

    fn invalidate_from(&mut self, first: usize) {
        for (index, cached) in &mut self.entries {
            if *index >= first {
                cached.dirty = Some(EntryChange::Replace);
            }
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
    output_bytes: usize,
    generation: u64,
    revision: u64,
    pending: Option<CodeUpdate>,
    parser: Option<CodeParser>,
    prepared: Option<Arc<PreparedCode>>,
    text_view: Entity<TextViewState>,
    scroll: ScrollHandle,
    virtual_scroll: UniformListScrollHandle,
    parse_task: Option<Task<()>>,
    render_cache: Rc<RefCell<CodeRenderCache>>,
}

enum CodeUpdate {
    Replace(CodeSource),
    Append(String),
}

impl CodeView {
    fn new(source: CodeSource, cx: &mut Context<Self>) -> Self {
        let size = source.output.len() + source.command.as_ref().map_or(0, String::len);

        // Short content has its final layout on the first frame. Larger
        // output starts with a fixed-height viewport while it is prepared.
        let output_bytes = source.output.len();

        let (parser, pending) = if size < 16 * 1024 {
            (Some(CodeParser::new(source)), None)
        } else {
            (None, Some(CodeUpdate::Replace(source)))
        };

        let prepared = parser
            .as_ref()
            .map(|parser| Arc::new(parser.prepared.clone()));

        let mut view = Self {
            output_bytes,
            generation: 0,
            revision: 0,
            pending,
            parser,
            prepared,
            text_view: cx.new(|cx| TextViewState::plain("", cx)),
            scroll: ScrollHandle::default(),
            virtual_scroll: UniformListScrollHandle::default(),
            parse_task: None,
            render_cache: Rc::new(RefCell::new(CodeRenderCache::default())),
        };

        view.start_parse(cx);

        view
    }

    fn set_source(&mut self, source: CodeSource, cx: &mut Context<Self>) {
        self.output_bytes = source.output.len();
        self.generation += 1;
        self.pending = Some(CodeUpdate::Replace(source));
        self.revision += 1;

        if self.parse_task.is_none() {
            self.start_parse(cx);
        }
    }

    fn append_output(&mut self, output: &str, cx: &mut Context<Self>) {
        if output.is_empty() {
            return;
        }

        #[cfg(test)]
        record_copied(output.len());

        self.output_bytes += output.len();
        self.revision += 1;

        match &mut self.pending {
            Some(CodeUpdate::Replace(source)) => source.output.push_str(output),
            Some(CodeUpdate::Append(pending)) => pending.push_str(output),
            None => self.pending = Some(CodeUpdate::Append(output.into())),
        }

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
        let Ok(mut parser) = view.update(cx, |view, _| view.parser.take()) else {
            return;
        };

        loop {
            cx.background_executor()
                .timer(Duration::from_millis(24))
                .await;

            let Ok((generation, revision, pending)) = view.update(cx, |view, _| {
                (view.generation, view.revision, view.pending.take())
            }) else {
                break;
            };

            let (updated, prepared) = cx
                .background_spawn(async move {
                    match pending {
                        Some(CodeUpdate::Replace(source)) => parser = Some(CodeParser::new(source)),
                        Some(CodeUpdate::Append(output)) => parser
                            .as_mut()
                            .expect("append has a parser")
                            .append(&output),
                        None => {}
                    }

                    let mut parser = parser.expect("source initializes a parser");

                    parser.prepared.parse_syntax();

                    let prepared = Arc::new(parser.prepared.clone());

                    (parser, prepared)
                })
                .await;

            parser = Some(updated);

            let Ok(done) = view.update(cx, |view, cx| {
                let done = view.revision == revision;

                // A completed prefix remains useful during sustained output.
                // Replacements must never display an older result, while
                // append-only streams can show progress before they pause.
                if view.generation == generation {
                    view.prepared = Some(prepared);

                    cx.notify();
                }

                if done {
                    view.parser = parser.take();
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

        self.render_cache
            .borrow_mut()
            .update(&prepared, &theme, foreground, background);

        if prepared.virtualized {
            let settings = cx.global::<AgentSettings>();
            let font_size = px(settings.transcript_font_size);
            let line_height = font_size * TRANSCRIPT_LINE_HEIGHT;
            let source = prepared.clone();
            let cache = self.render_cache.clone();

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
                            let segment = source.segments.get(row)?;
                            let styles = cache.borrow_mut().styles(row, segment.range.clone());

                            Some(
                                div()
                                    .h(line_height)
                                    .flex_none()
                                    .line_height(line_height)
                                    .whitespace_nowrap()
                                    .child(
                                        StyledText::new(segment.text.clone())
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

        let styles = self
            .render_cache
            .borrow_mut()
            .styles(usize::MAX, 0..prepared.text.len());

        self.text_view.update(cx, |view, cx| {
            view.set_highlighted_code(prepared.short_text.clone(), styles, cx)
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
