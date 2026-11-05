//! A definition open on an editable canvas: its edit history, its file, and
//! what other programs did to that file while it was open.
//!
//! The file stays the definition. The editor remembers the bytes it last
//! read or wrote, so its own save is told apart from another program's
//! write by content, not by timing.

#[cfg(test)]
#[path = "editor_tests.rs"]
mod editor_tests;

use std::path::PathBuf;
use std::time::Duration;
use std::{fs, io};

use gpui::prelude::*;
use gpui::{
    AnyElement, Context, Entity, EventEmitter, FontWeight, KeyDownEvent, Render, StyleRefinement,
    Subscription, Task, Window, div, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use nmt_agent::agent_spec::ProfileReference;
use nmt_agent::orchestration::canonical::to_canonical_json;
use nmt_agent::orchestration::definition::Definition;
use nmt_agent::orchestration::edit::{Edit, Editor};
use nmt_agent::orchestration::graph::DefinitionError;
use nmt_agent::orchestration::placement::place;
use nmt_agent::session::AgentKind;
use nmt_platform::durable_file;
use rust_i18n::t;

use crate::agent_tab::orchestration::canvas::{
    CanvasEvent, CanvasNode, GraphCanvas, Selection, dependency_edges,
};
use crate::agent_tab::orchestration::properties::{Choice, Field, Properties};
use crate::agent_tab::settings::AgentSettings;

/// How long the notice of a silent reload stays.
const RELOAD_NOTICE: Duration = Duration::from_secs(4);

pub(super) enum EditorEvent {
    /// The file was written, so the definitions list reads it again.
    Saved,
    /// The file stopped decoding while there was nothing unsaved to keep,
    /// so the definition can no longer be shown on a canvas.
    Undecodable,
}

/// The file changed under unsaved edits.
enum Conflict {
    /// Another program wrote it; `None` when what it wrote does not decode.
    Changed(Option<Definition>),
    Deleted,
}

pub(super) struct DefinitionEditor {
    name: String,
    path: PathBuf,
    editor: Editor,
    errors: Vec<DefinitionError>,
    canvas: Entity<GraphCanvas>,
    properties: Properties,

    /// The file's bytes as last read or written; a file holding these is
    /// unchanged, whoever reports it.
    disk: Option<Vec<u8>>,

    conflict: Option<Conflict>,

    /// Counts saves started. A read begun before the latest save may
    /// return the bytes that save replaced, so its result is dropped; the
    /// save's own file event starts a fresh read.
    saves: u64,

    writing: bool,
    save_error: Option<String>,

    /// Clears the notice of a reload no one was asked about; dropping it
    /// leaves no notice shown.
    reload_notice: Option<Task<()>>,

    _canvas_events: Subscription,
}

impl EventEmitter<EditorEvent> for DefinitionEditor {}

impl DefinitionEditor {
    pub(super) fn new(
        name: String,
        path: PathBuf,
        definition: Definition,
        cx: &mut Context<Self>,
    ) -> Self {
        let canvas = cx.new(|cx| GraphCanvas::new(true, cx));

        let canvas_events = cx.subscribe(&canvas, |this, _, event, cx| this.on_canvas(event, cx));

        let mut editor = Self {
            name,
            path,
            editor: Editor::open(definition),
            errors: Vec::new(),
            canvas,
            properties: Properties::new(),
            disk: None,
            conflict: None,
            saves: 0,
            writing: false,
            save_error: None,
            reload_notice: None,
            _canvas_events: canvas_events,
        };

        editor.validate(cx);

        editor.check_file(cx);

        editor
    }

    pub(super) fn is_dirty(&self) -> bool {
        self.editor.is_dirty()
    }

    fn validate(&mut self, cx: &mut Context<Self>) {
        self.errors = self.editor.errors();

        let definition = self.editor.definition();
        let edges = dependency_edges(definition);

        let marked: Vec<&str> = self
            .errors
            .iter()
            .flat_map(DefinitionError::nodes)
            .collect();

        let nodes: Vec<CanvasNode> = place(definition)
            .into_iter()
            .zip(&definition.nodes)
            .map(|(position, node)| CanvasNode {
                id: node.id.clone(),
                position,
                state: None,
                marked: marked.contains(&node.id.as_str()),
            })
            .collect();

        let key = format!("definition:{}", self.name);

        self.canvas
            .update(cx, |canvas, cx| canvas.show(&key, nodes, edges, cx));
    }

    /// Apply `edit`; returns whether it changed the definition.
    fn apply(&mut self, edit: Edit, cx: &mut Context<Self>) -> bool {
        if !self.editor.apply(edit) {
            return false;
        }

        self.validate(cx);

        cx.notify();

        true
    }

    fn node_id(&self, node: usize) -> Option<String> {
        self.editor
            .definition()
            .nodes
            .get(node)
            .map(|node| node.id.clone())
    }

    fn on_canvas(&mut self, event: &CanvasEvent, cx: &mut Context<Self>) {
        match *event {
            CanvasEvent::OpenDetails(_) => {}
            CanvasEvent::Selected => cx.notify(),
            CanvasEvent::Moved { node, to } => {
                if let Some(id) = self.node_id(node) {
                    self.apply(Edit::MoveNodes(vec![(id, to)]), cx);
                }
            }
            CanvasEvent::Connect { from, to } => {
                if let (Some(from), Some(to)) = (self.node_id(from), self.node_id(to)) {
                    self.apply(Edit::Connect { from, to }, cx);
                }
            }
            CanvasEvent::Delete(Selection::Node(node)) => {
                if let Some(id) = self.node_id(node) {
                    self.apply(Edit::RemoveNodes(vec![id]), cx);
                }
            }
            CanvasEvent::Delete(Selection::Edge(edge)) => {
                let edges = dependency_edges(self.editor.definition());

                if let Some(&(from, to)) = edges.get(edge)
                    && let (Some(from), Some(to)) = (self.node_id(from), self.node_id(to))
                {
                    self.apply(Edit::Disconnect { from, to }, cx);
                }
            }
        }
    }

    /// Apply what a property field holds; a field whose value the
    /// definition does not take is refilled with the definition's value.
    pub(super) fn commit(&mut self, field: Field, cx: &mut Context<Self>) {
        let edit = self
            .properties
            .edit_for(field, self.editor.definition(), cx);

        let renamed_slot = match &edit {
            Some(Edit::RenameSlot { to, .. }) => Some(to.clone()),
            _ => None,
        };

        if !edit.is_some_and(|edit| self.apply(edit, cx)) {
            self.properties.refill(field);

            cx.notify();

            return;
        }

        if renamed_slot.is_some() {
            self.properties.select_slot(renamed_slot);
        }
    }

    pub(super) fn choose(&mut self, choice: Choice, cx: &mut Context<Self>) {
        if let Some(edit) = self
            .properties
            .edit_for_choice(choice, self.editor.definition())
        {
            self.apply(edit, cx);
        }
    }

    pub(super) fn show_slot(&mut self, name: String, cx: &mut Context<Self>) {
        self.properties.select_slot(Some(name));

        cx.notify();
    }

    /// Add a slot running the first saved profile, which the user then
    /// changes in the panel.
    pub(super) fn add_slot(&mut self, cx: &mut Context<Self>) {
        let profile = cx.global::<AgentSettings>().profiles.first().map_or_else(
            || ProfileReference {
                kind: AgentKind::Claude,
                name: AgentKind::Claude.full_name().to_owned(),
            },
            |profile| ProfileReference {
                kind: profile.kind,
                name: profile.name.clone(),
            },
        );

        let name = self.editor.unused_slot_name();

        if self.apply(
            Edit::AddSlot {
                name: name.clone(),
                profile,
            },
            cx,
        ) {
            self.properties.select_slot(Some(name));
        }
    }

    pub(super) fn remove_slot(&mut self, cx: &mut Context<Self>) {
        if let Some(name) = self.properties.slot().map(str::to_owned) {
            self.apply(Edit::RemoveSlot(name), cx);
        }
    }

    fn add_node(&mut self, cx: &mut Context<Self>) {
        let position = self.canvas.read(cx).visible_center();
        let edit = self.editor.new_node(position);

        if self.apply(edit, cx) {
            let added = self.editor.definition().nodes.len() - 1;

            self.canvas.update(cx, |canvas, cx| {
                canvas.select(Some(Selection::Node(added)), cx)
            });
        }
    }

    /// Undo or redo; the selection is dropped because indices may now name
    /// other nodes.
    fn step_history(&mut self, forward: bool, cx: &mut Context<Self>) {
        let stepped = if forward {
            self.editor.redo()
        } else {
            self.editor.undo()
        };

        if stepped {
            self.validate(cx);

            self.canvas.update(cx, |canvas, cx| canvas.select(None, cx));

            cx.notify();
        }
    }

    /// Write the definition in the canonical format. The task completes
    /// with whether the file was written.
    pub(super) fn save(&mut self, cx: &mut Context<Self>) -> Task<bool> {
        if self.writing {
            return Task::ready(false);
        }

        // A field being typed in has not lost focus yet, so its value is
        // applied here or the save would miss it.
        for field in Field::in_commit_order() {
            self.commit(field, cx);
        }

        let definition = self.editor.definition().clone();
        let bytes = to_canonical_json(&definition).into_bytes();
        let path = self.path.clone();

        self.saves += 1;
        self.writing = true;

        cx.notify();

        let write = cx.background_executor().spawn({
            let bytes = bytes.clone();

            async move { durable_file::write(&path, &bytes) }
        });

        cx.spawn(async move |this, cx| {
            let result = write.await;

            this.update(cx, |this, cx| {
                this.writing = false;

                let written = match result {
                    Ok(()) => {
                        this.editor.mark_saved(definition);

                        this.disk = Some(bytes);
                        this.conflict = None;
                        this.save_error = None;

                        cx.emit(EditorEvent::Saved);

                        true
                    }
                    Err(error) => {
                        this.save_error = Some(error.to_string());

                        false
                    }
                };

                cx.notify();

                written
            })
            .unwrap_or(false)
        })
    }

    /// Read the file again and reconcile it with the canvas: nothing when
    /// it holds the known bytes, a reload when nothing is unsaved, and a
    /// conflict to resolve otherwise.
    pub(super) fn check_file(&mut self, cx: &mut Context<Self>) {
        let path = self.path.clone();
        let saves = self.saves;

        let read = cx.background_executor().spawn(async move {
            match fs::read(&path) {
                Ok(bytes) => Ok(Some(bytes)),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(error),
            }
        });

        cx.spawn(async move |this, cx| {
            let result = read.await;

            let _ = this.update(cx, |this, cx| {
                if this.writing || this.saves != saves {
                    return;
                }

                match result {
                    Ok(Some(bytes)) => this.file_read(bytes, cx),
                    Ok(None) => this.file_deleted(cx),
                    Err(error) => tracing::warn!("cannot read orchestration definition: {error}"),
                }
            });
        })
        .detach();
    }

    fn file_read(&mut self, bytes: Vec<u8>, cx: &mut Context<Self>) {
        if self.disk.as_ref() == Some(&bytes) {
            return;
        }

        let decoded = serde_json::from_slice::<Definition>(&bytes).ok();

        self.disk = Some(bytes);

        // A file holding the saved version needs nothing, whatever its
        // formatting; this is also how the first read after opening goes.
        if decoded.as_ref() == Some(self.editor.saved()) {
            self.conflict = None;

            cx.notify();

            return;
        }

        if self.editor.is_dirty() {
            self.conflict = Some(Conflict::Changed(decoded));

            cx.notify();

            return;
        }

        match decoded {
            Some(definition) => {
                self.reload(definition, cx);

                self.show_reload_notice(cx);
            }
            None => cx.emit(EditorEvent::Undecodable),
        }
    }

    fn file_deleted(&mut self, cx: &mut Context<Self>) {
        if matches!(self.conflict, Some(Conflict::Deleted)) {
            return;
        }

        self.disk = None;
        self.conflict = Some(Conflict::Deleted);

        cx.notify();
    }

    /// Replace the canvas's definition with the file's, keeping the pan and
    /// zoom because the graph shown keeps its name.
    fn reload(&mut self, definition: Definition, cx: &mut Context<Self>) {
        self.editor.reload(definition);

        self.conflict = None;

        self.validate(cx);

        self.canvas.update(cx, |canvas, cx| canvas.select(None, cx));

        cx.notify();
    }

    /// Say for a few seconds that the canvas now shows another program's
    /// version, since nothing else marks a reload that asked nothing.
    fn show_reload_notice(&mut self, cx: &mut Context<Self>) {
        self.reload_notice = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(RELOAD_NOTICE).await;

            let _ = this.update(cx, |this, cx| {
                this.reload_notice = None;

                cx.notify();
            });
        }));
    }

    fn reload_from_conflict(&mut self, cx: &mut Context<Self>) {
        match self.conflict.take() {
            Some(Conflict::Changed(Some(definition))) => self.reload(definition, cx),
            Some(Conflict::Changed(None)) => cx.emit(EditorEvent::Undecodable),
            Some(Conflict::Deleted) | None => {}
        }
    }

    fn keep_mine(&mut self, cx: &mut Context<Self>) {
        self.conflict = None;

        cx.notify();
    }

    fn on_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;

        if !keystroke.modifiers.secondary() || keystroke.modifiers.alt {
            return;
        }

        match keystroke.key.as_str() {
            "z" => self.step_history(keystroke.modifiers.shift, cx),
            "y" if !keystroke.modifiers.shift => self.step_history(true, cx),
            "s" if !keystroke.modifiers.shift => self.save(cx).detach(),
            _ => return,
        }

        cx.stop_propagation();
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();

        h_flex()
            .gap_1()
            .p_2()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(self.name.clone()),
            )
            .children(self.reload_notice.is_some().then(|| {
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(t!("orchestration-reloaded").into_owned())
            }))
            .children(self.editor.is_dirty().then(|| {
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(t!("orchestration-unsaved").into_owned())
            }))
            .child(
                Button::new("orchestration-add-node")
                    .ghost()
                    .small()
                    .label(t!("orchestration-add-node"))
                    .on_click(cx.listener(|this, _, _, cx| this.add_node(cx))),
            )
            .child(
                Button::new("orchestration-undo")
                    .ghost()
                    .small()
                    .label(t!("orchestration-undo"))
                    .disabled(!self.editor.can_undo())
                    .on_click(cx.listener(|this, _, _, cx| this.step_history(false, cx))),
            )
            .child(
                Button::new("orchestration-redo")
                    .ghost()
                    .small()
                    .label(t!("orchestration-redo"))
                    .disabled(!self.editor.can_redo())
                    .on_click(cx.listener(|this, _, _, cx| this.step_history(true, cx))),
            )
            .child(
                Button::new("orchestration-save")
                    .primary()
                    .small()
                    .label(t!("orchestration-save"))
                    .disabled(self.writing)
                    .on_click(cx.listener(|this, _, _, cx| this.save(cx).detach())),
            )
    }

    fn render_conflict(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = cx.theme();

        let message = match self.conflict.as_ref()? {
            Conflict::Changed(_) => t!("orchestration-file-changed"),
            Conflict::Deleted => t!("orchestration-file-deleted"),
        };

        let changed = matches!(self.conflict, Some(Conflict::Changed(_)));

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
                        .child(message.into_owned()),
                )
                .when(changed, |banner| {
                    banner
                        .child(
                            Button::new("orchestration-keep-mine")
                                .ghost()
                                .small()
                                .label(t!("orchestration-keep-mine"))
                                .on_click(cx.listener(|this, _, _, cx| this.keep_mine(cx))),
                        )
                        .child(
                            Button::new("orchestration-reload")
                                .small()
                                .label(t!("orchestration-reload"))
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.reload_from_conflict(cx)),
                                ),
                        )
                })
                .into_any_element(),
        )
    }
}

impl Render for DefinitionEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = match self.canvas.read(cx).selection() {
            Some(Selection::Node(node)) => Some(node),
            Some(Selection::Edge(_)) | None => None,
        };

        let panel =
            self.properties
                .render(self.editor.definition(), &self.errors, selected, window, cx);

        let danger = cx.theme().danger;

        let errors: Vec<String> = self
            .save_error
            .iter()
            .cloned()
            .chain(self.errors.iter().map(ToString::to_string))
            .collect();

        v_flex()
            .size_full()
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| this.on_key(event, cx)))
            .child(self.render_header(cx))
            .children(self.render_conflict(cx))
            .when(!errors.is_empty(), |editor| {
                editor.child(
                    v_flex()
                        .id("orchestration-errors")
                        .px_2()
                        .py_1()
                        .gap_0p5()
                        .max_h(px(96.))
                        .overflow_y_scroll()
                        .children(
                            errors
                                .into_iter()
                                .map(|error| div().text_xs().text_color(danger).child(error)),
                        ),
                )
            })
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div().flex_1().min_w_0().h_full().child(
                            self.canvas
                                .clone()
                                .cached(StyleRefinement::default().size_full()),
                        ),
                    )
                    .child(panel),
            )
    }
}
