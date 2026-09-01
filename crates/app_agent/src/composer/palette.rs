use nmt_i18n::i18n;

use crate::composer::{CommandFeedbackKind, ForkState, RewindAction, RewindState};
use crate::input_history::InputHistoryDirection;
use crate::*;

/// Tallest the palette grows before its own rows scroll: nine rows and the
/// note under them. The transcript reads this as the height the picker covers
/// when it floats over the bottom of the pane.
pub(crate) const PALETTE_MAX_HEIGHT: Pixels = px(9. * 48. + 36.);

#[derive(Clone)]
pub(crate) enum PaletteAction {
    Command(SlashCommandInfo),
    Choice { command: String, value: String },
    Skill(SkillInfo),
    RewindCheckpoint(sessions::ClaudeCheckpoint),
    RewindAction(RewindAction),
    ForkCheckpoint(ForkCheckpoint),
    ForkCancel,
}

/// One drawn palette row. The text is `SharedString` because the model is
/// rebuilt on every frame the palette paints: catalog text taken straight from
/// the translation catalogs borrows instead of copying, and text that is
/// composed still reaches `child` without a second copy.
#[derive(Clone)]
pub(crate) struct PaletteRow {
    pub(crate) label: SharedString,
    pub(crate) description: SharedString,
    pub(crate) hint: Option<SharedString>,
    pub(crate) disabled_reason: Option<SharedString>,
    pub(crate) action: PaletteAction,
}

pub(crate) struct PaletteModel {
    pub(crate) rows: Vec<PaletteRow>,
    pub(crate) note: Option<SharedString>,
}

#[derive(Clone, Copy)]
pub(crate) enum PaletteControl {
    Previous,
    Next,
    Activate,
    Complete,
    Dismiss,
}

impl PaletteControl {
    /// Which way this control moves a highlighted row, or `None` where it moves
    /// none. Several lists take the same keys — the command palette, the recent
    /// conversations, the rewind and fork pickers — so the reading lives here
    /// rather than beside each list that acts on it.
    fn direction(self) -> Option<PaletteDirection> {
        match self {
            PaletteControl::Previous => Some(PaletteDirection::Previous),
            PaletteControl::Next => Some(PaletteDirection::Next),
            PaletteControl::Activate | PaletteControl::Complete | PaletteControl::Dismiss => None,
        }
    }
}

impl AgentPane {
    pub(crate) fn open_recent_sessions(&mut self, cx: &mut Context<Self>) -> bool {
        if self.is_command_busy() {
            self.set_command_feedback(
                CommandFeedbackKind::Error,
                translated("agent-composer-resume-idle-only"),
                cx,
            );
            return false;
        }

        let rows = self
            .history_ui
            .pending
            .unwrap_or(self.history_ui.sessions.len());
        if rows == 0 {
            self.history_ui.mode = RecentSessionsMode::Hidden;
            self.set_command_feedback(
                CommandFeedbackKind::Notice,
                translated("agent-composer-no-recent-sessions"),
                cx,
            );
            return true;
        }

        self.history_ui.mode = RecentSessionsMode::Open;
        self.history_ui.selected = 0;
        // A list opened from a command was opened without the pointer, and a
        // strip that was on screen the last time the pointer crossed it has
        // no way to report that the pointer has since left.
        self.history_ui.pointer_inside = false;
        self.history_ui.pointer = None;
        self.palette.feedback = None;
        cx.notify();
        true
    }

