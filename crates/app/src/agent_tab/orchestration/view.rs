//! The Orchestration tab: the saved definitions, the workspace's recent runs,
//! and the selected run as a graph of node states with a detail view per
//! node.

#[cfg(test)]
#[path = "view_tests.rs"]
mod view_tests;

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures::StreamExt as _;
use futures::channel::mpsc::unbounded;
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, Entity, EntityId, FocusHandle, Focusable, FontWeight, Hsla, Render,
    Role, StyleRefinement, Subscription, Task, Window, div, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_component::{
    ActiveTheme as _, Disableable as _, IconName, Sizable as _, Theme, h_flex, v_flex,
};
use nmt_agent::AgentWorkspace;
use nmt_agent::orchestration::library::{
    DefinitionEntry, create_definition, definitions_directory, load_definitions,
};
use nmt_agent::orchestration::placement::place;
use nmt_agent::orchestration::run::{NodeRun, NodeState, RunId, RunState};
use nmt_agent::orchestration::store::{RunSummary, recent_runs};
use nmt_agent::session::AgentKind;
use notify::{
    Event as NotifyEvent, RecursiveMode, Result as NotifyResult, Watcher as _, recommended_watcher,
};
use rust_i18n::t;

use crate::agent_tab::AgentPane;
use crate::agent_tab::execution::TranscriptChanged;
use crate::agent_tab::orchestration::canvas::{
    CanvasEvent, CanvasNode, CardState, GraphCanvas, dependency_edges,
};
use crate::agent_tab::orchestration::editor::{DefinitionEditor, EditorEvent};
use crate::agent_tab::orchestration::{OrchestrationRuntime, missing_profile};
use crate::agent_tab::settings::{AgentSettings, UI_RADIUS};
use crate::agent_tab::transcript::{TranscriptView, elapsed_label, relative_time};

pub(super) const SIDEBAR_WIDTH: f32 = 280.;

/// File events arrive in bursts while an editor saves; one reload after the
/// burst is enough.
const RELOAD_DEBOUNCE: Duration = Duration::from_millis(150);

/// What the area beside the definitions list shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MainView {
    Definition,
    Run,
}

/// The node whose turn the detail view shows.
struct NodeDetail {
    node: usize,

    /// Entries shown last, so a growing live turn only rebuilds the rows
    /// from the last one on.
    shown: usize,

    /// A saved turn is read once; a live one is read from its slot session.
    saved_requested: bool,
}

pub struct OrchestrationPane {
    directory: PathBuf,
    workspace: AgentWorkspace,
    focus: FocusHandle,
    input: Entity<TextareaState>,
    definitions: Vec<DefinitionEntry>,
    selected: Option<String>,
    runs: Vec<RunSummary>,
    runtime: Option<Entity<OrchestrationRuntime>>,
    runtime_observer: Option<Subscription>,
    runtime_content_observer: Option<Subscription>,
    content_pending: bool,
    detail: Option<NodeDetail>,
    transcript: Entity<TranscriptView>,
    canvas: Entity<GraphCanvas>,
    main: MainView,

    /// The selected definition on its editable canvas, while it decodes.
    /// It stays while a run is shown, so its unsaved edits wait there.
    editor: Option<(Entity<DefinitionEditor>, Subscription)>,

    /// A definition chosen while the open one has unsaved edits, opened
    /// once those are saved or discarded.
    pending_selection: Option<String>,

    /// The name field of a definition being created, and why the last name
    /// was refused.
    new_definition: Option<(Entity<InputState>, Subscription)>,

    new_definition_error: Option<String>,

    /// One view per slot session, made when first needed and kept while
    /// that session is open: binding a second view to a session would retire
    /// the first one's pending answers.
    slot_panes: BTreeMap<usize, (EntityId, Entity<AgentPane>)>,

    confirm_resume: bool,
    error: Option<String>,
    last_state: Option<RunState>,
    _watcher: Option<Task<()>>,
    _canvas_events: Subscription,
}

