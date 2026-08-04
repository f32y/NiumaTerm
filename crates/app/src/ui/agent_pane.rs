//! Agent tab: renders a Codex conversation as chat bubbles instead of a
//! terminal grid. All Codex process and protocol handling lives in
//! [`nmt_agent_utils::codex::app_server`]; this module only maps its typed
//! events onto the transcript UI.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use chrono::Local;
use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui::prelude::*;
use gpui::{
    AnyElement, ClipboardItem, Context, Entity, FocusHandle, ScrollHandle, Window, div, px,
    relative,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenuItem};
use gpui_component::{ActiveTheme as _, Sizable as _, h_flex, text, v_flex};
use nmt_agent_utils::codex::app_server::{
    self, Event as CodexEvent, Item as CodexItem, ModelInfo, SendOutcome, Session, ThreadSettings,
};
use serde_json::Value;
use tracing::warn;

use crate::ui::AppSettings;

/// A transcript entry plus the local wall-clock time it first appeared, shown
/// beside its bubble. Streamed items keep their start time.
struct Entry {
    at: String,
    item: ChatItem,
}

/// One entry in the conversation transcript. Streaming notifications mutate
/// the entry in place (keyed by the protocol's item id) until `item/completed`
/// finalizes it with the authoritative payload.
enum ChatItem {
    User {
        text: String,
    },
    Agent {
        item_id: String,
        text: String,
    },
    Reasoning {
        item_id: String,
        text: String,
    },
    Command {
        item_id: String,
        command: String,
        output: String,
        status: String,
        exit_code: Option<i64>,
    },
    FileChange {
        item_id: String,
        summary: String,
        status: String,
    },
    /// Fallback card for every other tool-call item kind (mcpToolCall,
    /// webSearch, dynamicToolCall, …): kind + best-effort title + status.
    Tool {
        item_id: String,
        kind: String,
        title: String,
        status: String,
    },
    /// Per-turn duration record, appended after the turn's last output:
    /// "Worked for x seconds". The live ticking line is a render-only row
    /// pinned to the transcript end (see `working_started`).
    Working {
        started: Instant,
        done_seconds: Option<u64>,
    },
    Error {
        text: String,
    },
}

#[derive(Clone, Copy, PartialEq)]
enum Status {
    Starting,
    Idle,
    Running,
    Exited,
}

pub(crate) struct AgentPane {
    pub(crate) focus: FocusHandle,
    items: Vec<Entry>,
    scroll: ScrollHandle,
    input: Entity<InputState>,
    session: Option<Session>,
    status: Status,
    status_detail: Option<String>,
    /// Description of the approval request blocking the turn, shown as the
    /// card above the input; the request id lives in the session.
    pending_approval: Option<String>,
    /// Current thread settings, seeded from the session's `Ready` event and
    /// changed via the dropdowns under the input; sent as overrides on every
    /// turn start (idempotent when unchanged).
    settings: ThreadSettings,
    /// Model catalog; service tiers are per model, so the tier dropdown lists
    /// the selected model's tiers.
    models: Vec<ModelInfo>,
    /// Collapsed tool-call groups the user has expanded, keyed by the index
    /// of the group's first transcript entry (stable — the list only appends).
    expanded_groups: HashSet<usize>,
    /// Start time of the running turn. While set, a ticking
    /// "Working in progress ..." row renders at the transcript end; cleared
    /// into a permanent "Worked for x seconds" entry when the turn completes.
    working_started: Option<Instant>,
}

