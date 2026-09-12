mod membership;
mod options;

mod timeline;

#[cfg(test)]
mod tests;

use std::collections::BTreeSet;

use gpui::prelude::*;
use gpui::{App, Context, Entity, FocusHandle, Focusable, Render, Subscription, Window, div, px};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Enter, Escape, Textarea, TextareaState};
use gpui_component::{ActiveTheme as _, Disableable as _, IconName, Sizable as _, h_flex, v_flex};
use nmt_agent::session::AgentKind;
use nmt_agent::team::content::UserInput;
use nmt_agent::team::discussion::{DiscussionMode, DiscussionState};
use nmt_agent::team::identity::{MemberId, RoomId};
use nmt_agent::team::session::TeamError;
use rust_i18n::t;

use crate::agent_tab::AgentPane;
use crate::agent_tab::settings::{AgentSettings, UI_RADIUS};
use crate::agent_tab::team::view::timeline::TimelineRow;
use crate::agent_tab::team::{TeamCommand, TeamRuntime};
use crate::agent_tab::transcript::{TranscriptView, transcript_column};
use crate::agent_tab::view::composer_layout::{
    composer_card, composer_controls_row, composer_input_row,
};
use crate::agent_tab::view::{ComposerEnterBehavior, StopResponseIcon, composer_enter_behavior};

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
    rows: Vec<TimelineRow>,
    revision: Option<u64>,
    profile_index: usize,
    member_name: Entity<TextareaState>,
    member_role: Entity<TextareaState>,
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

        let member_name = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 1)
                .placeholder(t!("team-member-name").into_owned())
        });

        let member_role = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 3)
                .placeholder(t!("team-member-role").into_owned())
        });

        let transcript = cx.new(|_| TranscriptView::new(AgentKind::Codex, None));

        let changed = cx.observe(&runtime, |this, _, cx| {
            this.sync_timeline(cx);

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
            rows: Vec::new(),
            revision: None,
            profile_index: 0,
            member_name,
            member_role,
            error: None,
            _observers: vec![changed],
        };

        pane.sync_timeline(cx);

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
        let opacity = settings.background_opacity;

        let surface = v_flex()
            .debug_selector(|| "team-surface".into())
            .size_full()
            .relative()
            .min_h_0()
            .bg(background.alpha(opacity))
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
