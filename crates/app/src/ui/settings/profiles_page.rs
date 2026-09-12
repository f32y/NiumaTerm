use rust_i18n::t;

use crate::ui::settings::*;

/// The Profiles page: exactly two groups — Terminal Profile and Agent
/// Profile — so the sidebar shows two stable entries. Profile cards render
/// inside each group instead of as their own groups, which would otherwise
/// add one sidebar entry per profile under `single_group_pages`.
pub(super) fn profiles_page(profiles: &[Profile], agent_profiles: &[AgentProfile]) -> SettingPage {
    SettingPage::new(t!("settings-profiles-title"))
        .default_open(true)
        .group(terminal_profiles_group(profiles))
        .group(agent_profiles_group(agent_profiles))
}

fn terminal_profiles_group(profiles: &[Profile]) -> SettingGroup {
    // Selector options come from the current names; the settings view is
    // rebuilt per render, so renames refresh the list immediately.
    let options: Vec<(SharedString, SharedString)> = profiles
        .iter()
        .enumerate()
        .map(|(ix, p)| {
            let label = if p.name.is_empty() {
                t!("settings-profiles-unnamed", n = (ix + 1)).into_owned()
            } else {
                p.name.clone()
            };

            (p.name.clone().into(), label.into())
        })
        .collect();

    let mut group = SettingGroup::new()
        .title(t!("settings-profiles-terminal-group"))
        .item(SettingItem::new(
            t!("settings-profiles-default"),
            SettingField::dropdown(
                options,
                |cx| cx.global::<AppSettings>().default_profile.clone().into(),
                |value, cx| {
                    cx.global_mut::<AppSettings>().default_profile = value.to_string();
                },
            ),
        ))
        .item(SettingItem::new(
            t!("settings-profiles-add"),
            SettingField::render(|_, _, _| {
                Button::new("profile-add")
                    .outline()
                    .label(t!("settings-common-add"))
                    .on_click(|_, _, cx: &mut App| {
                        cx.global_mut::<AppSettings>().add_profile();
                    })
            }),
        ));

    let count = profiles.len();

    for ix in 0..count {
        group = group.item(terminal_profile_card(ix, count));
    }

    group
}