impl AgentPane {
    pub(crate) fn new(cwd: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Message Codex — Enter to send"));

        cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.send_user_message(window, cx);
            }
        })
        .detach();

        let mut this = Self {
            focus: cx.focus_handle(),
            items: Vec::new(),
            scroll: ScrollHandle::new(),
            input,
            session: None,
            status: Status::Starting,
            status_detail: None,
            pending_approval: None,
            settings: ThreadSettings::default(),
            models: Vec::new(),
            expanded_groups: HashSet::new(),
            working_started: None,
        };

        // The session's reader thread feeds parsed messages into this
        // channel; the async pump below hops them onto the UI thread, where
        // `Session::process` turns them into typed events. Channel closure is
        // the EOF signal (the sender is owned by the reader thread).
        let (tx, mut rx) = mpsc::unbounded::<Value>();

        match Session::spawn(
            cwd,
            move |message| {
                let _ = tx.unbounded_send(message);
            },
            |line| warn!("codex app-server: {line}"),
        ) {
            Ok(session) => {
                this.session = Some(session);

                cx.spawn(async move |this, cx| {
                    while let Some(message) = rx.next().await {
                        let updated = this.update(cx, |this, cx| {
                            let events = match this.session.as_mut() {
                                Some(session) => session.process(message),
                                None => Vec::new(),
                            };

                            for event in events {
                                this.apply_event(event, cx);
                            }
                        });

                        if updated.is_err() {
                            return;
                        }
                    }

                    let _ = this.update(cx, |this, cx| {
                        this.status = Status::Exited;
                        this.finish_working(cx);
                        this.push(
                            ChatItem::Error {
                                text: "Codex exited.".to_string(),
                            },
                            cx,
                        );
                    });
                })
                .detach();
            }
            Err(err) => {
                this.status = Status::Exited;
                this.items.push(Entry {
                    at: Local::now().format("%H:%M").to_string(),
                    item: ChatItem::Error {
                        text: format!("Failed to start Codex: {err}"),
                    },
                });
            }
        }

        this
    }

    pub(crate) fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.focus(window, cx));
    }

    fn push(&mut self, item: ChatItem, cx: &mut Context<Self>) {
        self.items.push(Entry {
            at: Local::now().format("%H:%M").to_string(),
            item,
        });
        self.scroll.scroll_to_bottom();
        cx.notify();
    }

    fn send_user_message(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).text().to_string();
        let text = text.trim().to_string();

        if text.is_empty() {
            return;
        }

        let settings = self.settings.clone();
        let outcome = match self.session.as_mut() {
            Some(session) => session.send_user_message(&text, &settings),
            None => SendOutcome::NotReady,
        };

        if outcome == SendOutcome::NotReady {
            self.push(
                ChatItem::Error {
                    text: "Codex is still starting; try again in a moment.".to_string(),
                },
                cx,
            );
            return;
        }

        self.input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.push(ChatItem::User { text }, cx);

        // A steer joins the running turn, which already has its progress line.
        if outcome == SendOutcome::StartedTurn {
            self.start_working(cx);
        }
    }

    /// Start the turn clock and drive the once-a-second repaint of the live
    /// progress row; the ticker stops itself once `finish_working` clears it.
    fn start_working(&mut self, cx: &mut Context<Self>) {
        self.working_started = Some(Instant::now());
        self.scroll.scroll_to_bottom();
        cx.notify();

        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;

                let ticking = this.update(cx, |this, cx| {
                    if this.working_started.is_some() {
                        cx.notify();
                        true
                    } else {
                        false
                    }
                });

                if !ticking.unwrap_or(false) {
                    break;
                }
            }
        })
        .detach();
    }

    /// Record the finished turn's duration as a permanent transcript entry —
    /// appended last, so it sits below the turn's output cards.
    fn finish_working(&mut self, cx: &mut Context<Self>) {
        if let Some(started) = self.working_started.take() {
            self.push(
                ChatItem::Working {
                    started,
                    done_seconds: Some(started.elapsed().as_secs()),
                },
                cx,
            );
        }
    }

    fn interrupt(&mut self, cx: &mut Context<Self>) {
        if let Some(session) = self.session.as_mut() {
            session.interrupt();
            cx.notify();
        }
    }

    fn respond_approval(&mut self, decision: &str, cx: &mut Context<Self>) {
        // The card is dismissed immediately for a snappy UI; the session's
        // `ApprovalResolved` confirmation is then a no-op.
        self.pending_approval = None;

        if let Some(session) = self.session.as_mut() {
            session.respond_approval(decision);
        }
        cx.notify();
    }

    /// Apply one typed session event to the transcript and status line.
    fn apply_event(&mut self, event: CodexEvent, cx: &mut Context<Self>) {
        match event {
            CodexEvent::Ready(settings) => {
                // Seed the settings dropdowns with the thread's effective
                // configuration so they show real values before any change.
                self.settings = settings;
                self.status = Status::Idle;
                cx.notify();
            }
            CodexEvent::Models(models) => {
                self.models = models;
                cx.notify();
            }
            CodexEvent::TurnStarted => {
                self.status = Status::Running;
                cx.notify();
            }
            CodexEvent::TurnCompleted { error } => {
                self.finish_working(cx);
                if self.status == Status::Running {
                    self.status = Status::Idle;
                }
                if let Some(text) = error {
                    self.push(ChatItem::Error { text }, cx);
                }
                cx.notify();
            }
            CodexEvent::ItemStarted(item) => self.start_item(item, cx),
            CodexEvent::ItemCompleted(item) => self.complete_item(item, cx),
            CodexEvent::AgentMessageDelta { item_id, delta } => {
                self.append_delta(&item_id, &delta, |item| match item {
                    ChatItem::Agent { text, .. } => Some(text),
                    _ => None,
                });
                cx.notify();
            }
            CodexEvent::ReasoningSummaryDelta { item_id, delta } => {
                self.append_delta(&item_id, &delta, |item| match item {
                    ChatItem::Reasoning { text, .. } => Some(text),
                    _ => None,
                });
                cx.notify();
            }
            CodexEvent::CommandOutputDelta { item_id, delta } => {
                self.append_delta(&item_id, &delta, |item| match item {
                    ChatItem::Command { output, .. } => Some(output),
                    _ => None,
                });
                cx.notify();
            }
            CodexEvent::ApprovalRequested { description } => {
                self.pending_approval = Some(description);
                self.scroll.scroll_to_bottom();
                cx.notify();
            }
            CodexEvent::ApprovalResolved => {
                self.pending_approval = None;
                cx.notify();
            }
            CodexEvent::Error { message, fatal } => {
                if fatal {
                    self.status = Status::Exited;
                }
                self.push(ChatItem::Error { text: message }, cx);
            }
            CodexEvent::StatusDetail(detail) => {
                self.status_detail = detail;
                cx.notify();
            }
        }
    }

    fn start_item(&mut self, item: CodexItem, cx: &mut Context<Self>) {
        let chat_item = match item {
            // Echoes of our own turn input, already rendered locally on send.
            CodexItem::UserMessage => return,
            CodexItem::AgentMessage { id, text } => ChatItem::Agent {
                item_id: id,
                text: text.unwrap_or_default(),
            },
            CodexItem::Reasoning { id, summary } => ChatItem::Reasoning {
                item_id: id,
                text: summary.unwrap_or_default(),
            },
            CodexItem::CommandExecution {
                id,
                command,
                aggregated_output,
                status,
                exit_code,
            } => ChatItem::Command {
                item_id: id,
                command,
                output: aggregated_output.unwrap_or_default(),
                status: status.unwrap_or_else(|| "inProgress".to_string()),
                exit_code,
            },
            CodexItem::FileChange { id, paths, status } => ChatItem::FileChange {
                item_id: id,
                summary: paths,
                status: status.unwrap_or_else(|| "inProgress".to_string()),
            },
            CodexItem::Other {
                id,
                kind,
                title,
                status,
            } => ChatItem::Tool {
                item_id: id,
                kind,
                title,
                status: status.unwrap_or_else(|| "inProgress".to_string()),
            },
        };

        self.push(chat_item, cx);
    }

    fn complete_item(&mut self, item: CodexItem, cx: &mut Context<Self>) {
        let Some(id) = codex_item_id(&item).map(str::to_owned) else {
            return;
        };

        let known = self
            .items
            .iter()
            .any(|entry| chat_item_id(&entry.item) == Some(id.as_str()));

        // A completed item this pane never saw start (e.g. joined mid-turn)
        // still gets a transcript entry.
        if !known {
            self.start_item(item.clone(), cx);
        }

        for entry in &mut self.items {
            if chat_item_id(&entry.item) != Some(id.as_str()) {
                continue;
            }

            match (&mut entry.item, &item) {
                (ChatItem::Agent { text, .. }, CodexItem::AgentMessage { text: full, .. }) => {
                    if let Some(full) = full {
                        *text = full.clone();
                    }
                }
                (
                    ChatItem::Command {
                        output,
                        status,
                        exit_code,
                        ..
                    },
                    CodexItem::CommandExecution {
                        aggregated_output,
                        status: new_status,
                        exit_code: new_exit,
                        ..
                    },
                ) => {
                    if let Some(full) = aggregated_output {
                        *output = full.clone();
                    }
                    if let Some(state) = new_status {
                        *status = state.clone();
                    }
                    *exit_code = *new_exit;
                }
                (
                    ChatItem::FileChange { status, .. },
                    CodexItem::FileChange {
                        status: new_status, ..
                    },
                )
                | (
                    ChatItem::Tool { status, .. },
                    CodexItem::Other {
                        status: new_status, ..
                    },
                ) => {
                    if let Some(state) = new_status {
                        *status = state.clone();
                    }
                }
                (ChatItem::Reasoning { text, .. }, CodexItem::Reasoning { summary, .. }) => {
                    // Some reasoning items never stream summary deltas; the
                    // completed payload's summary is the fallback. Items that
                    // stay empty are hidden by the renderer.
                    if text.is_empty()
                        && let Some(summary) = summary
                    {
                        *text = summary.clone();
                    }
                }
                _ => {}
            }
            break;
        }

        self.scroll.scroll_to_bottom();
        cx.notify();
    }

    fn append_delta(
        &mut self,
        item_id: &str,
        delta: &str,
        select: fn(&mut ChatItem) -> Option<&mut String>,
    ) {
        for entry in &mut self.items {
            if chat_item_id(&entry.item) == Some(item_id)
                && let Some(text) = select(&mut entry.item)
            {
                text.push_str(delta);
                self.scroll.scroll_to_bottom();
                return;
            }
        }
    }

    fn status_label(&self) -> String {
        match self.status {
            Status::Starting => "starting…".to_string(),
            Status::Running => self
                .status_detail
                .clone()
                .unwrap_or_else(|| "working…".to_string()),
            Status::Idle => "ready".to_string(),
            Status::Exited => "exited".to_string(),
        }
    }
}

