use crate::ui::settings::*;

/// Draft edited in the agent-profile dialog: `target` is the list index in
/// edit mode, `None` while adding. Inputs write here; only Save commits the
/// draft into `AppSettings`, so Cancel is a plain close.
#[derive(Default)]
struct AgentProfileDraft {
    target: Option<usize>,
    profile: AgentProfile,
}

impl Global for AgentProfileDraft {}

/// Open the add/edit dialog for an agent profile. `target` is the index in
/// `AppSettings::agent_profiles` for edit mode, `None` for a new profile.
/// The dialog edits an [`AgentProfileDraft`]; Save commits, Cancel discards.
pub(super) fn open_agent_profile_dialog(target: Option<usize>, window: &mut Window, cx: &mut App) {
    let profile = match target {
        Some(ix) => cx
            .global::<AppSettings>()
            .agent_profiles
            .get(ix)
            .cloned()
            .unwrap_or_default(),
        // A new profile starts from the Claude Code built-in with a blank
        // name; Save fills in a unique placeholder.
        None => AgentProfile {
            name: String::new(),
            ..builtin_agent_profile(AgentProfileKind::ClaudeCode)
        },
    };
    cx.set_global(AgentProfileDraft { target, profile });

    window.open_dialog(cx, move |dialog, window, _| {
        let title = if target.is_some() {
            "Edit Agent Profile"
        } else {
            "Add Agent Profile"
        };
        let settings_height = window.viewport_size().height;
        let dialog_height = settings_height * 0.6;
        let dialog_top = (settings_height - dialog_height) * 0.5;

        let mut footer = DialogFooter::new()
            .child(DialogClose::new().child(Button::new("agent-profile-cancel").label("Cancel")));

        if let Some(ix) = target {
            footer = footer.child(
                Button::new("agent-profile-delete")
                    .danger()
                    .label("Delete")
                    .on_click(move |_, window, cx: &mut App| {
                        let name = cx.global::<AgentProfileDraft>().profile.name.clone();
                        let subject = if name.is_empty() {
                            "this profile".to_string()
                        } else {
                            format!("profile \"{name}\"")
                        };

                        window.open_alert_dialog(cx, move |alert, _, _| {
                            alert
                                .confirm()
                                .title("Delete Agent Profile")
                                .description(format!("Delete {subject}? This cannot be undone."))
                                .on_ok(move |_, window, cx| {
                                    cx.global_mut::<AppSettings>().remove_agent_profile(ix);
                                    // Pop the confirm and the edit dialog
                                    // explicitly, then return false so the
                                    // alert's own close path does not pop a
                                    // third dialog (the settings one).
                                    window.close_dialog(cx);
                                    window.close_dialog(cx);
                                    false
                                })
                        });
                    }),
            );
        }

        footer = footer.child(
            Button::new("agent-profile-save")
                .primary()
                .label("Save")
                .on_click(|_, window, cx: &mut App| {
                    save_agent_profile_draft(cx);
                    window.close_dialog(cx);
                }),
        );

        dialog
            .title(title)
            .overlay_closable(false)
            .margin_top(dialog_top)
            .w(px(560.))
            .h(dialog_height)
            .content(|content, window, cx| {
                content.overflow_hidden().child(
                    div().flex_1().overflow_hidden().child(
                        v_flex()
                            .size_full()
                            .overflow_y_scrollbar()
                            .child(div().pr_2().child(agent_profile_dialog_content(window, cx))),
                    ),
                )
            })
            .footer(footer)
    });
}

/// Commit the dialog draft into `AppSettings`: dedupe the name, then update
/// the edited entry or append a new one.
fn save_agent_profile_draft(cx: &mut App) {
    let target = cx.global::<AgentProfileDraft>().target;
    let mut profile = cx.global::<AgentProfileDraft>().profile.clone();

    let settings = cx.global_mut::<AppSettings>();
    profile.name = settings.unique_agent_profile_name(&profile.name, profile.kind, target);

    match target {
        Some(ix) => settings.update_agent_profile(ix, profile),
        None => {
            settings.agent_profiles.push(profile);

            // Adding to a previously empty list makes the new profile the
            // default, so NewAgentTab immediately uses it.
            if settings.default_agent_profile.is_empty() {
                settings.default_agent_profile = settings
                    .agent_profiles
                    .last()
                    .map(|p| p.name.clone())
                    .unwrap_or_default();
            }
        }
    }
}

