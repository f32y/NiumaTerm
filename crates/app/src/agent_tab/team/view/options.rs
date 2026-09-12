use gpui::prelude::*;
use gpui::{Anchor, AnyElement, App, Context};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::{Disableable as _, Sizable as _, WindowExt as _, h_flex};
use nmt_agent::session::lifecycle::Status;
use nmt_agent::team::discussion::{DiscussionMode, DiscussionState, PauseReason};
use nmt_agent::team::identity::MemberId;
use rust_i18n::t;

use crate::agent_tab::team::TeamCommand;
use crate::agent_tab::team::view::TeamPane;
use crate::agent_tab::thread_controls::settings_pill;

impl TeamPane {
    pub(super) fn render_controls(&self, cx: &mut Context<Self>) -> AnyElement {
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
                        pane.update(cx, |pane, cx| pane.open_member_form(window, cx));
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

    pub(super) fn status_text(&self, cx: &App) -> String {
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