impl OrchestrationPane {
    pub fn new(
        directory: PathBuf,
        workspace: AgentWorkspace,
        run: Option<RunId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.on_release(|this, cx| {
            if let Some(runtime) = this.runtime.take() {
                runtime.update(cx, |runtime, cx| runtime.close(cx));
            }
        })
        .detach();

        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 8)
                .placeholder(t!("orchestration-input-placeholder").into_owned())
        });

        let transcript = cx.new(|_| TranscriptView::new(AgentKind::Codex, None));
        let canvas = cx.new(|cx| GraphCanvas::new(false, cx));

        cx.observe_global::<Theme>(|this, cx| this.sync_canvas(cx))
            .detach();

        let canvas_events = cx.subscribe(&canvas, |this, _, event, cx| match event {
            CanvasEvent::OpenDetails(node) if this.main == MainView::Run => {
                this.open_detail(*node, cx)
            }
            CanvasEvent::OpenDetails(_)
            | CanvasEvent::Selected
            | CanvasEvent::Moved { .. }
            | CanvasEvent::Connect { .. }
            | CanvasEvent::Delete(_) => {}
        });

        let mut pane = Self {
            directory,
            workspace,
            focus: cx.focus_handle(),
            input,
            definitions: Vec::new(),
            selected: None,
            runs: Vec::new(),
            runtime: None,
            runtime_observer: None,
            runtime_content_observer: None,
            content_pending: false,
            detail: None,
            transcript,
            canvas,
            main: MainView::Definition,
            editor: None,
            pending_selection: None,
            new_definition: None,
            new_definition_error: None,
            slot_panes: BTreeMap::new(),
            confirm_resume: false,
            error: None,
            last_state: None,
            _watcher: None,
            _canvas_events: canvas_events,
        };

        pane._watcher = pane.watch_definitions(cx);

        pane.reload(cx);

        if let Some(id) = run {
            let runtime = OrchestrationRuntime::open(&pane.directory, id, cx);

            pane.show_runtime(runtime, cx);
        }

        pane
    }

    /// The run this tab shows, for saving the tab.
    pub fn run_id(&self, cx: &App) -> Option<RunId> {
        self.runtime
            .as_ref()
            .and_then(|runtime| runtime.read(cx).run().map(|run| run.id()))
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.input.update(cx, |input, cx| input.focus(window, cx));
    }

    /// Stop the run's turns and release its sessions, as closing the tab
    /// does; the run stays saved.
    pub fn close(&mut self, cx: &mut Context<Self>) {
        if let Some(runtime) = self.runtime.take() {
            runtime.update(cx, |runtime, cx| runtime.close(cx));
        }

        self.runtime_observer = None;
        self.runtime_content_observer = None;
        self.content_pending = false;

        self.slot_panes.clear();
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let directory = self.directory.clone();
        let primary = self.workspace.primary().map(str::to_owned);

        let task = cx.background_executor().spawn(async move {
            let definitions = load_definitions(&directory);
            let runs = recent_runs(&directory, primary.as_deref());

            (definitions, runs)
        });

        cx.spawn(async move |this, cx| {
            let (definitions, runs) = task.await;

            let _ = this.update(cx, |this, cx| {
                match definitions {
                    Ok(definitions) => this.definitions = definitions,
                    Err(error) => this.error = Some(error.to_string()),
                }

                this.sync_editor(cx);

                match runs {
                    Ok(runs) => this.runs = runs,
                    Err(error) => this.error = Some(error.to_string()),
                }

                cx.notify();
            });
        })
        .detach();
    }

    fn watch_definitions(&self, cx: &mut Context<Self>) -> Option<Task<()>> {
        let directory = definitions_directory(&self.directory);

        let (sender, mut receiver) = unbounded();

        let watcher = cx.background_executor().spawn(async move {
            if !directory.exists() {
                return None;
            }

            let mut watcher = recommended_watcher(move |event: NotifyResult<NotifyEvent>| {
                if let Ok(event) = event
                    && (event.kind.is_create() || event.kind.is_modify() || event.kind.is_remove())
                {
                    let _ = sender.unbounded_send(());
                }
            })
            .inspect_err(|error| tracing::warn!("cannot watch orchestration definitions: {error}"))
            .ok()?;

            watcher
                .watch(&directory, RecursiveMode::NonRecursive)
                .inspect_err(|error| {
                    tracing::warn!("cannot watch orchestration definitions: {error}")
                })
                .ok()?;

            Some(watcher)
        });

        Some(cx.spawn(async move |this, cx| {
            let Some(_watcher) = watcher.await else {
                return;
            };

            while receiver.next().await.is_some() {
                cx.background_executor().timer(RELOAD_DEBOUNCE).await;

                while receiver.try_recv().is_ok() {}

                if this.update(cx, |this, cx| this.reload(cx)).is_err() {
                    break;
                }
            }
        }))
    }

    fn open_folder(&mut self, cx: &mut Context<Self>) {
        let directory = definitions_directory(&self.directory);

        if let Err(error) = fs::create_dir_all(&directory) {
            self.error = Some(error.to_string());

            cx.notify();

            return;
        }

        cx.open_with_system(&directory);

        if self._watcher.is_none() {
            self._watcher = self.watch_definitions(cx);
        }
    }

    /// Whether the open definition has edits its file does not hold.
    pub fn has_unsaved_edits(&self, cx: &App) -> bool {
        self.editor
            .as_ref()
            .is_some_and(|(editor, _)| editor.read(cx).is_dirty())
    }

    /// Save the open definition; completes with whether it was written, or
    /// `true` when nothing was open.
    pub fn save_unsaved(&mut self, cx: &mut Context<Self>) -> Task<bool> {
        match &self.editor {
            Some((editor, _)) => editor.update(cx, |editor, cx| editor.save(cx)),
            None => Task::ready(true),
        }
    }

    /// Drop the open definition's unsaved edits; the canvas opens the file
    /// again on the next selection.
    pub fn discard_unsaved(&mut self, cx: &mut Context<Self>) {
        self.editor = None;

        cx.notify();
    }

    fn select_definition(&mut self, name: String, cx: &mut Context<Self>) {
        if self.selected.as_ref() != Some(&name) {
            if self.has_unsaved_edits(cx) {
                self.pending_selection = Some(name);

                cx.notify();

                return;
            }

            self.editor = None;
        }

        self.selected = Some(name);
        self.main = MainView::Definition;
        self.detail = None;
        self.error = None;

        self.sync_editor(cx);

        cx.notify();
    }

    /// Open the selected definition on an editor once it decodes, or have
    /// the open editor compare its file with what it last read or wrote.
    fn sync_editor(&mut self, cx: &mut Context<Self>) {
        if let Some((editor, _)) = &self.editor {
            editor.update(cx, |editor, cx| editor.check_file(cx));

            return;
        }

        let Some(entry) = self
            .definitions
            .iter()
            .find(|entry| Some(&entry.name) == self.selected.as_ref())
        else {
            return;
        };

        let Some(definition) = entry.definition.clone() else {
            return;
        };

        let (name, path) = (entry.name.clone(), entry.path.clone());
        let editor = cx.new(|cx| DefinitionEditor::new(name, path, definition, cx));

        let events = cx.subscribe(&editor, |this, _, event, cx| match event {
            EditorEvent::Saved => this.reload(cx),
            EditorEvent::Undecodable => {
                this.editor = None;

                cx.notify();
            }
        });

        self.editor = Some((editor, events));
    }

    /// Answer the unsaved-edits question asked by `select_definition`.
    fn resolve_pending_selection(&mut self, save: bool, cx: &mut Context<Self>) {
        let Some(name) = self.pending_selection.take() else {
            return;
        };

        if !save {
            self.editor = None;

            self.select_definition(name, cx);

            return;
        }

        let saved = self.save_unsaved(cx);

        cx.spawn(async move |this, cx| {
            if saved.await {
                let _ = this.update(cx, |this, cx| {
                    this.editor = None;

                    this.select_definition(name, cx);
                });
            }
        })
        .detach();
    }

    fn render_pending_selection(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let name = self.pending_selection.as_ref()?;
        let theme = cx.theme();

        Some(
            h_flex()
                .gap_2()
                .p_2()
                .border_b_1()
                .border_color(theme.border)
                .bg(theme.warning.opacity(0.15))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .child(t!("orchestration-unsaved-switch", name = name).into_owned()),
                )
                .child(
                    Button::new("orchestration-switch-cancel")
                        .ghost()
                        .small()
                        .label(t!("orchestration-cancel"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.pending_selection = None;

                            cx.notify();
                        })),
                )
                .child(
                    Button::new("orchestration-switch-discard")
                        .small()
                        .label(t!("orchestration-discard"))
                        .on_click(
                            cx.listener(|this, _, _, cx| this.resolve_pending_selection(false, cx)),
                        ),
                )
                .child(
                    Button::new("orchestration-switch-save")
                        .primary()
                        .small()
                        .label(t!("orchestration-save"))
                        .on_click(
                            cx.listener(|this, _, _, cx| this.resolve_pending_selection(true, cx)),
                        ),
                )
                .into_any_element(),
        )
    }

    fn begin_new_definition(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(t!("orchestration-new-name").into_owned())
        });

        let events = cx.subscribe(&input, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.create_definition(cx);
            }
        });

        input.update(cx, |input, cx| input.focus(window, cx));

        self.new_definition = Some((input, events));
        self.new_definition_error = None;

        cx.notify();
    }

    fn cancel_new_definition(&mut self, cx: &mut Context<Self>) {
        self.new_definition = None;
        self.new_definition_error = None;

        cx.notify();
    }

    fn create_definition(&mut self, cx: &mut Context<Self>) {
        let Some((input, _)) = &self.new_definition else {
            return;
        };

        let name = input.read(cx).text().to_string().trim().to_owned();
        let directory = self.directory.clone();

        let create = cx.background_executor().spawn({
            let name = name.clone();

            async move { create_definition(&directory, &name) }
        });

        cx.spawn(async move |this, cx| {
            let result = create.await;

            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(_) => {
                        this.new_definition = None;
                        this.new_definition_error = None;

                        this.reload(cx);

                        this.select_definition(name, cx);
                    }
                    Err(error) => this.new_definition_error = Some(error.to_string()),
                }

                cx.notify();
            });
        })
        .detach();
    }

    fn render_new_definition(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (input, _) = self.new_definition.as_ref()?;
        let theme = cx.theme();

        Some(
            v_flex()
                .gap_1()
                .child(
                    h_flex()
                        .gap_1()
                        .child(div().flex_1().min_w_0().child(Input::new(input).small()))
                        .child(
                            Button::new("orchestration-new-cancel")
                                .ghost()
                                .xsmall()
                                .label(t!("orchestration-cancel"))
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.cancel_new_definition(cx)),
                                ),
                        )
                        .child(
                            Button::new("orchestration-new-create")
                                .primary()
                                .xsmall()
                                .label(t!("orchestration-create"))
                                .on_click(cx.listener(|this, _, _, cx| this.create_definition(cx))),
                        ),
                )
                .children(
                    self.new_definition_error
                        .clone()
                        .map(|error| div().text_xs().text_color(theme.danger).child(error)),
                )
                .into_any_element(),
        )
    }

    fn start_run(&mut self, cx: &mut Context<Self>) {
        let Some(entry) = self
            .definitions
            .iter()
            .find(|entry| Some(&entry.name) == self.selected.as_ref())
        else {
            return;
        };

        let Ok(graph) = entry.graph.clone() else {
            return;
        };

        let input = self.input.read(cx).text().to_string();

        if graph.uses_input() && input.trim().is_empty() {
            self.error = Some(t!("orchestration-needs-input").into_owned());

            cx.notify();

            return;
        }

        if let Some(missing) = missing_profile(&graph, cx) {
            self.error = Some(
                t!(
                    "orchestration-missing-profile",
                    slot = missing.slot,
                    profile = missing.profile.name
                )
                .into_owned(),
            );

            cx.notify();

            return;
        }

        let name = entry.name.clone();

        let runtime = OrchestrationRuntime::start(
            &self.directory,
            name,
            graph,
            input,
            self.workspace.clone(),
            cx,
        );

        self.error = None;

        self.show_runtime(runtime, cx);
    }

    fn open_run(&mut self, id: RunId, cx: &mut Context<Self>) {
        if self.run_id(cx) == Some(id) {
            self.main = MainView::Run;

            cx.notify();

            return;
        }

        let runtime = OrchestrationRuntime::open(&self.directory, id, cx);

        self.show_runtime(runtime, cx);
    }

    fn show_runtime(&mut self, runtime: Entity<OrchestrationRuntime>, cx: &mut Context<Self>) {
        self.close(cx);

        self.detail = None;
        self.confirm_resume = false;
        self.last_state = None;
        self.main = MainView::Run;

        self.runtime_observer = Some(cx.observe(&runtime, |this, runtime, cx| {
            let state = runtime.read(cx).run().map(|run| run.state());

            // A run that changes state changes its row in the recent list,
            // and a new run joins it.
            if state != this.last_state {
                this.last_state = state;

                this.reload(cx);
            }

            this.sync_canvas(cx);
            this.sync_detail(cx);

            cx.notify();
        }));

        self.runtime_content_observer = Some(cx.subscribe(
            &runtime,
            |this, _, _: &TranscriptChanged, cx| {
                if this.detail.is_some() && !this.content_pending {
                    this.content_pending = true;

                    cx.notify();
                }
            },
        ));

        self.runtime = Some(runtime);

        self.sync_canvas(cx);

        cx.notify();
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        if let Some(runtime) = &self.runtime {
            runtime.update(cx, |runtime, cx| runtime.stop(cx)).detach();
        }
    }

    fn resume(&mut self, cx: &mut Context<Self>) {
        self.confirm_resume = false;

        if let Some(runtime) = &self.runtime {
            runtime
                .update(cx, |runtime, cx| runtime.resume(cx))
                .detach();
        }

        cx.notify();
    }

    fn open_detail(&mut self, node: usize, cx: &mut Context<Self>) {
        self.detail = Some(NodeDetail {
            node,
            shown: 0,
            saved_requested: false,
        });

        self.transcript = cx.new(|_| TranscriptView::new(AgentKind::Codex, None));

        self.sync_detail(cx);

        cx.notify();
    }

    fn close_detail(&mut self, cx: &mut Context<Self>) {
        self.detail = None;

        cx.notify();
    }

    /// Fill the detail transcript: from the slot session while the node
    /// runs, otherwise from the turn saved when it ended.
    fn sync_detail(&mut self, cx: &mut Context<Self>) {
        let (Some(runtime), Some(detail)) = (self.runtime.clone(), self.detail.as_mut()) else {
            return;
        };

        let node = detail.node;

        if let Some(entries) = runtime.read(cx).live_entries(node, cx) {
            let first = detail.shown.saturating_sub(1);

            detail.shown = entries.len();
            detail.saved_requested = false;

            self.transcript.update(cx, |transcript, cx| {
                transcript.show_attributed_entries(entries, HashMap::new(), first, cx)
            });

            return;
        }

        let ended = runtime.read(cx).run().is_some_and(|run| {
            !matches!(
                run.nodes()[node].state,
                NodeState::Waiting | NodeState::Sending { .. } | NodeState::Accepted { .. }
            )
        });

        if !ended || detail.saved_requested {
            return;
        }

        detail.saved_requested = true;

        let read = runtime.update(cx, |runtime, cx| runtime.saved_turn(node, cx));

        cx.spawn(async move |this, cx| {
            let result = read.await;

            let _ = this.update(cx, |this, cx| {
                if this
                    .detail
                    .as_ref()
                    .is_none_or(|detail| detail.node != node)
                {
                    return;
                }

                match result {
                    Ok(Some(entries)) => this.transcript.update(cx, |transcript, cx| {
                        transcript.show_attributed_entries(entries, HashMap::new(), 0, cx)
                    }),
                    Ok(None) => {}
                    Err(error) => this.error = Some(error.to_string()),
                }

                cx.notify();
            });
        })
        .detach();
    }

    /// The view that draws `node`'s slot session's pending approvals and
    /// questions, kept for as long as that session is open.
    fn slot_pane(
        &mut self,
        node: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<AgentPane>> {
        let runtime = self.runtime.clone()?;
        let slot = runtime.read(cx).graph()?.slot_of(node);
        let session = runtime.read(cx).node_session(node)?.entity_id();

        if let Some((bound, pane)) = self.slot_panes.get(&slot)
            && *bound == session
        {
            return Some(pane.clone());
        }

        let pane = runtime.update(cx, |runtime, cx| {
            runtime
                .slot_owner(slot)
                .map(|owner| cx.new(|cx| AgentPane::attach_team_member(owner, window, cx)))
        })?;

        self.slot_panes.insert(slot, (session, pane.clone()));

        Some(pane)
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> AnyElement {
        let new_definition = self.render_new_definition(cx);
        let theme = cx.theme();
        let selected = self.selected.clone();

        let selected_entry = self
            .definitions
            .iter()
            .find(|entry| Some(&entry.name) == selected.as_ref());

        let can_start = selected_entry.is_some_and(|entry| entry.graph.is_ok());
        let dirty = self.has_unsaved_edits(cx);

        let definitions: Vec<AnyElement> = if self.definitions.is_empty() {
            vec![
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(
                        t!(
                            "orchestration-no-definitions",
                            path = definitions_directory(&self.directory).display().to_string()
                        )
                        .into_owned(),
                    )
                    .into_any_element(),
            ]
        } else {
            self.definitions
                .iter()
                .enumerate()
                .map(|(index, entry)| {
                    let name = entry.name.clone();
                    let active = Some(&entry.name) == selected.as_ref();

                    let unsaved = active && dirty;

                    let errors: Vec<String> = match &entry.graph {
                        Err(errors) if active => errors.clone(),
                        _ => Vec::new(),
                    };

                    v_flex()
                        .id(("orchestration-definition", index))
                        .px_2()
                        .py_1()
                        .rounded(UI_RADIUS)
                        .cursor_pointer()
                        .when(active, |this| this.bg(theme.accent))
                        .hover(|this| this.bg(theme.accent))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.select_definition(name.clone(), cx)
                        }))
                        .child(
                            h_flex()
                                .gap_2()
                                .child(
                                    div()
                                        .flex_1()
                                        .truncate()
                                        .text_sm()
                                        .child(entry.name.clone()),
                                )
                                .children(unsaved.then(|| {
                                    div()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(t!("orchestration-unsaved").into_owned())
                                }))
                                .children(entry.graph.is_err().then(|| {
                                    div()
                                        .text_xs()
                                        .text_color(theme.danger)
                                        .child(t!("orchestration-invalid").into_owned())
                                })),
                        )
                        .children(
                            errors
                                .into_iter()
                                .map(|error| div().text_xs().text_color(theme.danger).child(error)),
                        )
                        .into_any_element()
                })
                .collect()
        };

        let runs: Vec<AnyElement> = if self.runs.is_empty() {
            vec![
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(t!("orchestration-no-runs").into_owned())
                    .into_any_element(),
            ]
        } else {
            let current = self.run_id(cx);

            self.runs
                .iter()
                .enumerate()
                .map(|(index, summary)| {
                    let id = summary.id;
                    let started = UNIX_EPOCH + Duration::from_millis(summary.started_at);

                    h_flex()
                        .id(("orchestration-run", index))
                        .px_2()
                        .py_1()
                        .gap_2()
                        .rounded(UI_RADIUS)
                        .cursor_pointer()
                        .when(current == Some(id), |this| this.bg(theme.accent))
                        .hover(|this| this.bg(theme.accent))
                        .on_click(cx.listener(move |this, _, _, cx| this.open_run(id, cx)))
                        .child(
                            div()
                                .flex_1()
                                .truncate()
                                .text_sm()
                                .child(summary.definition_name.clone()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(run_state_label(summary.state)),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(relative_time(started)),
                        )
                        .into_any_element()
                })
                .collect()
        };

        v_flex()
            .w(px(SIDEBAR_WIDTH))
            .h_full()
            .flex_none()
            .p_2()
            .gap_2()
            .border_r_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(t!("orchestration-definitions").into_owned()),
                    )
                    .child(
                        Button::new("orchestration-new-definition")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Plus)
                            .label(t!("orchestration-new-definition"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.begin_new_definition(window, cx)
                            })),
                    )
                    .child(
                        Button::new("orchestration-open-folder")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Folder)
                            .label(t!("orchestration-open-folder"))
                            .on_click(cx.listener(|this, _, _, cx| this.open_folder(cx))),
                    ),
            )
            .children(new_definition)
            .child(
                v_flex()
                    .id("orchestration-definitions")
                    .gap_0p5()
                    .max_h(px(260.))
                    .overflow_y_scroll()
                    .children(definitions),
            )
            .child(Textarea::new(&self.input))
            .child(
                Button::new("orchestration-start")
                    .primary()
                    .small()
                    .label(t!("orchestration-start"))
                    .disabled(!can_start)
                    .on_click(cx.listener(|this, _, _, cx| this.start_run(cx))),
            )
            .child(
                div()
                    .pt_2()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(t!("orchestration-recent-runs").into_owned()),
            )
            .child(
                v_flex()
                    .id("orchestration-runs")
                    .flex_1()
                    .min_h_0()
                    .gap_0p5()
                    .overflow_y_scroll()
                    .children(runs),
            )
            .into_any_element()
    }

    fn render_main(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        match self.main {
            MainView::Run => self.render_run(window, cx),
            MainView::Definition => self.render_definition(cx),
        }
    }

    fn sync_canvas(&mut self, cx: &mut Context<Self>) {
        let Some(runtime) = &self.runtime else {
            return;
        };

        let runtime = runtime.read(cx);

        let (Some(run), Some(graph)) = (runtime.run(), runtime.graph()) else {
            return;
        };

        let key = format!("run:{}", run.id());
        let edges = dependency_edges(run.definition());

        let nodes = place(run.definition())
            .into_iter()
            .enumerate()
            .map(|(node, position)| {
                let state = &run.nodes()[node].state;

                CanvasNode {
                    id: graph.definition().nodes[node].id.clone(),
                    position,
                    state: Some(CardState {
                        label: node_state_label(state),
                        color: node_state_color(state, cx),
                        needs_input: runtime.needs_input(node, cx),
                    }),
                    marked: false,
                }
            })
            .collect();

        self.canvas
            .update(cx, |canvas, cx| canvas.show(&key, nodes, edges, cx));
    }

    fn render_run(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(runtime) = self.runtime.clone() else {
            return placeholder(
                t!("orchestration-empty-title").into_owned(),
                t!("orchestration-empty-detail").into_owned(),
                cx,
            );
        };

        if let Some(node) = self.detail.as_ref().map(|detail| detail.node) {
            return self.render_detail(&runtime, node, window, cx);
        }

        let runtime_ref = runtime.read(cx);

        let (Some(run), Some(_)) = (runtime_ref.run(), runtime_ref.graph()) else {
            let message = runtime_ref
                .error()
                .map(str::to_owned)
                .unwrap_or_else(|| t!("orchestration-loading").into_owned());

            return placeholder(message, String::new(), cx);
        };

        let stoppable = runtime_ref.stoppable();
        let resumable = runtime_ref.resumable();
        let name = run.definition_name().to_owned();
        let input = run.input().lines().next().unwrap_or_default().to_owned();
        let state = run_state_label(run.state());

        let theme = cx.theme();

        let header = h_flex()
            .gap_2()
            .p_2()
            .border_b_1()
            .border_color(theme.border)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(name),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .truncate()
                            .child(input),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(state),
            )
            .children(stoppable.then(|| {
                Button::new("orchestration-stop")
                    .small()
                    .label(t!("orchestration-stop"))
                    .on_click(cx.listener(|this, _, _, cx| this.stop(cx)))
            }))
            .children((resumable && !self.confirm_resume).then(|| {
                Button::new("orchestration-resume")
                    .small()
                    .label(t!("orchestration-resume"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.confirm_resume = true;

                        cx.notify();
                    }))
            }));

        let confirmation = (resumable && self.confirm_resume).then(|| {
            h_flex()
                .gap_2()
                .p_2()
                .border_b_1()
                .border_color(theme.border)
                .child(
                    // Without a zero minimum width the text keeps its full
                    // line length and pushes the buttons out of a narrow pane.
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .child(t!("orchestration-resume-warning").into_owned()),
                )
                .child(
                    Button::new("orchestration-resume-cancel")
                        .ghost()
                        .small()
                        .label(t!("orchestration-cancel"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.confirm_resume = false;

                            cx.notify();
                        })),
                )
                .child(
                    Button::new("orchestration-resume-confirm")
                        .primary()
                        .small()
                        .label(t!("orchestration-resume-confirm"))
                        .on_click(cx.listener(|this, _, _, cx| this.resume(cx))),
                )
        });

        v_flex()
            .size_full()
            .child(header)
            .children(confirmation)
            .child(
                div().flex_1().min_h_0().child(
                    self.canvas
                        .clone()
                        .cached(StyleRefinement::default().size_full()),
                ),
            )
            .into_any_element()
    }

    /// The selected definition on its editor, or its decoding error with a
    /// way to open the file when it does not decode.
    fn render_definition(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if let Some((editor, _)) = &self.editor {
            return editor.clone().into_any_element();
        }

        let Some(entry) = self
            .definitions
            .iter()
            .find(|entry| Some(&entry.name) == self.selected.as_ref())
        else {
            return placeholder(
                t!("orchestration-empty-title").into_owned(),
                t!("orchestration-empty-detail").into_owned(),
                cx,
            );
        };

        let name = entry.name.clone();
        let path = entry.path.clone();
        let errors = entry.graph.as_ref().err().cloned().unwrap_or_default();
        let theme = cx.theme();

        v_flex()
            .size_full()
            .p_3()
            .gap_2()
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(name),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(t!("orchestration-undecodable").into_owned()),
            )
            .children(
                errors
                    .into_iter()
                    .map(|error| div().text_xs().text_color(theme.danger).child(error)),
            )
            .child(
                Button::new("orchestration-open-file")
                    .small()
                    .label(t!("orchestration-open-file"))
                    .on_click(cx.listener(move |_, _, _, cx| cx.open_with_system(&path))),
            )
            .into_any_element()
    }

    fn render_detail(
        &mut self,
        runtime: &Entity<OrchestrationRuntime>,
        node: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let interactions = match self.slot_pane(node, window, cx) {
            Some(pane) if runtime.read(cx).needs_input(node, cx) => {
                pane.update(cx, |pane, cx| pane.render_team_interactions(window, cx))
            }
            _ => Vec::new(),
        };

        let theme = cx.theme();
        let runtime_ref = runtime.read(cx);

        let (Some(run), Some(graph)) = (runtime_ref.run(), runtime_ref.graph()) else {
            return div().into_any_element();
        };

        let definition = graph.definition();
        let record = &run.nodes()[node];
        let slot = &definition.slots[graph.slot_of(node)].name;

        let note = match &record.state {
            NodeState::Failed { reason } => {
                Some(t!("orchestration-failure", reason = reason).into_owned())
            }
            NodeState::Waiting | NodeState::NotRun => {
                Some(t!("orchestration-not-run").into_owned())
            }
            _ => None,
        };

        let header = v_flex()
            .px_2()
            .py_1()
            .gap_0p5()
            .border_b_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(
                        Button::new("orchestration-detail-back")
                            .ghost()
                            .small()
                            .icon(IconName::ArrowLeft)
                            .tooltip(t!("orchestration-back"))
                            .on_click(cx.listener(|this, _, _, cx| this.close_detail(cx))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .truncate()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(definition.nodes[node].id.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(node_state_color(&record.state, cx))
                            .child(node_state_label(&record.state)),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(t!("orchestration-slot", slot = slot).into_owned())
                    .children(elapsed(record).map(elapsed_label)),
            );

        v_flex()
            .size_full()
            .child(header)
            .children(note.map(|note| {
                div()
                    .px_2()
                    .py_1()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(note)
            }))
            .child(v_flex().flex_1().min_h_0().child(self.transcript.clone()))
            .children((!interactions.is_empty()).then(|| {
                v_flex()
                    .p_2()
                    .gap_2()
                    .border_t_1()
                    .border_color(theme.border)
                    .children(interactions)
            }))
            .into_any_element()
    }
}

impl Focusable for OrchestrationPane {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for OrchestrationPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.content_pending {
            self.content_pending = false;

            self.sync_detail(cx);
        }

        let settings = cx.global::<AgentSettings>();

        let background = if settings.pane_background_follows_terminal {
            settings.terminal_background
        } else {
            cx.theme().sidebar
        };

        let font = settings.font();
        let font_size = settings.font_size;
        let background_opacity = settings.background_opacity;
        let error = self.error.clone();
        let theme_danger = cx.theme().danger;
        let pending_selection = self.render_pending_selection(cx);

        h_flex()
            .id("orchestration-surface")
            .debug_selector(|| "orchestration-surface".into())
            .role(Role::Pane)
            .size_full()
            .min_h_0()
            .bg(background.alpha(background_opacity))
            .overflow_hidden()
            .font(font)
            .text_size(px(font_size))
            .track_focus(&self.focus)
            .child(self.render_sidebar(cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .children(error.map(|error| {
                        div()
                            .px_2()
                            .py_1()
                            .text_xs()
                            .text_color(theme_danger)
                            .child(error)
                    }))
                    .children(pending_selection)
                    .child(self.render_main(window, cx)),
            )
    }
}

fn placeholder(title: String, detail: String, cx: &App) -> AnyElement {
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .gap_1()
        .child(div().text_sm().child(title))
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(detail),
        )
        .into_any_element()
}