/// One of the two Base Agent choice buttons in the add dialog; the selected
/// kind renders as the primary variant.
fn kind_choice_button(
    id: &'static str,
    kind: AgentProfileKind,
    current: AgentProfileKind,
) -> Button {
    let button = Button::new(id).label(agent_kind_label(kind));
    let button = if kind == current {
        button.primary()
    } else {
        button.outline()
    };

    button.on_click(move |_, _, cx: &mut App| {
        let draft = cx.global_mut::<AgentProfileDraft>();
        if draft.profile.kind == kind {
            return;
        }

        // The executable follows the kind while it still holds a built-in
        // default; a hand-typed path survives the switch.
        let executable = draft.profile.executable.trim();
        if executable.is_empty() || executable == "claude" || executable == "codex" {
            draft.profile.executable = builtin_agent_profile(kind).executable;
        }
        draft.profile.kind = kind;
    })
}

fn agent_profile_dialog_content(window: &mut Window, cx: &mut App) -> Div {
    let profile = cx.global::<AgentProfileDraft>().profile.clone();
    let is_edit = cx.global::<AgentProfileDraft>().target.is_some();

    let kind_label = agent_kind_label(profile.kind);
    let key_env = match profile.kind {
        AgentProfileKind::ClaudeCode => "ANTHROPIC_API_KEY",
        AgentProfileKind::Codex => "OPENAI_API_KEY",
    };
    let endpoint_on = profile.use_custom_endpoint;

    let name_input = card_text_input(
        "agent-profile-dialog-name".to_string(),
        profile.name.clone().into(),
        false,
        |value, cx| cx.global_mut::<AgentProfileDraft>().profile.name = value,
        window,
        cx,
    );

    let exe_input = card_text_input(
        "agent-profile-dialog-exe".to_string(),
        profile.executable.clone().into(),
        false,
        |value, cx| cx.global_mut::<AgentProfileDraft>().profile.executable = value,
        window,
        cx,
    );

    let model_input = card_text_input(
        "agent-profile-dialog-model".to_string(),
        profile.model.clone().into(),
        false,
        |value, cx| cx.global_mut::<AgentProfileDraft>().profile.model = value,
        window,
        cx,
    );

    let url_input = card_text_input(
        "agent-profile-dialog-url".to_string(),
        profile.api_base_url.clone().into(),
        false,
        |value, cx| cx.global_mut::<AgentProfileDraft>().profile.api_base_url = value,
        window,
        cx,
    );

    let key_input = card_text_input(
        "agent-profile-dialog-key".to_string(),
        profile.api_key.clone().into(),
        false,
        |value, cx| cx.global_mut::<AgentProfileDraft>().profile.api_key = value,
        window,
        cx,
    );

    let kind_control: AnyElement = if is_edit {
        // The kind decides the backend protocol; changing it under an existing
        // profile would silently repurpose tabs and persisted state, so it
        // is fixed after creation.
        Label::new(kind_label).text_sm().into_any_element()
    } else {
        h_flex()
            .gap_2()
            .child(kind_choice_button(
                "agent-profile-kind-claude",
                AgentProfileKind::ClaudeCode,
                profile.kind,
            ))
            .child(kind_choice_button(
                "agent-profile-kind-codex",
                AgentProfileKind::Codex,
                profile.kind,
            ))
            .into_any_element()
    };

    let endpoint_switch = Switch::new("agent-profile-dialog-endpoint")
        .checked(endpoint_on)
        .on_click(|checked: &bool, _, cx: &mut App| {
            cx.global_mut::<AgentProfileDraft>()
                .profile
                .use_custom_endpoint = *checked;
        });

    let mut env_rows = v_flex().w_full().gap_2();
    for (row, var) in profile.env.iter().enumerate() {
        let env_name_input = card_text_input(
            format!("agent-profile-dialog-env-{row}-name"),
            var.name.clone().into(),
            false,
            move |value, cx| {
                if let Some(var) = cx
                    .global_mut::<AgentProfileDraft>()
                    .profile
                    .env
                    .get_mut(row)
                {
                    var.name = value;
                }
            },
            window,
            cx,
        );

        let env_value_input = card_text_input(
            format!("agent-profile-dialog-env-{row}-value"),
            var.value.clone().into(),
            false,
            move |value, cx| {
                if let Some(var) = cx
                    .global_mut::<AgentProfileDraft>()
                    .profile
                    .env
                    .get_mut(row)
                {
                    var.value = value;
                }
            },
            window,
            cx,
        );

        env_rows = env_rows.child(
            h_flex()
                .w_full()
                .gap_2()
                .child(Input::new(&env_name_input).flex_1())
                .child(Input::new(&env_value_input).flex_1())
                .child(
                    Button::new(SharedString::from(format!(
                        "agent-profile-dialog-env-remove-{row}"
                    )))
                    .outline()
                    .label("Remove")
                    .on_click(move |_, _, cx: &mut App| {
                        let env = &mut cx.global_mut::<AgentProfileDraft>().profile.env;
                        if row < env.len() {
                            env.remove(row);
                        }
                    }),
                ),
        );
    }

    let env_section = v_flex()
        .w_full()
        .gap_2()
        .child(Label::new("Environment Variables").text_sm())
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("Extra environment variables applied to the agent process."),
        )
        .child(env_rows)
        .child(
            h_flex().child(
                Button::new("agent-profile-dialog-env-add")
                    .outline()
                    .label("Add Variable")
                    .on_click(|_, _, cx: &mut App| {
                        cx.global_mut::<AgentProfileDraft>()
                            .profile
                            .env
                            .push(EnvVar::default());
                    }),
            ),
        );

    v_flex()
        .w_full()
        .gap_4()
        .child(card_row(
            "Name",
            "Display name; it keys the default selector and per-profile settings.",
            Input::new(&name_input).w_64(),
            cx,
        ))
        .child(card_row(
            "Base Agent",
            "Which agent CLI this profile launches.",
            kind_control,
            cx,
        ))
        .child(card_row(
            "Executable Path",
            "Executable name or full path; a bare name resolves via PATH.",
            Input::new(&exe_input).w_64(),
            cx,
        ))
        .child(card_row(
            "Model",
            match profile.kind {
                AgentProfileKind::ClaudeCode => {
                    "Initial model; passed to Claude Code as ANTHROPIC_MODEL."
                }
                AgentProfileKind::Codex => {
                    "Initial model; passed to Codex when its app-server thread starts."
                }
            },
            Input::new(&model_input).w_64(),
            cx,
        ))
        .child(card_row(
            "Use Custom API Endpoint",
            "Route this agent through your own API endpoint.",
            endpoint_switch,
            cx,
        ))
        .child(card_row(
            "API URL",
            match profile.kind {
                AgentProfileKind::ClaudeCode => {
                    "Exported as ANTHROPIC_BASE_URL while the custom endpoint is enabled."
                        .to_string()
                }
                AgentProfileKind::Codex => {
                    "Injected as a profile-scoped Codex model provider base URL.".to_string()
                }
            },
            Input::new(&url_input).disabled(!endpoint_on).w_64(),
            cx,
        ))
        .child(card_row(
            "API Key",
            match profile.kind {
                AgentProfileKind::ClaudeCode => {
                    format!("Exported as {key_env} while the custom endpoint is enabled.")
                }
                AgentProfileKind::Codex => {
                    format!("Exported as {key_env} and referenced by the profile-scoped provider.")
                }
            },
            Input::new(&key_input).disabled(!endpoint_on).w_64(),
            cx,
        ))
        .child(env_section)
}
