use crate::ui::settings::*;

/// The Profiles page: exactly two groups — Terminal Profile and Agent
/// Profile — so the sidebar shows two stable entries. Profile cards render
/// inside each group instead of as their own groups, which would otherwise
/// add one sidebar entry per profile under `single_group_pages`.
pub(super) fn profiles_page(profiles: &[Profile], agent_profiles: &[AgentProfile]) -> SettingPage {
    SettingPage::new("Profiles")
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
                format!("Profile {}", ix + 1)
            } else {
                p.name.clone()
            };

            (
                SharedString::from(p.name.clone()),
                SharedString::from(label),
            )
        })
        .collect();

    let mut group = SettingGroup::new()
        .title("Terminal Profile")
        .description("Shell profiles available to terminals.")
        .item(
            SettingItem::new(
                "Default Profile",
                SettingField::dropdown(
                    options,
                    |cx| cx.global::<AppSettings>().default_profile.clone().into(),
                    |value, cx| {
                        cx.global_mut::<AppSettings>().default_profile = value.to_string();
                    },
                ),
            )
            .description("Profile used by new terminals."),
        )
        .item(
            SettingItem::new(
                "Add Profile",
                SettingField::render(|_, _, _| {
                    Button::new("profile-add").outline().label("Add").on_click(
                        |_, _, cx: &mut App| {
                            cx.global_mut::<AppSettings>().add_profile();
                        },
                    )
                }),
            )
            .description("Create a new profile."),
        );

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
            format!("Profile {}", ix + 1)
        } else {
            profile.name.clone()
        };

        let disabled = options.disabled;
        let size = options.size;

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
                        .label("Browse")
                        .disabled(disabled)
                        .w(relative(1. / 3.))
                        .on_click(move |_, window, cx| {
                            let rx = cx.prompt_for_paths(PathPromptOptions {
                                files: true,
                                directories: false,
                                multiple: false,
                                prompt: Some("Select shell executable".into()),
                                file_types: vec![FileDialogFilter {
                                    name: "Executables".into(),
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
            .label("Remove")
            .disabled(disabled || count <= 1)
            .on_click(move |_, window, cx: &mut App| {
                let name = cx
                    .global::<AppSettings>()
                    .profiles
                    .get(ix)
                    .map(|profile| profile.name.clone())
                    .unwrap_or_default();
                let subject = if name.is_empty() {
                    "this profile".to_string()
                } else {
                    format!("profile \"{name}\"")
                };

                window.open_alert_dialog(cx, move |alert, _, _| {
                    alert
                        .confirm()
                        .title("Remove Profile")
                        .description(format!("Remove {subject}? This cannot be undone."))
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
                    "Name",
                    "Display name; the card title and default selector follow it.",
                    Input::new(&name_input)
                        .disabled(disabled)
                        .with_size(size)
                        .w_64(),
                    cx,
                ))
                .child(card_row(
                    "Shell Path",
                    "Path to the shell executable.",
                    shell_control,
                    cx,
                ))
                .child(card_row(
                    "Arguments",
                    "Command-line arguments, space-separated.",
                    Input::new(&args_input)
                        .disabled(disabled)
                        .with_size(size)
                        .w_64(),
                    cx,
                ))
                .child(card_row(
                    "Remove Profile",
                    if count <= 1 {
                        "The last profile cannot be removed."
                    } else {
                        "Removing the default falls back to the first profile."
                    },
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
                format!("Agent Profile {}", ix + 1)
            } else {
                p.name.clone()
            };

            (
                SharedString::from(p.name.clone()),
                SharedString::from(label),
            )
        })
        .collect();

    let group = SettingGroup::new()
        .title("Agent Profile")
        .description("Launch profiles for agent tabs (Claude Code and Codex).")
        .item(
            SettingItem::new(
                "Default Profile",
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
            )
            .description("Profile used by new agent tabs."),
        )
        .item(
            SettingItem::new(
                "Add Profile",
                SettingField::render(|_, _, _| {
                    Button::new("agent-profile-add")
                        .outline()
                        .label("Add")
                        .on_click(|_, window, cx: &mut App| {
                            open_agent_profile_dialog(None, window, cx);
                        })
                }),
            )
            .description("Create a new agent profile."),
        );

    group.item(SettingItem::render(|_, window, cx| {
        agent_profile_list(window, cx)
    }))
}
