use std::collections::BTreeSet;

use gpui::prelude::*;
use gpui::{App, Context, Entity, IntoElement, SharedString, Window, div, px};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Textarea, TextareaState};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::{
    ActiveTheme as _, Disableable as _, IconNamed, Size, WindowExt as _, h_flex, v_flex,
};
use nmt_agent::AgentWorkspace;
use nmt_agent::team::member::{HistoryScope, MemberConfig, ProfileReference};
use nmt_config::profile::AgentProfile;
use rand::seq::SliceRandom as _;
use rust_i18n::t;

use crate::agent_tab::settings::AgentSettings;
use crate::agent_tab::team::TeamRuntime;
use crate::agent_tab::team::view::TeamPane;
use crate::agent_tab::thread_controls::{launch_effort, launch_model, stored_thread_settings};

struct DiceIcon;

impl IconNamed for DiceIcon {
    fn path(self) -> SharedString {
        "icons/dice.svg".into()
    }
}

pub(super) struct MemberDraft {
    profile_index: usize,
    member_name: Entity<TextareaState>,
    member_role: Entity<TextareaState>,
}

impl MemberDraft {
    pub(super) fn new(window: &mut Window, cx: &mut App) -> Self {
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

        Self {
            profile_index: 0,
            member_name,
            member_role,
        }
    }

    pub(super) fn configuration(
        &self,
        roots: AgentWorkspace,
        cx: &App,
    ) -> Option<(AgentProfile, MemberConfig)> {
        let profiles = &cx.global::<AgentSettings>().profiles;

        let profile = profiles
            .get(self.profile_index)
            .or_else(|| profiles.first())
            .cloned()?;

        let kind = profile.kind;

        let mut settings = stored_thread_settings(kind, &profile, cx)
            .cloned()
            .unwrap_or_default();

        settings.model = launch_model(kind, &profile).or(settings.model);
        settings.effort = launch_effort(&profile).or(settings.effort);

        let config = MemberConfig {
            name: self.member_name.read(cx).text().to_string(),
            profile: ProfileReference {
                kind,
                name: profile.name.clone(),
            },
            roots,
            settings,
            role: self.member_role.read(cx).text().to_string(),
            history: HistoryScope::CompletedPublic,
        };

        Some((profile, config))
    }

    pub(super) fn clear(&self, window: &mut Window, cx: &mut App) {
        self.member_name
            .update(cx, |input, cx| input.set_value("", window, cx));

        self.member_role
            .update(cx, |input, cx| input.set_value("", window, cx));
    }

    pub(super) fn open_member_form(
        &mut self,
        runtime: &Entity<TeamRuntime>,
        window: &mut Window,
        cx: &mut Context<TeamPane>,
    ) {
        if self
            .member_name
            .read(cx)
            .text()
            .to_string()
            .trim()
            .is_empty()
        {
            self.randomize_member_name(runtime, window, cx);
        }

        let pane = cx.entity();

        window.open_dialog(cx, move |dialog, _, _| {
            let pane = pane.clone();

            dialog
                .title(t!("team-add-member"))
                .content(move |content, window, cx| {
                    content.child(pane.update(cx, |pane, cx| {
                        pane.member_draft
                            .render_member_form(pane.error.clone(), window, cx)
                            .into_any_element()
                    }))
                })
        });
    }

    fn randomize_member_name(
        &mut self,
        runtime: &Entity<TeamRuntime>,
        window: &mut Window,
        cx: &mut Context<TeamPane>,
    ) {
        let mut existing: BTreeSet<_> = runtime
            .read(cx)
            .room()
            .members()
            .iter()
            .map(|member| member.name().to_lowercase())
            .collect();

        existing.insert(
            self.member_name
                .read(cx)
                .text()
                .to_string()
                .trim()
                .to_lowercase(),
        );

        let name = suggest_member_name(&existing);

        self.member_name
            .update(cx, |input, cx| input.set_value(name, window, cx));
    }

    pub(super) fn render_member_form(
        &mut self,
        error: Option<String>,
        window: &Window,
        cx: &mut Context<TeamPane>,
    ) -> impl IntoElement {
        // A single-line textarea includes its line, vertical padding, and both borders.
        let control_height = window.rem_size() * 1.25 + Size::Medium.input_py() * 2.0 + px(2.0);
        let profiles = &cx.global::<AgentSettings>().profiles;
        let count = profiles.len();

        let profile = profiles
            .get(self.profile_index)
            .or_else(|| profiles.first())
            .map(|profile| profile.name.clone())
            .unwrap_or_else(|| t!("team-no-profiles").into_owned());

        let profiles: Vec<_> = profiles
            .iter()
            .map(|profile| profile.name.clone())
            .collect();

        let selected = self.profile_index;
        let pane = cx.entity();

        v_flex()
            .gap_2()
            .pt_2()
            .child(
                Button::new("team-profile")
                    .ghost()
                    .h(control_height)
                    .label(format!("{profile} ▾"))
                    .disabled(count == 0)
                    .dropdown_menu(move |menu, _, _| {
                        let mut menu = menu;

                        for (index, name) in profiles.iter().enumerate() {
                            let pane = pane.clone();

                            menu = menu.item(
                                PopupMenuItem::new(name.clone())
                                    .checked(index == selected)
                                    .on_click(move |_, _, cx| {
                                        pane.update(cx, |pane, cx| {
                                            pane.member_draft.profile_index = index;

                                            cx.notify();
                                        });
                                    }),
                            );
                        }

                        menu
                    }),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Textarea::new(&self.member_name)),
                    )
                    .child(
                        Button::new("team-random-name")
                            .ghost()
                            .size(control_height)
                            .flex_none()
                            .icon(DiceIcon)
                            .tooltip(t!("team-random-name"))
                            .accessibility_label(t!("team-random-name"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.member_draft
                                    .randomize_member_name(&this.runtime, window, cx);
                            })),
                    ),
            )
            .child(Textarea::new(&self.member_role))
            .child(
                Button::new("team-create-member")
                    .primary()
                    .h(control_height)
                    .label(t!("team-add-member"))
                    .disabled(count == 0)
                    .on_click(cx.listener(|this, _, window, cx| this.add_member(window, cx))),
            )
            .when_some(error, |view, error| {
                view.child(div().text_sm().text_color(cx.theme().danger).child(error))
            })
    }
}

fn suggest_member_name(existing: &BTreeSet<String>) -> String {
    let pool = t!("team-member-name-pool");

    let mut names: Vec<_> = pool
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .collect();

    names.shuffle(&mut rand::rng());

    for name in &names {
        if !existing.contains(&name.to_lowercase()) {
            return (*name).to_owned();
        }
    }

    let base = names.first().copied().unwrap_or("Agent");

    let mut suffix = 2usize;

    loop {
        let name = format!("{base} {suffix}");

        if !existing.contains(&name.to_lowercase()) {
            return name;
        }

        suffix += 1;
    }
}