/// Item ids key streaming deltas to their transcript entry.
fn chat_item_id(item: &ChatItem) -> Option<&str> {
    match item {
        ChatItem::Agent { item_id, .. }
        | ChatItem::Reasoning { item_id, .. }
        | ChatItem::Command { item_id, .. }
        | ChatItem::FileChange { item_id, .. }
        | ChatItem::Tool { item_id, .. } => Some(item_id),
        ChatItem::User { .. } | ChatItem::Error { .. } | ChatItem::Working { .. } => None,
    }
}

/// The transcript line for a `Working` entry, shared by render and Copy.
fn working_label(started: Instant, done_seconds: Option<u64>) -> String {
    match done_seconds {
        Some(seconds) => format!("Worked for {seconds} seconds"),
        None => format!(
            "Working in progress ... ({} seconds)",
            started.elapsed().as_secs()
        ),
    }
}

/// The grouping key for collapsed tool-call rows: consecutive entries with the
/// same key fold into one summary line. Conversation text never groups.
fn tool_kind(item: &ChatItem) -> Option<&str> {
    match item {
        ChatItem::Command { .. } => Some("command"),
        ChatItem::FileChange { .. } => Some("fileChange"),
        ChatItem::Tool { kind, .. } => Some(kind),
        _ => None,
    }
}