    /// Rows for a skill query, shared by the `/` picker stage and the `$`
    /// prefix. Discovery runs in the background, so a missing catalog is a
    /// loading state rather than an empty result.
    fn skill_palette_model(&self, query: &str) -> PaletteModel {
        let Some(skill_catalog) = self.palette.skill_catalog.as_ref() else {
            return PaletteModel {
                rows: Vec::new(),
                note: Some(translated("agent-composer-skill-discovery-loading")),
            };
        };

        let rows = filter_skill_catalog(&skill_catalog.skills, query)
            .into_iter()
            .map(|skill| PaletteRow {
                label: format!("${}", skill.name).into(),
                description: SharedString::new(&skill.description),
                hint: Some(SharedString::new(&skill.scope)),
                disabled_reason: self.skill_disabled_reason(&skill),
                action: PaletteAction::Skill(skill),
            })
            .collect::<Vec<_>>();
        let note = if rows.is_empty() && !skill_catalog.errors.is_empty() {
            Some(SharedString::new(&skill_catalog.errors[0]))
        } else if rows.is_empty() && query.is_empty() {
            Some(translated("agent-composer-no-skills"))
        } else if rows.is_empty() {
            Some(translated("agent-composer-no-matching-skills"))
        } else {
            skill_catalog.errors.first().map(|error| {
                i18n("agent-composer-skill-load-partial")
                    .replace("{error}", error)
                    .into()
            })
        };

        PaletteModel { rows, note }
    }

    pub(crate) fn palette_model(&mut self, cx: &Context<Self>) -> Option<PaletteModel> {
        if let Some(state) = self.rewind.state.as_ref() {
            return self.rewind_palette_model(state);
        }
        if let Some(state) = self.fork.state.as_ref() {
            return self.fork_palette_model(state);
        }
        if self.palette.dismissed {
            return None;
        }

        let (text, cursor) = {
            let input = self.input.read(cx);
            let document = input.text();

            // Reached from `render`, so this runs on every frame the pane
            // paints. Only a document opening with one of the two picker
            // sigils can produce a model, and its first character settles that
            // without walking the rope to copy the whole document out.
            if !matches!(document.chars().next(), Some('/' | '$')) {
                return None;
            }

            (document.to_string(), input.cursor())
        };

        if self.kind.caps().skill_references
            && let Some(query) = parse_skill_prefix(&text)
        {
            return Some(self.skill_palette_model(&query));
        }

        let parsed = parse_slash_command(&text)?;
        let catalog = self.command_catalog();
        // A harness with `$name` skill references reaches them that way.
        // Listing them under `/` too is a convenience for users who expect one
        // command key, so it follows the compatibility setting as well. Where
        // `/name` is instead the only way to reach a skill, the listing is not
        // a convenience and follows nothing.
        let caps = self.kind.caps();
        let slash_skills = caps.slash_skills_are_prompts
            || (caps.skill_references && cx.global::<AgentSettings>().codex_skill_command_compat);

        if parsed.has_argument_separator {
            let command = catalog.iter().find(|command| command.name == parsed.name)?;

            if command.arguments == SlashCommandArguments::Skills {
                let query = parsed.arguments.trim().to_ascii_lowercase();

                return Some(self.skill_palette_model(&query));
            }

            if command.arguments != SlashCommandArguments::Choices {
                return None;
            }

            let query = parsed.arguments.to_ascii_lowercase();
            let rows = self
                .command_choices(&command.name)
                .into_iter()
                .filter(|(value, label)| {
                    query.is_empty()
                        || value.to_ascii_lowercase().contains(&query)
                        || label.to_ascii_lowercase().contains(&query)
                })
                .map(|(value, label)| PaletteRow {
                    description: SharedString::new(&value),
                    label: label.into(),
                    hint: None,
                    disabled_reason: None,
                    action: PaletteAction::Choice {
                        command: command.name.clone(),
                        value,
                    },
                })
                .collect::<Vec<_>>();

            return Some(PaletteModel {
                note: rows
                    .is_empty()
                    .then(|| translated("agent-composer-no-matching-values")),
                rows,
            });
        }

        // Moving the caret into later prose must not turn an ordinary edit
        // into palette navigation; only the first slash token owns the keys.
        if cursor > 1 + parsed.name.len() {
            return None;
        }

        let skills: &[SkillInfo] = if slash_skills {
            self.palette
                .skill_catalog
                .as_ref()
                .map(|catalog| catalog.skills.as_slice())
                .unwrap_or_default()
        } else {
            &[]
        };
        let rows = filter_palette_catalog(&catalog, skills, &parsed.name)
            .into_iter()
            .map(|entry| match entry {
                PaletteCatalogEntry::Command(command) => {
                    // A local command runs against the pane and stays available
                    // whatever the harness is doing; anything the harness owns
                    // needs a session that has finished starting and not ended.
                    let disabled_reason = if command.run_policy == SlashCommandRunPolicy::IdleOnly
                        && self.is_command_busy()
                    {
                        Some(translated("agent-composer-available-when-idle"))
                    } else if command.source == SlashCommandSource::Local {
                        None
                    } else {
                        match self.runtime.status {
                            Status::Starting => Some(translated("agent-composer-agent-starting")),
                            Status::Exited => Some(translated("agent-composer-agent-exited")),
                            _ => None,
                        }
                    };

                    PaletteRow {
                        label: format!("/{}", command.name).into(),
                        description: SharedString::new(&command.description),
                        hint: command.argument_hint.as_deref().map(SharedString::new),
                        disabled_reason,
                        action: PaletteAction::Command(command.clone()),
                    }
                }
                PaletteCatalogEntry::Skill(skill) => PaletteRow {
                    label: format!("/{}", skill.name).into(),
                    description: SharedString::new(&skill.description),
                    hint: Some(
                        i18n("agent-composer-skill-scope")
                            .replace("{scope}", &skill.scope)
                            .into(),
                    ),
                    disabled_reason: self.skill_disabled_reason(skill),
                    action: PaletteAction::Skill(skill.clone()),
                },
            })
            .collect::<Vec<_>>();
        let note = if rows.is_empty() {
            if slash_skills && self.palette.skill_catalog.is_none() {
                Some(translated("agent-composer-skill-discovery-loading"))
            } else if slash_skills
                && self
                    .palette
                    .skill_catalog
                    .as_ref()
                    .is_some_and(|catalog| !catalog.errors.is_empty())
            {
                self.palette
                    .skill_catalog
                    .as_ref()
                    .and_then(|catalog| catalog.errors.first())
                    .map(SharedString::new)
            } else if slash_skills {
                Some(translated("agent-composer-no-matching-commands-skills"))
            } else {
                Some(translated("agent-composer-no-matching-commands"))
            }
        } else if self.kind.caps().async_command_discovery && !self.palette.provider_commands_ready
        {
            Some(translated("agent-composer-claude-command-loading"))
        } else if slash_skills && self.palette.skill_catalog.is_none() {
            Some(translated("agent-composer-skill-discovery-loading"))
        } else if slash_skills {
            self.palette
                .skill_catalog
                .as_ref()
                .and_then(|catalog| catalog.errors.first())
                .map(|error| {
                    i18n("agent-composer-skill-load-partial")
                        .replace("{error}", error)
                        .into()
                })
        } else {
            None
        };

        Some(PaletteModel { rows, note })
    }