fn terminal_profile_card(ix: usize, count: usize) -> SettingItem {
    SettingItem::render(move |options, window, cx| {
        // get(ix): the render closure outlives profile removal, so a stale
        // index must read as empty, not panic.
        let profile = cx
            .global::<AppSettings>()
            .profiles
            .get(ix)
            .cloned()
            .unwrap_or_default();

        let title = if profile.name.is_empty() {
            t!("settings-profiles-unnamed", n = (ix + 1)).into_owned()
        } else {
            profile.name.clone()
        };

        let disabled = options.is_disabled();
        let size = options.size();

        let name_input = card_text_input(
            format!("terminal-profile-name-{ix}"),
            profile.name.clone().into(),
            false,
            move |value, cx| cx.global_mut::<AppSettings>().rename_profile(ix, value),
            window,
            cx,
        );

        let shell_input = card_text_input(
            format!("terminal-profile-shell-{ix}"),
            profile.shell.clone().into(),
            false,
            move |value, cx| {
                if let Some(profile) = cx.global_mut::<AppSettings>().profiles.get_mut(ix) {
                    profile.shell = value;
                }
            },
            window,
            cx,
        );

        let args_input = card_text_input(
            format!("terminal-profile-args-{ix}"),
            profile.args.clone().into(),
            false,
            move |value, cx| {
                if let Some(profile) = cx.global_mut::<AppSettings>().profiles.get_mut(ix) {
                    profile.args = value;
                }
            },
            window,
            cx,
        );

        let browse_input = shell_input.clone();

        let shell_control = v_flex()
            .gap_2()
            .w_64()
            .child(
                Input::new(&shell_input)
                    .disabled(disabled)
                    .with_size(size)
                    .w_full(),
            )
            .child(
                h_flex().w_full().justify_end().child(
                    Button::new(("profile-shell-browse", ix))
                        .outline()
                        .label(t!("settings-common-browse"))
                        .disabled(disabled)
                        .w(relative(1. / 3.))
                        .on_click(move |_, window, cx| {
                            let rx = cx.prompt_for_paths(PathPromptOptions {
                                files: true,
                                directories: false,
                                multiple: false,
                                prompt: Some(t!("settings-profiles-select-shell").into()),
                                file_types: vec![FileDialogFilter {
                                    name: t!("settings-profiles-executables-filter").into(),
                                    extensions: vec!["exe".into()],
                                }],
                            });

                            let input = browse_input.clone();

                            window
                                .spawn(cx, async move |cx| {
                                    if let Ok(Ok(Some(paths))) = rx.await
                                        && let Some(path) = paths.first()
                                    {
                                        let value = path.display().to_string();

                                        let _ =
                                            cx.update_global(|settings: &mut AppSettings, _, _| {
                                                if let Some(profile) = settings.profiles.get_mut(ix)
                                                {
                                                    profile.shell = value.clone();
                                                }
                                            });

                                        let _ = input.update_in(cx, |input, window, cx| {
                                            input.set_value(value, window, cx);
                                        });
                                    }
                                })
                                .detach();
                        }),
                ),
            );

        let remove_button = Button::new(("profile-remove", ix))
            .danger()
            .label(t!("settings-common-remove"))
            .disabled(disabled || count <= 1)
            .on_click(move |_, window, cx: &mut App| {
                let name = cx
                    .global::<AppSettings>()
                    .profiles
                    .get(ix)
                    .map(|profile| profile.name.clone())
                    .unwrap_or_default();

                let subject = if name.is_empty() {
                    t!("settings-profiles-this-profile").to_string()
                } else {
                    t!("settings-profiles-named-profile", name = &name).into_owned()
                };

                window.open_alert_dialog(cx, move |alert, _, _| {
                    alert
                        .confirm()
                        .title(t!("settings-profiles-remove-title"))
                        .description(
                            t!("settings-profiles-remove-confirm", subject = &subject).into_owned(),
                        )
                        .on_ok(move |_, _, cx| {
                            cx.global_mut::<AppSettings>().remove_profile(ix);

                            true
                        })
                });
            });

        GroupBox::new().outline().title(title).child(
            v_flex()
                .w_full()
                .gap_4()
                .child(card_row(
                    t!("settings-common-name"),
                    "",
                    Input::new(&name_input)
                        .disabled(disabled)
                        .with_size(size)
                        .w_64(),
                    cx,
                ))
                .child(card_row(
                    t!("settings-profiles-shell-path"),
                    "",
                    shell_control,
                    cx,
                ))
                .child(card_row(
                    t!("settings-profiles-arguments"),
                    "",
                    Input::new(&args_input)
                        .disabled(disabled)
                        .with_size(size)
                        .w_64(),
                    cx,
                ))
                .child(card_row(
                    t!("settings-profiles-remove-title"),
                    "",
                    remove_button,
                    cx,
                )),
        )
    })
}

fn agent_profiles_group(agent_profiles: &[AgentProfile]) -> SettingGroup {
    let options: Vec<(SharedString, SharedString)> = agent_profiles
        .iter()
        .enumerate()
        .map(|(ix, p)| {
            let label = if p.name.is_empty() {
                t!("settings-profiles-agent-unnamed", n = (ix + 1)).into_owned()
            } else {
                p.name.clone()
            };

            (p.name.clone().into(), label.into())
        })
        .collect();

    let group = SettingGroup::new()
        .title(t!("settings-profiles-agent-group"))
        .item(SettingItem::new(
            t!("settings-profiles-default"),
            SettingField::dropdown(
                options,
                |cx| {
                    cx.global::<AppSettings>()
                        .default_agent_profile
                        .clone()
                        .into()
                },
                |value, cx| {
                    cx.global_mut::<AppSettings>().default_agent_profile = value.to_string();
                },
            ),
        ))
        .item(SettingItem::new(
            t!("settings-profiles-add"),
            SettingField::render(|_, _, _| {
                Button::new("agent-profile-add")
                    .outline()
                    .label(t!("settings-common-add"))
                    .on_click(|_, window, cx: &mut App| {
                        open_agent_profile_dialog(None, window, cx);
                    })
            }),
        ));

    group.item(SettingItem::render(|_, window, cx| {
        agent_profile_list(window, cx)
    }))
}
