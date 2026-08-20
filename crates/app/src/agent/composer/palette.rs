use nmt_i18n::i18n;

use crate::agent::composer::{CommandFeedbackKind, RewindAction, RewindState};
use crate::agent::input_history::InputHistoryDirection;
use crate::agent::*;

#[derive(Clone)]
pub(in crate::agent) enum PaletteAction {
    Command(SlashCommandInfo),
    Choice { command: String, value: String },
    Skill(SkillInfo),
    RewindCheckpoint(sessions::ClaudeCheckpoint),
    RewindAction(RewindAction),
}

#[derive(Clone)]
pub(in crate::agent) struct PaletteRow {
    pub(in crate::agent) label: String,
    pub(in crate::agent) description: String,
    pub(in crate::agent) hint: Option<String>,
    pub(in crate::agent) disabled_reason: Option<String>,
    pub(in crate::agent) action: PaletteAction,
}

pub(in crate::agent) struct PaletteModel {
    pub(in crate::agent) rows: Vec<PaletteRow>,
    pub(in crate::agent) note: Option<String>,
}

#[derive(Clone, Copy)]
pub(in crate::agent) enum PaletteControl {
    Previous,
    Next,
    Activate,
    Complete,
    Dismiss,
}

impl AgentPane {
    pub(in crate::agent) fn open_recent_sessions(&mut self, cx: &mut Context<Self>) -> bool {
        if self.is_command_busy() {
            self.set_command_feedback(
                CommandFeedbackKind::Error,
                i18n("agent-composer-resume-idle-only").to_string(),
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
                i18n("agent-composer-no-recent-sessions").to_string(),
                cx,
            );
            return true;
        }

        self.history_ui.mode = RecentSessionsMode::Open;
        self.history_ui.selected = 0;
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
                note: Some(i18n("agent-composer-skill-discovery-loading").to_string()),
            };
        };

        let rows = filter_skill_catalog(&skill_catalog.skills, query)
            .into_iter()
            .map(|skill| PaletteRow {
                label: format!("${}", skill.name),
                description: skill.description.clone(),
                hint: Some(skill.scope.clone()),
                disabled_reason: self.skill_disabled_reason(&skill),
                action: PaletteAction::Skill(skill),
            })
            .collect::<Vec<_>>();
        let note = if rows.is_empty() && !skill_catalog.errors.is_empty() {
            Some(skill_catalog.errors[0].clone())
        } else if rows.is_empty() && query.is_empty() {
            Some(i18n("agent-composer-no-skills").to_string())
        } else if rows.is_empty() {
            Some(i18n("agent-composer-no-matching-skills").to_string())
        } else if let Some(error) = skill_catalog.errors.first() {
            Some(i18n("agent-composer-skill-load-partial").replace("{error}", error))
        } else {
            None
        };

        PaletteModel { rows, note }
    }

    pub(in crate::agent) fn palette_model(&self, cx: &Context<Self>) -> Option<PaletteModel> {
        if let Some(state) = self.rewind.state.as_ref() {
            return self.rewind_palette_model(state);
        }
        if self.palette.dismissed {
            return None;
        }

        let input = self.input.read(cx);
        let text = input.text().to_string();

        if self.kind.caps().skill_references {
            if let Some(query) = parse_skill_prefix(&text) {
                return Some(self.skill_palette_model(&query));
            }
        }

        let parsed = parse_slash_command(&text)?;
        let cursor = input.cursor();
        let catalog = self.command_catalog();
        // A harness with `$name` skill references reaches them that way.
        // Listing them under `/` too is a convenience for users who expect one
        // command key, so it follows the compatibility setting as well. Where
        // `/name` is instead the only way to reach a skill, the listing is not
        // a convenience and follows nothing.
        let caps = self.kind.caps();
        let slash_skills = caps.slash_skills_are_prompts
            || (caps.skill_references && cx.global::<AppSettings>().codex_skill_command_compat);

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
                    description: value.clone(),
                    label,
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
                    .then(|| i18n("agent-composer-no-matching-values").to_string()),
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
                    let disabled_reason = if command.run_policy == SlashCommandRunPolicy::IdleOnly
                        && self.is_command_busy()
                    {
                        Some(i18n("agent-composer-available-when-idle").to_string())
                    } else if command.source != SlashCommandSource::Local
                        && matches!(self.status, Status::Starting | Status::Exited)
                    {
                        Some(match self.status {
                            Status::Starting => i18n("agent-composer-agent-starting").to_string(),
                            Status::Exited => i18n("agent-composer-agent-exited").to_string(),
                            _ => unreachable!(),
                        })
                    } else {
                        None
                    };

                    PaletteRow {
                        label: format!("/{}", command.name),
                        description: command.description.clone(),
                        hint: command.argument_hint.clone(),
                        disabled_reason,
                        action: PaletteAction::Command(command),
                    }
                }
                PaletteCatalogEntry::Skill(skill) => PaletteRow {
                    label: format!("/{}", skill.name),
                    description: skill.description.clone(),
                    hint: Some(i18n("agent-composer-skill-scope").replace("{scope}", &skill.scope)),
                    disabled_reason: self.skill_disabled_reason(&skill),
                    action: PaletteAction::Skill(skill),
                },
            })
            .collect::<Vec<_>>();
        let note = if rows.is_empty() {
            if slash_skills && self.palette.skill_catalog.is_none() {
                Some(i18n("agent-composer-skill-discovery-loading").to_string())
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
                    .and_then(|catalog| catalog.errors.first().cloned())
            } else if slash_skills {
                Some(i18n("agent-composer-no-matching-commands-skills").to_string())
            } else {
                Some(i18n("agent-composer-no-matching-commands").to_string())
            }
        } else if self.kind.caps().async_command_discovery && !self.palette.provider_commands_ready
        {
            Some(i18n("agent-composer-claude-command-loading").to_string())
        } else if slash_skills && self.palette.skill_catalog.is_none() {
            Some(i18n("agent-composer-skill-discovery-loading").to_string())
        } else if slash_skills {
            self.palette
                .skill_catalog
                .as_ref()
                .and_then(|catalog| catalog.errors.first())
                .map(|error| i18n("agent-composer-skill-load-partial").replace("{error}", error))
        } else {
            None
        };

        Some(PaletteModel { rows, note })
    }

    pub(in crate::agent) fn handle_palette_control(
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
                let direction = match control {
                    PaletteControl::Previous => PaletteDirection::Previous,
                    PaletteControl::Next => PaletteDirection::Next,
                    _ => unreachable!(),
                };

                if let Some(selected) =
                    move_palette_selection(self.palette.selected, model.rows.len(), direction)
                {
                    self.palette.selected = selected;
                    self.palette.scroll.scroll_to_item(self.palette.selected);
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
                let direction = match control {
                    PaletteControl::Previous => PaletteDirection::Previous,
                    PaletteControl::Next => PaletteDirection::Next,
                    _ => unreachable!(),
                };

                if let Some(selected) = move_palette_selection(
                    self.history_ui.selected,
                    self.history_ui.sessions.len(),
                    direction,
                ) {
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
            PaletteControl::Complete => unreachable!(),
        }

        true
    }

    pub(in crate::agent) fn activate_palette_index(
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

    pub(in crate::agent) fn render_command_palette(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let model = self.palette_model(cx)?;
        let selected = self
            .palette
            .selected
            .min(model.rows.len().saturating_sub(1));
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
                .max_h(px(9. * 48. + 36.))
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