    pub(crate) fn handle_palette_control(
        &mut self,
        control: PaletteControl,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(model) = self.palette_model(cx) else {
            // The card is answered before the recent-sessions list or the input
            // history get a look, because the turn is blocked on it and neither
            // of those can lead anywhere until it is.
            if self.handle_question_control(control, cx) {
                return;
            }
            if self.handle_recent_sessions_control(control, cx) {
                return;
            }
            let direction = match control {
                PaletteControl::Previous => Some(InputHistoryDirection::Older),
                PaletteControl::Next => Some(InputHistoryDirection::Newer),
                PaletteControl::Activate | PaletteControl::Complete | PaletteControl::Dismiss => {
                    None
                }
            };
            if direction
                .is_some_and(|direction| self.handle_input_history_control(direction, window, cx))
            {
                return;
            }
            cx.propagate();
            return;
        };

        cx.stop_propagation();

        match control {
            PaletteControl::Previous | PaletteControl::Next => {
                if let Some(direction) = control.direction()
                    && let Some(selected) =
                        move_palette_selection(self.palette.selected, model.rows.len(), direction)
                {
                    self.palette.selected = selected;
                    self.palette.scroll.scroll_to_item(self.palette.selected);
                    self.follow_branch_selection(cx);
                    cx.notify();
                }
            }
            PaletteControl::Activate => {
                if model.rows.is_empty() {
                    self.submit_current_slash(window, cx);
                } else {
                    self.activate_palette_index(self.palette.selected, true, window, cx);
                }
            }
            PaletteControl::Complete => {
                self.activate_palette_index(self.palette.selected, false, window, cx);
            }
            PaletteControl::Dismiss => {
                self.dismiss_command_palette(cx);
            }
        }
    }

