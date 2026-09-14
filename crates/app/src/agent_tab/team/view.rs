mod membership;
mod timeline;

#[cfg(test)]
mod tests;

use crate::agent_tab::settings::{AgentSettings, UI_RADIUS};
use crate::agent_tab::team::view::membership::MemberDraft;
use crate::agent_tab::team::view::timeline::TimelineMirror;
use crate::agent_tab::team::{TeamCommand, TeamRuntime};
use crate::agent_tab::thread_controls::settings_pill;
use crate::agent_tab::transcript::{TranscriptView, transcript_column};
use crate::agent_tab::view::composer_layout::{
    composer_card, composer_controls_row, composer_input_row,
};
use crate::agent_tab::{
    AgentPane, ComposerEnterBehavior, StopResponseIcon, composer_enter_behavior,
};
use gpui::prelude::*;
use gpui::{
    Anchor, AnyElement, App, Context, Entity, FocusHandle, Focusable, Render, Subscription, Window,
    div, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Enter, Escape, Textarea, TextareaState};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::{
    ActiveTheme as _, Disableable as _, IconName, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use nmt_agent::session::AgentKind;
use nmt_agent::session::lifecycle::Status;
use nmt_agent::team::content::UserInput;
use nmt_agent::team::discussion::{DiscussionMode, DiscussionState, PauseReason};
use nmt_agent::team::identity::{MemberId, RoomId};
use nmt_agent::team::session::TeamError;
use rust_i18n::t;
use std::collections::BTreeSet;

pub struct TeamPane {
    runtime: Entity<TeamRuntime>,
    focus: FocusHandle,
    input: Entity<TextareaState>,
    transcript: Entity<TranscriptView>,
    selected: BTreeSet<MemberId>,
    author: Option<MemberId>,
    moderated: bool,
    discussion_mode: bool,
    inspected: Option<(MemberId, Entity<AgentPane>)>,
    timeline: TimelineMirror,
    member_draft: MemberDraft,
    error: Option<String>,
    _observers: Vec<Subscription>,
}

impl TeamPane {
    pub fn new(runtime: Entity<TeamRuntime>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 8)
                .submit_on_enter(true)
                .placeholder(t!("team-input-placeholder").into_owned())
        });

        let member_draft = MemberDraft::new(window, cx);

        let transcript = cx.new(|_| TranscriptView::new(AgentKind::Codex, None));

        let changed = cx.observe(&runtime, |this, _, cx| {
            this.timeline
                .sync_timeline(&this.runtime, &this.transcript, cx);

            cx.notify();
        });

        let room = runtime.read(cx).room();

        let selected = room
            .members()
            .iter()
            .filter(|member| !member.excluded())
            .map(|member| member.id())
            .collect::<BTreeSet<_>>();

        let active = room
            .discussions()
            .iter()
            .find(|run| run.state() != DiscussionState::Completed);

        let author = active
            .map(|run| run.mode().report_author())
            .or_else(|| room.members().first().map(|member| member.id()));

        let moderated =
            active.is_some_and(|run| matches!(run.mode(), DiscussionMode::Moderated { .. }));

        let discussion_mode = active.is_some();

        let mut pane = Self {
            runtime,
            focus: cx.focus_handle(),
            input,
            transcript,
            selected,
            author,
            moderated,
            discussion_mode,
            inspected: None,
            timeline: TimelineMirror::default(),
            member_draft,
            error: None,
            _observers: vec![changed],
        };

        pane.timeline
            .sync_timeline(&pane.runtime, &pane.transcript, cx);

        pane
    }

    pub fn room_id(&self, cx: &App) -> RoomId {
        self.runtime.read(cx).room().id()
    }

    pub fn runtime(&self) -> &Entity<TeamRuntime> {
        &self.runtime
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.input.update(cx, |input, cx| input.focus(window, cx));
    }

    fn perform(&mut self, command: TeamCommand, cx: &mut Context<Self>) -> bool {
        match self
            .runtime
            .update(cx, |runtime, cx| runtime.command(command, cx))
        {
            Ok(()) => {
                self.error = None;

                true
            }

            Err(error) => {
                self.error = Some(match error {
                    TeamError::Unavailable => self
                        .runtime
                        .read(cx)
                        .error()
                        .map(str::to_owned)
                        .unwrap_or_else(|| t!("team-unavailable").into_owned()),

                    error => error.to_string(),
                });

                cx.notify();

                false
            }
        }
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = UserInput {
            text: self.input.read(cx).text().to_string(),
            ..UserInput::default()
        };

        if input.text.trim().is_empty() {
            return;
        }

        let active = self
            .runtime
            .read(cx)
            .room()
            .discussions()
            .iter()
            .any(|run| run.state() != DiscussionState::Completed);

        let command = if active {
            TeamCommand::Correction(input)
        } else if self.discussion_mode {
            let Some(author) = self.author else {
                self.error = Some(t!("team-select-author").into_owned());

                cx.notify();

                return;
            };

            let mode = if self.moderated {
                DiscussionMode::Moderated { moderator: author }
            } else {
                DiscussionMode::Fixed {
                    report_author: author,
                }
            };

            TeamCommand::Start {
                input,
                participants: self.selected.iter().copied().collect(),
                mode,
            }
        } else {
            TeamCommand::Direct {
                input,
                recipients: self.selected.iter().copied().collect(),
            }
        };

        if self.perform(command, cx) {
            self.input
                .update(cx, |input, cx| input.set_value("", window, cx));

            self.transcript.update(cx, |transcript, cx| {
                transcript.scroll_to_bottom();

                cx.notify();
            });
        }
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        let members: Vec<_> = self
            .runtime
            .read(cx)
            .hosts
            .iter()
            .filter(|(_, host)| host.active.is_some())
            .map(|(id, _)| *id)
            .collect();

        for member in members {
            self.perform(TeamCommand::Stop(member), cx);
        }
    }

    fn inspect_member(&mut self, member: MemberId, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.runtime.update(cx, |runtime, cx| {
            runtime
                .hosts
                .get(&member)
                .map(|host| cx.new(|cx| AgentPane::attach_team_member(&host.owner, window, cx)))
        });

        self.inspected = pane.map(|pane| (member, pane));

        cx.notify();
    }

    fn add_member(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((profile, config)) = self
            .member_draft
            .configuration(self.runtime.read(cx).room().workspace().clone(), cx)
        else {
            return;
        };

        match self
            .runtime
            .update(cx, |runtime, cx| runtime.add_member(profile, config, cx))
        {
            Ok(id) => {
                self.selected.insert(id);
                self.author.get_or_insert(id);

                self.member_draft.clear(window, cx);

                self.error = None;
                window.close_dialog(cx);
            }

            Err(error) => self.error = Some(error.to_string()),
        }

        cx.notify();
    }

    fn render_controls(&self, cx: &mut Context<Self>) -> AnyElement {
        let room = self.runtime.read(cx).room();

        let pending_recovery: Vec<_> = self
            .runtime
            .read(cx)
            .session
            .pending_recovery()
            .map(|attempt| {
                (
                    attempt.id,
                    room.member(attempt.intent.recipient)
                        .map_or("", |member| member.name())
                        .to_owned(),
                )
            })
            .collect();

        let members: Vec<_> = room
            .members()
            .iter()
            .map(|member| (member.id(), member.name().to_owned(), member.excluded()))
            .collect();

        let selected = self.selected.clone();

        let names = members
            .iter()
            .filter(|(id, _, _)| selected.contains(id))
            .map(|(_, name, _)| name.as_str())
            .collect::<Vec<_>>()
            .join(", ");

        let recipient_label = if names.is_empty() {
            t!("team-select-members").into_owned()
        } else {
            names
        };

        let pane = cx.entity();

        let recipients = settings_pill(Button::new("team-recipients"))
            .label(recipient_label)
            .dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu, window, cx| {
                let mut menu = menu;

                for (id, name, excluded) in &members {
                    let id = *id;
                    let pane = pane.clone();

                    menu = menu.item(
                        PopupMenuItem::new(name.clone())
                            .checked(selected.contains(&id))
                            .disabled(*excluded)
                            .on_click(move |_, _, cx| {
                                pane.update(cx, |pane, cx| {
                                    if !pane.selected.remove(&id) {
                                        pane.selected.insert(id);
                                    }

                                    cx.notify();
                                });
                            }),
                    );
                }

                if !members.is_empty() {
                    let inspect = members.clone();
                    let view = pane.clone();

                    menu = menu.separator().submenu(
                        t!("team-conversations"),
                        window,
                        cx,
                        move |menu, _, _| {
                            let mut menu = menu;

                            for (id, name, _) in &inspect {
                                let id = *id;
                                let view = view.clone();

                                menu = menu.item(PopupMenuItem::new(name.clone()).on_click(
                                    move |_, window, cx| {
                                        view.update(cx, |pane, cx| {
                                            pane.inspect_member(id, window, cx)
                                        });
                                    },
                                ));
                            }

                            menu
                        },
                    );
                }

                let pane = pane.clone();

                menu.item(PopupMenuItem::new(t!("team-add-member")).on_click(
                    move |_, window, cx| {
                        pane.update(cx, |pane, cx| {
                            pane.member_draft
                                .open_member_form(&pane.runtime, window, cx)
                        });
                    },
                ))
            });

        let active = room
            .discussions()
            .iter()
            .find(|run| run.state() != DiscussionState::Completed)
            .map(|run| (run.id(), run.mode()));

        let mode_label = if !self.discussion_mode {
            t!("team-send-direct")
        } else if self.moderated {
            t!("team-moderated")
        } else {
            t!("team-fixed")
        };

        let authors: Vec<_> = room
            .members()
            .iter()
            .filter(|member| !member.excluded())
            .map(|member| (member.id(), member.name().to_owned()))
            .collect();

        let author = self.author;
        let moderated = self.moderated;
        let discussion_mode = self.discussion_mode;
        let pane = cx.entity();

        let mode = settings_pill(Button::new("team-mode"))
            .label(mode_label)
            .dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu, window, cx| {
                let mut menu = menu;

                for (index, label) in [
                    t!("team-send-direct"),
                    t!("team-fixed"),
                    t!("team-moderated"),
                ]
                .into_iter()
                .enumerate()
                {
                    let pane = pane.clone();

                    let checked = match index {
                        0 => !discussion_mode,
                        1 => discussion_mode && !moderated,
                        _ => discussion_mode && moderated,
                    };

                    menu = menu.item(
                        PopupMenuItem::new(label)
                            .checked(checked)
                            .disabled(index == 0 && active.is_some())
                            .on_click(move |_, _, cx| {
                                pane.update(cx, |pane, cx| pane.select_mode(index, cx));
                            }),
                    );
                }

                let pane = pane.clone();
                let authors = authors.clone();

                menu.separator()
                    .submenu(t!("team-select-author"), window, cx, move |menu, _, _| {
                        let mut menu = menu;

                        for (id, name) in &authors {
                            let id = *id;
                            let pane = pane.clone();

                            menu = menu.item(
                                PopupMenuItem::new(name.clone())
                                    .checked(author == Some(id))
                                    .on_click(move |_, _, cx| {
                                        pane.update(cx, |pane, cx| pane.select_author(id, cx));
                                    }),
                            );
                        }

                        menu
                    })
            });

        let controls = room.controls().clone();
        let pane = cx.entity();

        let settings = settings_pill(Button::new("team-settings"))
            .label(t!("team-options"))
            .dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu, _, _| {
                let summaries = pane.clone();

                menu.item(
                    PopupMenuItem::new(t!("team-auto-summaries"))
                        .checked(controls.automatic_summaries)
                        .on_click(move |_, _, cx| {
                            summaries.update(cx, |pane, cx| {
                                pane.perform(
                                    TeamCommand::AutomaticSummaries(!controls.automatic_summaries),
                                    cx,
                                );
                            });
                        }),
                )
            });

        h_flex()
            .flex_wrap()
            .gap_2()
            .child(recipients)
            .child(mode)
            .child(settings)
            .when(!pending_recovery.is_empty(), |view| {
                let pane = cx.entity();

                let names = pending_recovery
                    .iter()
                    .map(|(_, name)| name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");

                view.child(
                    Button::new("team-review-interrupted")
                        .ghost()
                        .small()
                        .label(t!("team-review-interrupted"))
                        .on_click(move |_, window, cx| {
                            let pane = pane.clone();
                            let attempts = pending_recovery.clone();
                            let names = names.clone();

                            window.open_alert_dialog(cx, move |alert, _, _| {
                                let pane = pane.clone();
                                let attempts = attempts.clone();

                                alert
                                    .confirm()
                                    .title(t!("team-end-interrupted"))
                                    .description(
                                        t!(
                                            "team-end-interrupted-description",
                                            members = names.clone()
                                        )
                                        .into_owned(),
                                    )
                                    .on_ok(move |_, _, cx| {
                                        pane.update(cx, |pane, cx| {
                                            for (id, _) in &attempts {
                                                if !pane
                                                    .perform(TeamCommand::AbandonRestored(*id), cx)
                                                {
                                                    return false;
                                                }
                                            }

                                            true
                                        })
                                    })
                            });
                        }),
                )
            })
            .when_some(active, |view, (id, _)| {
                let paused = room
                    .discussions()
                    .iter()
                    .find(|run| run.id() == id)
                    .is_some_and(|run| {
                        !matches!(
                            run.state(),
                            DiscussionState::Running | DiscussionState::Finishing
                        )
                    });

                let pane = cx.entity();

                let progress = Button::new("team-progress")
                    .ghost()
                    .small()
                    .disabled(
                        self.runtime
                            .read(cx)
                            .session
                            .pending_recovery()
                            .next()
                            .is_some(),
                    )
                    .label(if paused {
                        t!("team-continue")
                    } else {
                        t!("team-pause")
                    })
                    .on_click(cx.listener(move |pane, _, _, cx| {
                        pane.perform(
                            if paused {
                                TeamCommand::Continue(id)
                            } else {
                                TeamCommand::Pause(id)
                            },
                            cx,
                        );
                    }));

                let actions = Button::new("team-discussion-actions")
                    .ghost()
                    .small()
                    .label(t!("team-discussion-actions"))
                    .dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu, _, _| {
                        let add = pane.clone();
                        let finish = pane.clone();

                        menu.item(PopupMenuItem::new(t!("team-add-turns")).on_click(
                            move |_, _, cx| {
                                add.update(cx, |pane, cx| {
                                    pane.perform(
                                        TeamCommand::AddTurns {
                                            discussion: id,
                                            count: 4,
                                        },
                                        cx,
                                    );
                                });
                            },
                        ))
                        .item(
                            PopupMenuItem::new(t!("team-finish")).on_click(move |_, _, cx| {
                                finish.update(cx, |pane, cx| {
                                    pane.perform(TeamCommand::Finish(id), cx);
                                });
                            }),
                        )
                    });

                view.child(progress).child(actions)
            })
            .into_any_element()
    }

    fn select_mode(&mut self, selection: usize, cx: &mut Context<Self>) {
        let active = self
            .runtime
            .read(cx)
            .room()
            .discussions()
            .iter()
            .find(|run| run.state() != DiscussionState::Completed)
            .map(|run| run.id());

        if let Some(id) = active {
            let Some(author) = self.author else { return };

            let mode = if selection == 2 {
                DiscussionMode::Moderated { moderator: author }
            } else {
                DiscussionMode::Fixed {
                    report_author: author,
                }
            };

            if !self.perform(
                TeamCommand::ChangeMode {
                    discussion: id,
                    mode,
                },
                cx,
            ) {
                return;
            }
        }

        self.discussion_mode = selection != 0;
        self.moderated = selection == 2;

        cx.notify();
    }

    fn select_author(&mut self, author: MemberId, cx: &mut Context<Self>) {
        let active = self
            .runtime
            .read(cx)
            .room()
            .discussions()
            .iter()
            .find(|run| run.state() != DiscussionState::Completed)
            .map(|run| run.id());

        if let Some(id) = active {
            let mode = if self.moderated {
                DiscussionMode::Moderated { moderator: author }
            } else {
                DiscussionMode::Fixed {
                    report_author: author,
                }
            };

            if !self.perform(
                TeamCommand::ChangeMode {
                    discussion: id,
                    mode,
                },
                cx,
            ) {
                return;
            }
        }

        self.author = Some(author);

        cx.notify();
    }

    fn status_text(&self, cx: &App) -> String {
        let runtime = self.runtime.read(cx);
        let room = runtime.room();
        let mut parts = Vec::new();

        for member in room
            .members()
            .iter()
            .filter(|member| self.selected.contains(&member.id()))
        {
            let status = runtime.hosts.get(&member.id()).map(|host| {
                host.owner
                    .session()
                    .read(cx)
                    .controller
                    .borrow()
                    .runtime
                    .status()
            });

            let needs_recovery = runtime
                .session
                .pending_recovery()
                .any(|attempt| attempt.intent.recipient == member.id());

            let label = if needs_recovery {
                t!("team-recovery-needed")
            } else {
                match status {
                    Some(Status::Starting) => t!("team-initializing"),
                    Some(Status::Running) => t!("team-responding"),
                    Some(Status::Idle) => t!("team-ready"),
                    _ => t!("team-unavailable"),
                }
            };

            parts.push(format!("{}: {label}", member.name()));
        }

        if let Some(run) = room
            .discussions()
            .iter()
            .find(|run| run.state() != DiscussionState::Completed)
        {
            parts.push(
                t!(
                    "team-budget-status",
                    remaining = run.budget().remaining_non_report_turns()
                )
                .into_owned(),
            );

            if !run.pauses().is_empty() {
                parts.push(t!("team-paused").into_owned());

                for reason in run.pauses() {
                    let label = match reason {
                        PauseReason::Interaction(_) => t!("team-interaction-required"),
                        PauseReason::UncertainAttempt(_) => t!("team-response-uncertain"),
                        PauseReason::Budget => t!("team-budget-needed"),

                        PauseReason::ContextSelection | PauseReason::SummaryFailed(_) => {
                            t!("team-context-needed")
                        }

                        PauseReason::MemberUnavailable(_) | PauseReason::ModeratorUnavailable => {
                            t!("team-unavailable")
                        }

                        _ => t!("team-continue-hint"),
                    };

                    if !parts.iter().any(|part| part == label.as_ref()) {
                        parts.push(label.into_owned());
                    }
                }
            }
        }

        parts.join(" · ")
    }
}