/// Entries with nothing to show (yet): an agent bubble before its first delta,
/// or a reasoning item that never streamed a summary. They render no row and
/// are transparent to tool-call grouping, so an invisible entry can't split a
/// run of same-kind tool calls into two summary lines.
fn hidden(item: &ChatItem) -> bool {
    match item {
        ChatItem::Agent { text, .. } | ChatItem::Reasoning { text, .. } => text.trim().is_empty(),
        _ => false,
    }
}

fn group_label(kind: &str, count: usize) -> String {
    let plural = if count == 1 { "" } else { "s" };

    match kind {
        "command" => format!("Ran {count} command{plural}"),
        "fileChange" => format!("Edited {count} file{plural}"),
        _ => format!("Ran {count} {kind} call{plural}"),
    }
}

/// The protocol item id, used to match a completed item to its entry.
fn codex_item_id(item: &CodexItem) -> Option<&str> {
    match item {
        CodexItem::UserMessage => None,
        CodexItem::AgentMessage { id, .. }
        | CodexItem::Reasoning { id, .. }
        | CodexItem::CommandExecution { id, .. }
        | CodexItem::FileChange { id, .. }
        | CodexItem::Other { id, .. } => Some(id),
    }
}

impl gpui::Focusable for AgentPane {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for AgentPane {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let bubbles: Vec<Option<AnyElement>> = self
            .items
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                // An empty element would still eat a list gap and show as a
                // blank band, so hidden entries produce no row at all.
                if hidden(&entry.item) {
                    return None;
                }