    fn dismiss_command_palette(&mut self, cx: &mut Context<Self>) {
        if self
            .rewind
            .state
            .as_ref()
            .is_some_and(RewindState::is_picker)
        {
            self.cancel_rewind_picker(cx);
        } else if self.fork.state.as_ref().is_some_and(ForkState::is_picker) {
            self.cancel_fork_picker(cx);
        } else {
            self.palette.dismissed = true;
            cx.notify();
        }
    }

    fn handle_recent_sessions_control(
        &mut self,
        control: PaletteControl,
        cx: &mut Context<Self>,
    ) -> bool {
        if matches!(control, PaletteControl::Complete) || self.input.read(cx).text().len() != 0 {
            return false;
        }

        let rows = self
            .history_ui
            .pending
            .unwrap_or(self.history_ui.sessions.len());
        if !self
            .history_ui
            .mode
            .is_visible(self.transcript.read(cx).is_empty(), rows)
        {
            return false;
        }

        cx.stop_propagation();

        match control {
            PaletteControl::Previous | PaletteControl::Next => {
                if let Some(direction) = control.direction()
                    && let Some(selected) = move_palette_selection(
                        self.history_ui.selected,
                        self.history_ui.sessions.len(),
                        direction,
                    )
                {
                    self.history_ui.selected = selected;
                    self.history_ui
                        .scroll
                        .scroll_to_item(selected, ScrollStrategy::Nearest);
                    cx.notify();
                }
            }
            PaletteControl::Activate => {
                self.resume_session(self.history_ui.selected, cx);
            }
            PaletteControl::Dismiss => {
                self.history_ui.mode = RecentSessionsMode::Hidden;
                cx.notify();
            }
            // Completion belongs to the command palette. The guard above hands
            // it back before the list claims the keys, so there is nothing left
            // for it to do here.
            PaletteControl::Complete => {}
        }

        true
    }

    /// Move the highlight to the row under the pointer without acting on it.
    ///
    /// Only a picker of branch points does this: there the highlight is what
    /// the transcript follows, so pointing at a prompt has to reach it the
    /// same way the arrow keys do. In the command palette the pointer often
    /// rests over the list while the user types, and moving the highlight
    /// there would change what Enter runs.
    fn hover_palette_index(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.palette.selected == index {
            return;
        }

        self.palette.selected = index;
        self.follow_branch_selection(cx);
        cx.notify();
    }