impl Focusable for TeamPane {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TeamPane {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let settings = cx.global::<AgentSettings>();

        let background = if settings.pane_background_follows_terminal {
            settings.terminal_background
        } else {
            cx.theme().sidebar
        };

        let font = settings.font();
        let font_size = settings.font_size;
        let background_opacity = settings.background_opacity;

        let surface = v_flex()
            .debug_selector(|| "team-surface".into())
            .size_full()
            .relative()
            .min_h_0()
            .bg(background.alpha(background_opacity))
            .rounded(UI_RADIUS - px(1.))
            .overflow_hidden()
            .font(font)
            .text_size(px(font_size))
            .track_focus(&self.focus);

        if let Some((id, pane)) = &self.inspected {
            let name = self
                .runtime
                .read(cx)
                .room()
                .member(*id)
                .map_or("", |member| member.name())
                .to_owned();

            return surface
                .child(transcript_column(
                    h_flex()
                        .gap_2()
                        .py_2()
                        .child(
                            Button::new("team-back")
                                .ghost()
                                .small()
                                .icon(IconName::ArrowLeft)
                                .label(t!("team-back"))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.inspected = None;
                                    this.focus(window, cx);

                                    cx.notify();
                                })),
                        )
                        .child(name),
                    cx,
                ))
                .child(div().flex_1().min_h_0().child(pane.clone()))
                .into_any_element();
        }

        let runtime = self.runtime.read(cx);
        let empty = runtime.room().members().is_empty();
        let running = runtime.hosts.values().any(|host| host.active.is_some());

        let failure = self
            .error
            .clone()
            .or_else(|| runtime.error().map(str::to_owned));

        let status = self.status_text(cx);
        let controls = self.render_controls(cx);

        let send = Button::new("team-send")
            .primary()
            .size(px(32.))
            .rounded_full()
            .disabled(!running && self.selected.is_empty())
            .when(running, |button| button.icon(StopResponseIcon))
            .when(!running, |button| button.icon(IconName::ArrowUp))
            .tooltip(if running {
                t!("agent-action-stop-response")
            } else {
                t!("agent-action-send-message")
            })
            .accessibility_label(if running {
                t!("agent-action-stop-response")
            } else {
                t!("agent-action-send-message")
            })
            .on_click(cx.listener(move |this, _, window, cx| {
                if running {
                    this.stop(cx);
                } else {
                    this.submit(window, cx);
                }
            }));

        surface
            .on_action(cx.listener(|this, _: &Escape, _, cx| this.stop(cx)))
            .child(
                div()
                    .debug_selector(|| "team-messages".into())
                    .flex_1()
                    .min_h_0()
                    .child(self.transcript.clone()),
            )
            .child(
                transcript_column(
                    div()
                        .w_full()
                        .when(empty, |view| {
                            view.child(
                                v_flex().gap_2().pb_3().items_center().child(
                                    div()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(t!("team-empty")),
                                ),
                            )
                        })
                        .child(
                            composer_card(cx)
                                .debug_selector(|| "team-composer".into())
                                .when_some(failure, |view, failure| {
                                    view.child(
                                        div()
                                            .px_3()
                                            .pt_2()
                                            .text_sm()
                                            .text_color(cx.theme().danger)
                                            .child(failure),
                                    )
                                })
                                .child(
                                    composer_input_row()
                                        .text_size(px(font_size + 2.))
                                        .capture_action(cx.listener(
                                            |this, action: &Enter, window, cx| {
                                                match composer_enter_behavior(
                                                    cx.global::<AgentSettings>().newline_shortcut,
                                                    action,
                                                ) {
                                                    ComposerEnterBehavior::InsertNewline => {
                                                        this.input.update(cx, |input, cx| {
                                                            input.replace("\n", window, cx)
                                                        })
                                                    }

                                                    ComposerEnterBehavior::Submit
                                                    | ComposerEnterBehavior::ActivateOrSubmit => {
                                                        this.submit(window, cx)
                                                    }
                                                }

                                                cx.stop_propagation();
                                            },
                                        ))
                                        .child(
                                            div().flex_1().min_w_0().child(
                                                Textarea::new(&self.input)
                                                    .appearance(false)
                                                    .disabled(empty),
                                            ),
                                        ),
                                )
                                .child(
                                    composer_controls_row()
                                        .child(div().flex_1().min_w_0().child(controls))
                                        .child(send),
                                ),
                        )
                        .child(
                            div()
                                .px(px(14.))
                                .py(px(6.))
                                .text_size(px(11.5))
                                .text_color(cx.theme().muted_foreground)
                                .child(status),
                        ),
                    cx,
                )
                .pb_3()
                .pt_1(),
            )
            .into_any_element()
    }
}