                // Timestamp beside every bubble, aligned to its bottom edge on the
                // inner side (left of user bubbles, right of agent output).
                let stamp = div()
                    .flex_none()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(entry.at.clone());

                // Full text of the entry for the right-click Copy action — the
                // whole message, independent of any partial selection.
                let copy_text = match &entry.item {
                    ChatItem::User { text }
                    | ChatItem::Error { text }
                    | ChatItem::Agent { text, .. }
                    | ChatItem::Reasoning { text, .. } => text.clone(),
                    ChatItem::Command {
                        command, output, ..
                    } => format!("$ {command}\n{output}"),
                    ChatItem::FileChange {
                        summary, status, ..
                    } => format!("Edit {summary} — {status}"),
                    ChatItem::Tool {
                        kind,
                        title,
                        status,
                        ..
                    } => format!("{kind} {title} — {status}"),
                    ChatItem::Working {
                        started,
                        done_seconds,
                    } => working_label(*started, *done_seconds),
                };

                let row = h_flex()
                    .id(("agent-bubble", index))
                    .w_full()
                    .items_end()
                    .gap_2()
                    .context_menu(move |menu, _, _| {
                        let copy_text = copy_text.clone();
                        menu.item(PopupMenuItem::new("Copy").on_click(move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(copy_text.clone()));
                        }))
                    });

                Some(match &entry.item {
                    ChatItem::User { text } => row
                        .justify_end()
                        .child(stamp)
                        .child(
                            div()
                                .max_w(relative(0.75))
                                .px_3()
                                .py_2()
                                .rounded(cx.theme().radius_lg)
                                .bg(cx.theme().primary)
                                .text_color(cx.theme().primary_foreground)
                                .child(text.clone()),
                        )
                        .into_any_element(),
                    ChatItem::Agent { text, .. } => row
                        .justify_start()
                        .child(
                            div()
                                .max_w(relative(0.75))
                                .px_3()
                                .py_2()
                                .rounded(cx.theme().radius_lg)
                                .bg(cx.theme().secondary)
                                .text_color(cx.theme().secondary_foreground)
                                .child(
                                    text::TextView::markdown(("agent-md", index), text.clone())
                                        .selectable(true),
                                ),
                        )
                        .child(stamp)
                        .into_any_element(),
                    // Reasoning is a transient thinking line, not a message; no
                    // timestamp, to keep it visually subordinate.
                    ChatItem::Reasoning { text, .. } => row
                        .justify_start()
                        .child(
                            div()
                                .max_w(relative(0.75))
                                .px_3()
                                .py_1()
                                .text_sm()
                                .italic()
                                .text_color(cx.theme().muted_foreground)
                                .child(text.clone()),
                        )
                        .into_any_element(),
                    ChatItem::Command {
                        command,
                        output,
                        status,
                        exit_code,
                        ..
                    } => {
                        let status_color = match status.as_str() {
                            "completed" => cx.theme().success,
                            "failed" | "declined" => cx.theme().danger,
                            _ => cx.theme().muted_foreground,
                        };
                        let status_line = match exit_code {
                            Some(code) => format!("{status} (exit {code})"),
                            None => status.clone(),
                        };
                        // Live output can grow unboundedly; the card shows the tail.
                        let tail: String = tail_lines(output, 12);

                        row.justify_start()
                            .child(
                                v_flex()
                                    .max_w(relative(0.9))
                                    .px_3()
                                    .py_2()
                                    .gap_1()
                                    .rounded(cx.theme().radius_lg)
                                    .border_1()
                                    .border_color(cx.theme().border)
                                    .text_sm()
                                    .child(
                                        h_flex()
                                            .gap_2()
                                            .child(div().child(format!("$ {command}")))
                                            .child(
                                                div().text_color(status_color).child(status_line),
                                            ),
                                    )
                                    .children((!tail.is_empty()).then(|| {
                                        div().text_color(cx.theme().muted_foreground).child(tail)
                                    })),
                            )
                            .child(stamp)
                            .into_any_element()
                    }
                    ChatItem::FileChange {
                        summary, status, ..
                    } => row
                        .justify_start()
                        .child(
                            div()
                                .max_w(relative(0.9))
                                .px_3()
                                .py_2()
                                .rounded(cx.theme().radius_lg)
                                .border_1()
                                .border_color(cx.theme().border)
                                .text_sm()
                                .child(format!("Edit {summary} — {status}")),
                        )
                        .child(stamp)
                        .into_any_element(),
                    ChatItem::Tool {
                        kind,
                        title,
                        status,
                        ..
                    } => {
                        let status_color = match status.as_str() {
                            "completed" => cx.theme().success,
                            "failed" | "declined" => cx.theme().danger,
                            _ => cx.theme().muted_foreground,
                        };
                        let label = if title.is_empty() {
                            kind.clone()
                        } else {
                            format!("{kind}: {title}")
                        };

                        row.justify_start()
                            .child(
                                h_flex()
                                    .max_w(relative(0.9))
                                    .px_3()
                                    .py_2()
                                    .gap_2()
                                    .rounded(cx.theme().radius_lg)
                                    .border_1()
                                    .border_color(cx.theme().border)
                                    .text_sm()
                                    .child(div().child(label))
                                    .child(div().text_color(status_color).child(status.clone())),
                            )
                            .child(stamp)
                            .into_any_element()
                    }
                    // Progress line, visually subordinate like the reasoning
                    // row; no timestamp (its content is itself a duration).
                    ChatItem::Working {
                        started,
                        done_seconds,
                    } => row
                        .justify_start()
                        .child(
                            div()
                                .px_3()
                                .py_1()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(working_label(*started, *done_seconds)),
                        )
                        .into_any_element(),
                    ChatItem::Error { text } => row
                        .justify_start()
                        .child(
                            div()
                                .max_w(relative(0.9))
                                .px_3()
                                .py_2()
                                .rounded(cx.theme().radius_lg)
                                .bg(cx.theme().danger.opacity(0.15))
                                .text_color(cx.theme().danger)
                                .text_sm()
                                .child(text.clone()),
                        )
                        .child(stamp)
                        .into_any_element(),
                })
            })
            .collect();

        // With the collapse setting on, runs of consecutive same-kind tool
        // calls render as one clickable summary line; the prebuilt bubbles of
        // a collapsed run are simply dropped. Groups are keyed by the index of
        // their first entry, which is stable because the transcript only
        // appends.
        let collapse = cx.global::<AppSettings>().collapse_tool_calls;
        let mut rows: Vec<AnyElement> = Vec::new();
        let mut elems = bubbles.into_iter();
        let mut cursor = 0;

        while cursor < self.items.len() {
            let kind = tool_kind(&self.items[cursor].item).map(str::to_owned);

            match kind {
                Some(kind) if collapse => {
                    let start = cursor;
                    let mut end = cursor + 1;

                    // Hidden entries inside the run are swallowed by the
                    // group; every visible entry in range is then a same-kind
                    // tool call, so the count excludes the hidden ones.
                    while end < self.items.len()
                        && (hidden(&self.items[end].item)
                            || tool_kind(&self.items[end].item) == Some(kind.as_str()))
                    {
                        end += 1;
                    }

                    let count = self.items[start..end]
                        .iter()
                        .filter(|entry| !hidden(&entry.item))
                        .count();

                    let expanded = self.expanded_groups.contains(&start);

                    rows.push(self.render_group_header(start, &kind, count, expanded, cx));

                    for _ in start..end {
                        let elem = elems.next().flatten();

                        if expanded && let Some(elem) = elem {
                            rows.push(elem);
                        }
                    }

                    cursor = end;
                }
                _ => {
                    if let Some(elem) = elems.next().flatten() {
                        rows.push(elem);
                    }
                    cursor += 1;
                }
            }
        }

        // Live progress row, pinned below everything the turn has produced so
        // far; replaced by the permanent "Worked for x seconds" entry on
        // completion.
        if let Some(started) = self.working_started {
            rows.push(
                h_flex()
                    .w_full()
                    .justify_start()
                    .child(
                        div()
                            .px_3()
                            .py_1()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(working_label(started, None)),
                    )
                    .into_any_element(),
            );
        }

        let approval = self.pending_approval.as_ref().map(|approval| {
            v_flex()
                .w_full()
                .p_3()
                .gap_2()
                .rounded(cx.theme().radius_lg)
                .border_1()
                .border_color(cx.theme().warning)
                .child(div().text_sm().child(approval.clone()))
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("approval-accept")
                                .primary()
                                .label("Allow")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.respond_approval("accept", cx)
                                })),
                        )
                        .child(
                            Button::new("approval-decline")
                                .outline()
                                .label("Decline")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.respond_approval("decline", cx)
                                })),
                        ),
                )
        });

        let running = self.status == Status::Running;

        v_flex()
            .size_full()
            .track_focus(&self.focus)
            // The agent tab is a terminal surface stand-in, so it overrides the
            // chrome's UI font with its own configured font (Settings → Agent
            // Font), same as the terminal pane does with the terminal font.
            .font_family(cx.global::<AppSettings>().agent_font_family.clone())
            .text_size(px(cx.global::<AppSettings>().agent_font_size as f32))
            .child(
                h_flex()
                    .w_full()
                    .px_3()
                    .py_1()
                    .gap_2()
                    .items_center()
                    .child(div().text_sm().child("Codex"))
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(self.status_label()),
                    )
                    .children(running.then(|| {
                        Button::new("agent-stop")
                            .ghost()
                            .label("Stop")
                            .on_click(cx.listener(|this, _, _, cx| this.interrupt(cx)))
                    })),
            )
            .child(
                div()
                    .id("agent-transcript")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(v_flex().w_full().p_3().gap_2().children(rows)),
            )
            .child(
                v_flex()
                    .w_full()
                    .p_2()
                    .gap_2()
                    .children(approval)
                    .child(
                        h_flex()
                            .w_full()
                            .gap_2()
                            .child(div().flex_1().child(Input::new(&self.input)))
                            .child(Button::new("agent-send").primary().label("Send").on_click(
                                cx.listener(|this, _, window, cx| {
                                    this.send_user_message(window, cx)
                                }),
                            )),
                    )
                    .child(self.render_settings_row(cx)),
            )
    }
}

