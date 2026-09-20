mod membership;
mod targeting;
mod timeline;

#[cfg(test)]
mod tests;

use std::collections::BTreeMap;

use gpui::prelude::*;
use gpui::{
    Anchor, AnyElement, App, Context, Entity, FocusHandle, Focusable, Render, Subscription, Task,
    Window, div, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Enter, Escape, Textarea, TextareaState};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::{
    ActiveTheme as _, Disableable as _, IconName, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use nmt_agent::session::AgentKind;
use nmt_agent::session::lifecycle::Status;
use nmt_agent::team::discussion::{DiscussionState, PauseReason};
use nmt_agent::team::model::{MemberId, RoomId, UserInput};
use nmt_agent::team::session::TeamError;
use rust_i18n::t;

use crate::agent_tab::AgentPane;
use crate::agent_tab::settings::{AgentSettings, UI_RADIUS};
use crate::agent_tab::team::dispatch::work_status;
use crate::agent_tab::team::view::membership::MemberDraft;
use crate::agent_tab::team::view::targeting::{DiscussionTargeting, MissingAuthor};
use crate::agent_tab::team::view::timeline::TimelineMirror;
use crate::agent_tab::team::{TeamCommand, TeamRuntime};
use crate::agent_tab::thread_controls::{harness_settings, harness_submenus, settings_pill};
use crate::agent_tab::transcript::{TranscriptView, transcript_column};
use crate::agent_tab::view::composer_layout::{
    ComposerEnterBehavior, composer_card, composer_controls_row, composer_enter_behavior,
    composer_input_row, send_button,
};

pub struct TeamPane {
    runtime: Entity<TeamRuntime>,
    focus: FocusHandle,
    input: Entity<TextareaState>,
    transcript: Entity<TranscriptView>,
    targeting: DiscussionTargeting,
    inspected: Option<MemberId>,

    /// One view per member session, made when first needed and kept: a
    /// member's questions are answered through its view, from the Team
    /// composer as much as from the member's own page, and binding a second
    /// view to the session would retire the first one's answers.
    member_panes: BTreeMap<MemberId, Entity<AgentPane>>,

    timeline: TimelineMirror,
    member_draft: MemberDraft,
    error: Option<String>,
    loaded: bool,
    submitting: bool,
    adding_member: bool,
    abandoning: bool,
    _observers: Vec<Subscription>,
}

impl TeamPane {
    pub fn new(runtime: Entity<TeamRuntime>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.on_release(|this, cx| {
            this.runtime
                .update(cx, |runtime, cx| runtime.close(cx))
                .detach();
        })
        .detach();

        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 8)
                .submit_on_enter(true)
                .placeholder(t!("team-input-placeholder").into_owned())
        });

        let member_draft = MemberDraft::new(window, cx);

        let transcript = cx.new(|_| TranscriptView::new(AgentKind::Codex, None));

        let changed = cx.observe(&runtime, |this, _, cx| {
            if !this.loaded && !this.runtime.read(cx).loading() {
                this.targeting = DiscussionTargeting::from_room(this.runtime.read(cx).room());
                this.loaded = true;
            }

            this.timeline
                .sync_timeline(&this.runtime, &this.transcript, cx);

            cx.notify();
        });

        let targeting = DiscussionTargeting::from_room(runtime.read(cx).room());

        let loaded = !runtime.read(cx).loading();

        let mut pane = Self {
            runtime,
            focus: cx.focus_handle(),
            input,
            transcript,
            targeting,
            inspected: None,
            member_panes: BTreeMap::new(),
            timeline: TimelineMirror::default(),
            member_draft,
            error: None,
            loaded,
            submitting: false,
            adding_member: false,
            abandoning: false,
            _observers: vec![changed],
        };

        pane.timeline
            .sync_timeline(&pane.runtime, &pane.transcript, cx);

        pane
    }

    pub fn room_id(&self, cx: &App) -> RoomId {
        self.runtime.read(cx).id()
    }

    pub fn runtime(&self) -> &Entity<TeamRuntime> {
        &self.runtime
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.input.update(cx, |input, cx| input.focus(window, cx));
    }

    fn perform(&mut self, command: TeamCommand, cx: &mut Context<Self>) -> Task<bool> {
        let result = self
            .runtime
            .update(cx, |runtime, cx| runtime.command(command, cx));

        cx.spawn(async move |this, cx| {
            let result = result.await;

            this.update(cx, |this, cx| match result {
                Ok(()) => {
                    this.error = None;

                    cx.notify();

                    true
                }
                Err(error) => {
                    this.error = Some(match error {
                        TeamError::Unavailable => this
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
            })
            .unwrap_or(false)
        })
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.submitting || self.runtime.read(cx).loading() {
            return;
        }

        let input = UserInput {
            text: self.input.read(cx).text().to_string(),
            ..UserInput::default()
        };

        if input.text.trim().is_empty() {
            return;
        }

        // A member waiting for an approval or an answer cannot take a new
        // request until it has one; a request queued behind that wait would
        // sit as "waiting to start" with nothing to say why. The question is
        // drawn in this composer, so the answer is a click away.
        if let Some(name) = self.waiting_member_name(cx) {
            self.error = Some(t!("team-member-needs-answer", name = name).into_owned());

            cx.notify();

            return;
        }

        let active = self
            .runtime
            .read(cx)
            .room()
            .discussions()
            .iter()
            .any(|run| run.state() != DiscussionState::Completed);

        let command = match self.targeting.command(input, active) {
            Ok(command) => command,
            Err(MissingAuthor) => {
                self.error = Some(t!("team-select-author").into_owned());

                cx.notify();

                return;
            }
        };

        let text = self.input.read(cx).text().to_string();

        self.submitting = true;

        cx.notify();

        let result = self.perform(command, cx);

        cx.spawn_in(window, async move |this, cx| {
            let saved = result.await;

            let _ = this.update_in(cx, |this, window, cx| {
                this.submitting = false;

                cx.notify();

                if !saved {
                    return;
                }

                if *this.input.read(cx).text() == text {
                    this.input
                        .update(cx, |input, cx| input.set_value("", window, cx));
                }

                this.transcript.update(cx, |transcript, cx| {
                    transcript.scroll_to_bottom();

                    cx.notify();
                });
            });
        })
        .detach();
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
            self.perform(TeamCommand::Stop(member), cx).detach();
        }
    }

    fn inspect_member(&mut self, member: MemberId, window: &mut Window, cx: &mut Context<Self>) {
        self.inspected = self.member_pane(member, window, cx).map(|_| member);

        cx.notify();
    }

    /// The view of `member`'s session, made on first use.
    fn member_pane(
        &mut self,
        member: MemberId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<AgentPane>> {
        if let Some(pane) = self.member_panes.get(&member) {
            return Some(pane.clone());
        }

        let pane = self.runtime.update(cx, |runtime, cx| {
            runtime
                .hosts
                .get(&member)
                .map(|host| cx.new(|cx| AgentPane::attach_team_member(&host.owner, window, cx)))
        })?;

        self.member_panes.insert(member, pane.clone());

        Some(pane)
    }

    /// The first selected recipient whose session waits on the user.
    fn waiting_member_name(&self, cx: &App) -> Option<String> {
        let runtime = self.runtime.read(cx);

        runtime
            .room()
            .members()
            .iter()
            .filter(|member| self.targeting.selected().contains(&member.id()))
            .find(|member| {
                runtime
                    .hosts
                    .get(&member.id())
                    .is_some_and(|host| work_status(host.owner.session().read(cx)).interaction)
            })
            .map(|member| member.name().to_owned())
    }

    /// Every member that asks the user something, with what it asks: an
    /// approval or questions, drawn by the member's own view so answering
    /// here is the same as answering there.
    fn render_member_interactions(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let asking: Vec<_> = {
            let runtime = self.runtime.read(cx);

            runtime
                .room()
                .members()
                .iter()
                .filter(|member| {
                    runtime.hosts.get(&member.id()).is_some_and(|host| {
                        let session = host.owner.session().read(cx);
                        let state = session.controller.borrow();

                        state.input().waiting() || state.input().pending_count() > 0
                    })
                })
                .map(|member| (member.id(), member.name().to_owned()))
                .collect()
        };

        let mut blocks = Vec::new();

        for (id, name) in asking {
            let Some(pane) = self.member_pane(id, window, cx) else {
                continue;
            };

            let interactions =
                pane.update(cx, |pane, cx| pane.render_team_interactions(window, cx));

            if interactions.is_empty() {
                continue;
            }

            blocks.push(
                v_flex()
                    .w_full()
                    .child(
                        div()
                            .px_4()
                            .pt_2()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(t!("team-member-needs-answer", name = name).into_owned()),
                    )
                    .children(interactions)
                    .into_any_element(),
            );
        }

        blocks
    }

    fn add_member(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.adding_member || self.runtime.read(cx).loading() {
            return;
        }

        let Some((profile, config)) = self
            .member_draft
            .configuration(self.runtime.read(cx).room().workspace().clone(), cx)
        else {
            return;
        };

        self.adding_member = true;

        let task = self
            .runtime
            .update(cx, |runtime, cx| runtime.add_member(profile, config, cx));

        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;

            let _ = this.update_in(cx, |this, window, cx| {
                this.adding_member = false;

                match result {
                    Ok(id) => {
                        this.targeting.member_added(id);

                        this.member_draft.clear(window, cx);

                        this.error = None;

                        window.close_dialog(cx);
                    }
                    Err(error) => this.error = Some(error.to_string()),
                }

                cx.notify();
            });
        })
        .detach();
    }

    fn render_controls(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        // Each member's settings are listed from its own view, which is also
        // what applies a pick, so the menu needs those views before it
        // opens; the runtime then persists the pick into the room.
        let member_settings: Vec<_> = self
            .runtime
            .read(cx)
            .room()
            .members()
            .iter()
            .filter(|member| !member.excluded())
            .map(|member| (member.id(), member.name().to_owned(), member.profile().kind))
            .collect::<Vec<_>>()
            .into_iter()
            .filter_map(|(id, name, kind)| {
                self.member_pane(id, window, cx)
                    .map(|member_pane| (name, kind, member_pane))
            })
            .collect();

        let room = self.runtime.read(cx).room();

        let pending_recovery: Vec<_> = self
            .runtime
            .read(cx)
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

        let recipients = self.targeting.render_recipients(room, cx.entity());

        let active = room
            .discussions()
            .iter()
            .find(|run| run.state() != DiscussionState::Completed)
            .map(|run| (run.id(), run.mode()));

        let mode = self
            .targeting
            .render_mode(room, active.is_some(), cx.entity());

        let controls = room.controls().clone();
        let pane = cx.entity();

        let settings = settings_pill(Button::new("team-settings"))
            .label(t!("team-options"))
            .dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu, window, cx| {
                let summaries = pane.clone();

                let mut menu = menu.item(
                    PopupMenuItem::new(t!("team-auto-summaries"))
                        .checked(controls.automatic_summaries)
                        .on_click(move |_, _, cx| {
                            summaries.update(cx, |pane, cx| {
                                pane.perform(
                                    TeamCommand::AutomaticSummaries(!controls.automatic_summaries),
                                    cx,
                                )
                                .detach();
                            });
                        }),
                );

                if !member_settings.is_empty() {
                    menu = menu.separator();
                }

                for (name, kind, member_pane) in &member_settings {
                    let member_pane = member_pane.clone();
                    let kind = *kind;

                    menu = menu.submenu(name.clone(), window, cx, move |submenu, window, cx| {
                        let settings = {
                            let session = member_pane.read(cx).session.borrow();

                            harness_settings(&session.controls, kind, cx)
                        };

                        harness_submenus(submenu, &member_pane, settings, window, cx)
                    });
                }

                menu
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
                                    .on_ok(move |_, window, cx| {
                                        let started = pane.update(cx, |pane, _| {
                                            if pane.abandoning {
                                                return false;
                                            }

                                            pane.abandoning = true;

                                            true
                                        });

                                        if !started {
                                            return false;
                                        }

                                        let pane = pane.downgrade();
                                        let attempts = attempts.clone();

                                        window
                                            .spawn(cx, async move |cx| {
                                                let mut saved = true;

                                                for (id, _) in attempts {
                                                    let task = cx.update(|_, cx| {
                                                        pane.update(cx, |pane, cx| {
                                                            pane.perform(
                                                                TeamCommand::AbandonRestored(id),
                                                                cx,
                                                            )
                                                        })
                                                    });

                                                    let Ok(Ok(task)) = task else {
                                                        return;
                                                    };

                                                    if !task.await {
                                                        saved = false;

                                                        break;
                                                    }
                                                }

                                                let _ = cx.update(|window, cx| {
                                                    let _ = pane.update(cx, |pane, cx| {
                                                        pane.abandoning = false;

                                                        cx.notify();
                                                    });

                                                    if saved {
                                                        window.close_dialog(cx);
                                                    }
                                                });
                                            })
                                            .detach();

                                        false
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
                    .disabled(self.runtime.read(cx).pending_recovery().next().is_some())
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
                        )
                        .detach();
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
                                    )
                                    .detach();
                                });
                            },
                        ))
                        .item(
                            PopupMenuItem::new(t!("team-finish")).on_click(move |_, _, cx| {
                                finish.update(cx, |pane, cx| {
                                    pane.perform(TeamCommand::Finish(id), cx).detach();
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
            let Some(command) =
                DiscussionTargeting::change_mode(id, selection == 2, self.targeting.author())
            else {
                return;
            };

            let result = self.perform(command, cx);

            cx.spawn(async move |this, cx| {
                if result.await {
                    let _ = this.update(cx, |this, cx| {
                        this.targeting.set_mode(selection);

                        cx.notify();
                    });
                }
            })
            .detach();

            return;
        }

        self.targeting.set_mode(selection);

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

        if let Some(id) = active
            && let Some(command) =
                DiscussionTargeting::change_mode(id, self.targeting.moderated(), Some(author))
        {
            let result = self.perform(command, cx);

            cx.spawn(async move |this, cx| {
                if result.await {
                    let _ = this.update(cx, |this, cx| {
                        this.targeting.set_author(author);

                        cx.notify();
                    });
                }
            })
            .detach();

            return;
        }

        self.targeting.set_author(author);

        cx.notify();
    }

    fn status_text(&self, cx: &App) -> String {
        let runtime = self.runtime.read(cx);
        let room = runtime.room();

        let mut parts = Vec::new();

        for member in room
            .members()
            .iter()
            .filter(|member| self.targeting.selected().contains(&member.id()))
        {
            let status = runtime.hosts.get(&member.id()).map(|host| {
                host.owner
                    .session()
                    .read(cx)
                    .controller
                    .borrow()
                    .runtime()
                    .status()
            });

            let needs_recovery = runtime
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
                    remaining = run.remaining_non_report_turns(room.attempts())
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
                        PauseReason::MemberUnavailable(_) => {
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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

        if let Some((id, pane)) = self
            .inspected
            .and_then(|id| self.member_panes.get(&id).map(|pane| (id, pane.clone())))
        {
            let name = self
                .runtime
                .read(cx)
                .room()
                .member(id)
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
        let loading = runtime.loading();

        let failure = self
            .error
            .clone()
            .or_else(|| runtime.error().map(str::to_owned));

        let status = self.status_text(cx);
        let interactions = self.render_member_interactions(window, cx);
        let controls = self.render_controls(window, cx);

        let send = send_button(
            "team-send",
            running,
            loading || self.submitting || (!running && self.targeting.selected().is_empty()),
        )
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
                                .children(interactions)
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