    pub(crate) fn activate_palette_index(
        &mut self,
        index: usize,
        execute: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = self
            .palette_model(cx)
            .and_then(|model| model.rows.get(index).cloned())
        else {
            return;
        };

        if let Some(reason) = row.disabled_reason {
            self.set_command_feedback(CommandFeedbackKind::Error, reason, cx);
            return;
        }

        let (text, can_execute) = match row.action {
            PaletteAction::Command(command) => {
                let needs_arguments = command.arguments != SlashCommandArguments::None;
                (
                    format!(
                        "/{}{}",
                        command.name,
                        if needs_arguments { " " } else { "" }
                    ),
                    !needs_arguments,
                )
            }
            PaletteAction::Choice { command, value } => (format!("/{command} {value}"), true),
            // Where a skill is written into the prompt, picking one lands the
            // token the harness will recognize and leaves the caret after it,
            // because what follows is the request the skill serves.
            PaletteAction::Skill(skill) if self.kind.caps().slash_skills_are_prompts => {
                let text = format!("/{} ", skill.name);
                self.input.update(cx, |input, cx| {
                    input.set_value(text.clone(), window, cx);
                    input.set_selected_range(text.len()..text.len(), cx);
                });
                self.palette.selected = 0;
                self.palette.dismissed = true;
                cx.notify();
                return;
            }
            PaletteAction::Skill(skill) => {
                let Ok((text, binding)) = prepare_skill_selection(&skill) else {
                    self.set_command_feedback(
                        CommandFeedbackKind::Error,
                        i18n("agent-command-skill-disabled-by-codex")
                            .replace("{name}", &skill.name),
                        cx,
                    );
                    return;
                };

                self.input.update(cx, |input, cx| {
                    input.set_value(text.clone(), window, cx);
                    input.set_selected_range(text.len()..text.len(), cx);
                });
                self.palette.skill_binding = Some(binding);
                self.palette.selected = 0;
                self.palette.dismissed = true;
                cx.notify();
                return;
            }
            PaletteAction::RewindCheckpoint(checkpoint) => {
                let Some(operation_id) = self.rewind.state.as_ref().and_then(|state| match state {
                    RewindState::SelectingCheckpoint { operation_id, .. } => Some(*operation_id),
                    _ => None,
                }) else {
                    return;
                };
                self.rewind.state = Some(RewindState::SelectingAction {
                    operation_id,
                    checkpoint,
                });
                self.palette.selected = 0;
                cx.notify();
                return;
            }
            PaletteAction::RewindAction(action) => {
                self.activate_rewind_action(action, window, cx);
                return;
            }
            PaletteAction::ForkCheckpoint(checkpoint) => {
                self.start_conversation_branch(checkpoint, cx);
                return;
            }
            PaletteAction::ForkCancel => {
                self.cancel_fork_picker(cx);
                return;
            }
        };

        self.input.update(cx, |input, cx| {
            input.set_value(text.clone(), window, cx);
            input.set_selected_range(text.len()..text.len(), cx);
        });
        self.palette.selected = 0;

        if execute && can_execute {
            self.submit_current_slash(window, cx);
        } else {
            cx.notify();
        }
    }

    pub(crate) fn render_command_palette(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let model = self.palette_model(cx)?;
        let selected = self
            .palette
            .selected
            .min(model.rows.len().saturating_sub(1));
        let hover_selects = self.branch_picker_is_open();
        let rows = model
            .rows
            .into_iter()
            .enumerate()
            .map(|(index, row)| {
                let disabled = row.disabled_reason.is_some();
                let detail = row.disabled_reason.clone().unwrap_or(row.description);
                let background = (index == selected).then(|| cx.theme().muted.opacity(0.7));

                div()
                    .id(("agent-slash-command", index))
                    .h(px(48.))
                    .flex_none()
                    .px_3()
                    .py_1p5()
                    .rounded(UI_RADIUS)
                    .when_some(background, |this, color| this.bg(color))
                    .when(disabled, |this| this.opacity(0.5))
                    .when(!disabled, |this| {
                        this.hover(|style| style.bg(cx.theme().muted.opacity(0.45)))
                    })
                    .when(hover_selects && !disabled, |this| {
                        this.on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                            if *hovered {
                                this.hover_palette_index(index, cx);
                            }
                        }))
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.activate_palette_index(index, true, window, cx)
                    }))
                    .child(
                        h_flex()
                            .w_full()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(cx.theme().foreground)
                                    .child(row.label),
                            )
                            .children(row.hint.map(|hint| {
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground.opacity(0.75))
                                    .child(hint)
                            })),
                    )
                    .child(
                        div()
                            .text_xs()
                            .truncate()
                            .text_color(cx.theme().muted_foreground)
                            .child(detail),
                    )
                    .into_any_element()
            })
            .collect::<Vec<_>>();

        let note = model.note.map(|note| {
            div()
                .px_3()
                .py_2()
                .text_xs()
                .text_color(cx.theme().muted_foreground.opacity(0.75))
                .child(note)
        });

        Some(
            v_flex()
                .id("agent-slash-command-palette")
                .on_mouse_down_out(cx.listener(|this, _, _, cx| this.dismiss_command_palette(cx)))
                .w_full()
                .max_h(PALETTE_MAX_HEIGHT)
                .overflow_y_scroll()
                .track_scroll(&self.palette.scroll)
                .p_1()
                .rounded(UI_RADIUS)
                .border_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().popover)
                .shadow_lg()
                .children(rows)
                .children(note)
                .into_any_element(),
        )
    }
}