impl AgentPane {
    /// The one-line summary of a collapsed run of same-kind tool calls;
    /// clicking toggles the detail rows.
    fn render_group_header(
        &self,
        start: usize,
        kind: &str,
        count: usize,
        expanded: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let arrow = if expanded { "▾" } else { "▸" };
        let label = format!("{arrow} {}", group_label(kind, count));
        let hover_bg = cx.theme().secondary;

        h_flex()
            .w_full()
            .justify_start()
            .child(
                div()
                    .id(("agent-toolgroup", start))
                    .px_3()
                    .py_1()
                    .rounded(cx.theme().radius_lg)
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .cursor_pointer()
                    .hover(move |style| style.bg(hover_bg))
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.expanded_groups.insert(start) {
                            this.expanded_groups.remove(&start);
                        }
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    /// The dropdown row under the input: model, approval policy, sandbox,
    /// reasoning effort, and service tier. Values are thread settings sent as
    /// overrides on the next `turn/start`.
    fn render_settings_row(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let model_options: Vec<(String, String)> = self
            .models
            .iter()
            .map(|m| (m.model.clone(), m.display.clone()))
            .collect();
        // Service tiers are per model, and the catalog only lists the
        // additional tiers (e.g. "Fast") — the normal tier is implicit, so
        // the menu carries a synthetic entry for it. Empty wire value =
        // normal = explicit `serviceTier: null` on the next turn.
        let mut tier_options: Vec<(String, String)> = vec![(String::new(), "normal".to_string())];

        tier_options.extend(
            self.models
                .iter()
                .find(|m| Some(&m.model) == self.settings.model.as_ref())
                .map(|m| m.tiers.clone())
                .unwrap_or_default(),
        );
        let approval_options: Vec<(String, String)> = app_server::APPROVAL_OPTIONS
            .iter()
            .map(|v| (v.to_string(), v.to_string()))
            .collect();
        let sandbox_options: Vec<(String, String)> = app_server::SANDBOX_OPTIONS
            .iter()
            .map(|(v, label)| (v.to_string(), label.to_string()))
            .collect();
        let effort_options: Vec<(String, String)> = app_server::EFFORT_OPTIONS
            .iter()
            .map(|v| (v.to_string(), v.to_string()))
            .collect();

        h_flex()
            .w_full()
            .gap_1()
            .flex_wrap()
            .text_color(cx.theme().muted_foreground)
            .child(Self::setting_picker(
                cx,
                "agent-model",
                "model",
                self.settings.model.clone(),
                model_options,
                |this, value| {
                    // A tier the new model doesn't offer falls back to that
                    // model's default tier instead of erroring the next turn.
                    if let Some(info) = this.models.iter().find(|m| m.model == value)
                        && !this
                            .settings
                            .tier
                            .as_ref()
                            .is_some_and(|tier| info.tiers.iter().any(|(id, _)| id == tier))
                    {
                        this.settings.tier = info.default_tier.clone();
                    }
                    this.settings.model = Some(value);
                },
            ))
            .child(Self::setting_picker(
                cx,
                "agent-approval",
                "approval",
                self.settings.approval.clone(),
                approval_options,
                |this, value| this.settings.approval = Some(value),
            ))
            .child(Self::setting_picker(
                cx,
                "agent-sandbox",
                "sandbox",
                self.settings.sandbox.clone(),
                sandbox_options,
                |this, value| this.settings.sandbox = Some(value),
            ))
            .child(Self::setting_picker(
                cx,
                "agent-effort",
                "effort",
                self.settings.effort.clone(),
                effort_options,
                |this, value| this.settings.effort = Some(value),
            ))
            .child(Self::setting_picker(
                cx,
                "agent-tier",
                "tier",
                Some(self.settings.tier.clone().unwrap_or_default()),
                tier_options,
                |this, value| {
                    this.settings.tier = (!value.is_empty()).then_some(value);
                },
            ))
    }

    /// One dropdown: a ghost button labeled `name: current` whose menu lists
    /// `(wire value, display label)` options; picking one stores the wire
    /// value via `set`.
    fn setting_picker(
        cx: &mut Context<Self>,
        id: &'static str,
        name: &'static str,
        current: Option<String>,
        options: Vec<(String, String)>,
        set: fn(&mut Self, String),
    ) -> impl IntoElement + use<> {
        let pane = cx.entity();

        // Show the display label of the current wire value when we know it.
        let current_label = current
            .as_ref()
            .map(|value| {
                options
                    .iter()
                    .find(|(wire, _)| wire == value)
                    .map(|(_, label)| label.clone())
                    .unwrap_or_else(|| value.clone())
            })
            .unwrap_or_else(|| "—".to_string());

        Button::new(id)
            .ghost()
            .xsmall()
            .child(div().text_xs().child(format!("{name}: {current_label}")))
            // Anchored bottom-left so the menu opens upward — the row sits at
            // the bottom edge of the pane.
            .dropdown_menu_with_anchor(gpui::Anchor::BottomLeft, move |menu, _, _| {
                let mut menu = menu;

                if options.is_empty() {
                    menu = menu.label("loading…");
                }

                for (value, label) in options.clone() {
                    let pane = pane.clone();
                    menu = menu.item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
                        pane.update(cx, |this, cx| {
                            set(this, value.clone());
                            cx.notify();
                        });
                    }));
                }

                menu
            })
    }
}

/// Last `n` lines of a live output stream, for the command card.
fn tail_lines(output: &str, n: usize) -> String {
    let lines: Vec<&str> = output.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}