/// Seconds the node has run, or ran, once it started.
fn elapsed(record: &NodeRun) -> Option<u64> {
    let started = record.started_at?;

    let finished = record.finished_at.unwrap_or_else(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(started, |now| now.as_millis() as u64)
    });

    Some(finished.saturating_sub(started) / 1000)
}

fn node_state_label(state: &NodeState) -> String {
    match state {
        NodeState::Waiting => t!("orchestration-node-waiting"),
        NodeState::Sending { .. } => t!("orchestration-node-starting"),
        NodeState::Accepted { .. } => t!("orchestration-node-running"),
        NodeState::Completed { .. } => t!("orchestration-node-completed"),
        NodeState::Failed { .. } => t!("orchestration-node-failed"),
        NodeState::Stopped => t!("orchestration-node-stopped"),
        NodeState::Interrupted => t!("orchestration-node-interrupted"),
        NodeState::NotRun => t!("orchestration-node-not-run"),
    }
    .into_owned()
}

fn node_state_color(state: &NodeState, cx: &App) -> Hsla {
    let theme = cx.theme();

    match state {
        NodeState::Completed { .. } => theme.success,
        NodeState::Failed { .. } => theme.danger,
        NodeState::Sending { .. } | NodeState::Accepted { .. } => theme.primary,
        NodeState::Waiting | NodeState::Stopped | NodeState::Interrupted | NodeState::NotRun => {
            theme.muted_foreground
        }
    }
}

fn run_state_label(state: RunState) -> String {
    match state {
        RunState::Running => t!("orchestration-run-running"),
        RunState::Failing => t!("orchestration-run-failing"),
        RunState::Completed => t!("orchestration-run-completed"),
        RunState::Failed => t!("orchestration-run-failed"),
        RunState::Stopped => t!("orchestration-run-stopped"),
        RunState::Interrupted => t!("orchestration-run-interrupted"),
    }
    .into_owned()
}
