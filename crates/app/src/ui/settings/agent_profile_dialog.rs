use nmt_i18n::i18n;

use crate::agent::AgentKind;
use crate::ui::settings::*;

/// Reasoning-effort choices a profile can pin. `default` is stored as an
/// empty string, which is also what a profile written before this field
/// existed carries, so both mean "leave the effort to the agent".
const PROFILE_EFFORT_OPTIONS: [&str; 6] = ["default", "low", "medium", "high", "xhigh", "max"];

fn effort_label(option: &str) -> &'static str {
    match option {
        "low" => i18n("settings-agent-profile-effort-low"),
        "medium" => i18n("settings-agent-profile-effort-medium"),
        "high" => i18n("settings-agent-profile-effort-high"),
        "xhigh" => i18n("settings-agent-profile-effort-xhigh"),
        "max" => i18n("settings-agent-profile-effort-max"),
        _ => i18n("settings-agent-profile-effort-default"),
    }
}

/// Which half of an environment-variable row is open for editing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EnvField {
    Name,
    Value,
}

/// Draft edited in the agent-profile dialog: `target` is the list index in
/// edit mode, `None` while adding. Inputs write here; only Save commits the
/// draft into `AppSettings`, so Cancel is a plain close.
#[derive(Default)]
struct AgentProfileDraft {
    target: Option<usize>,
    profile: AgentProfile,
    /// Environment-variable cell currently open for editing. The table shows
    /// plain text until a cell is double-clicked, so only one input exists at
    /// a time and the rows stay readable.
    editing_env: Option<(usize, EnvField)>,
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
    cx.set_global(AgentProfileDraft {
        target,
        profile,
        editing_env: None,
    });

    window.open_dialog(cx, move |dialog, window, _| {
        let title = if target.is_some() {
            i18n("settings-agent-profile-edit-title")
        } else {
            i18n("settings-agent-profile-add-title")
        };
        let settings_height = window.viewport_size().height;
        let dialog_height = settings_height * 0.72;
        let dialog_top = (settings_height - dialog_height) * 0.5;

        // Deleting lives in the profile list's own row control, so this
        // dialog stays an editor: everything in it is reversible by cancelling.
        let footer =
            DialogFooter::new()
                .child(DialogClose::new().child(
                    Button::new("agent-profile-cancel").label(i18n("settings-common-cancel")),
                ))
                .child(
                    Button::new("agent-profile-save")
                        .primary()
                        .label(i18n("settings-common-save"))
                        .on_click(|_, window, cx: &mut App| {
                            save_agent_profile_draft(cx);
                            window.close_dialog(cx);
                        }),
                );

        dialog
            .title(title)
            .overlay_closable(false)
            .margin_top(dialog_top)
            .w(px(672.))
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

    // A variable with no name cannot be exported, and an entry the user added
    // but never filled in would otherwise persist as noise in config.toml.
    profile.env.retain(|var| !var.name.trim().is_empty());

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

/// Point the draft at another agent type, as picked in the add dialog.
fn select_profile_kind(profile_kind: AgentProfileKind, cx: &mut App) {
    let draft = cx.global_mut::<AgentProfileDraft>();
    if draft.profile.kind == profile_kind {
        return;
    }

    // The executable follows the kind while it still holds any harness's
    // built-in default; a hand-typed path survives the switch. Comparing
    // against every registered default is what keeps a newly added harness
    // from stranding its own default in the field.
    let executable = draft.profile.executable.trim();
    let follows_default = executable.is_empty()
        || AgentKind::ALL
            .into_iter()
            .any(|other| builtin_agent_profile(other.profile_kind()).executable == executable);
    if follows_default {
        let builtin = builtin_agent_profile(profile_kind);
        draft.profile.executable = builtin.executable;
        // How the harness is launched belongs to the harness, so the choice
        // follows the kind for as long as the executable does.
        draft.profile.via_npx = builtin.via_npx;
    }
    draft.profile.kind = profile_kind;
}

/// One editable cell of the environment-variable table. It shows plain text
/// until double-clicked, then swaps in an input that writes straight into the
/// draft; leaving the field closes the editor, so there is nothing to commit.
fn env_cell(
    row: usize,
    field: EnvField,
    text: &str,
    placeholder: &'static str,
    editing: bool,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let key = match field {
        EnvField::Name => "name",
        EnvField::Value => "value",
    };

    if editing {
        let input = card_text_input(
            format!("agent-profile-dialog-env-{row}-{key}"),
            text.to_string().into(),
            false,
            move |value, cx| {
                if let Some(var) = cx
                    .global_mut::<AgentProfileDraft>()
                    .profile
                    .env
                    .get_mut(row)
                {
                    match field {
                        EnvField::Name => var.name = value,
                        EnvField::Value => var.value = value,
                    }
                }
            },
            window,
            cx,
        );

        // Enter and clicking away end the edit. The value is already in the
        // draft, so closing the editor is all that is left to do. The
        // subscription is held in its own keyed slot, which lives exactly as
        // long as this cell is the one being edited.
        window.use_keyed_state(
            SharedString::from(format!("agent-profile-dialog-env-{row}-{key}-close")),
            cx,
            |_, cx| {
                cx.subscribe(&input, |_, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                        cx.global_mut::<AgentProfileDraft>().editing_env = None;
                    }
                })
            },
        );

        // The cell is rendered because the user just asked to edit it, so the
        // caret belongs here without a second click.
        input.update(cx, |input, cx| input.focus(window, cx));

        return div()
            .flex_1()
            .min_w_0()
            .child(
                Input::new(&input)
                    .xsmall()
                    .appearance(false)
                    .p_0()
                    .text_sm(),
            )
            .into_any_element();
    }

    let empty = text.trim().is_empty();
    let label = if empty {
        placeholder.to_string()
    } else {
        text.to_string()
    };

    div()
        .id(("env-cell", row * 2 + field as usize))
        .flex_1()
        .min_w_0()
        .truncate()
        .text_sm()
        .when(empty, |this| {
            this.text_color(cx.theme().muted_foreground.opacity(0.6))
        })
        .child(label)
        .on_click(move |event, _, cx: &mut App| {
            if event.click_count() == 2 {
                cx.global_mut::<AgentProfileDraft>().editing_env = Some((row, field));
            }
        })
        .into_any_element()
}

/// The environment variables of the draft as a Name / Value / Operation
/// table, matching the agent-profile table on the Profiles page.
fn env_var_table(env: &[EnvVar], window: &mut Window, cx: &mut App) -> AnyElement {
    let editing = cx.global::<AgentProfileDraft>().editing_env;

    let mut table = table_frame(cx).child(
        table_header(cx)
            .child(div().flex_1().min_w_0().child(i18n("settings-common-name")))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(i18n("settings-common-value")),
            )
            .child(
                div()
                    .w(ENV_OPERATION_COLUMN)
                    .flex_none()
                    .text_right()
                    .child(i18n("settings-common-operation")),
            ),
    );

    if env.is_empty() {
        return table
            .child(
                table_row(false, cx)
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(i18n("settings-agent-profile-no-variables")),
            )
            .into_any_element();
    }

    for (row, var) in env.iter().enumerate() {
        let ruled = row + 1 < env.len();

        table = table.child(
            table_row(ruled, cx)
                .child(env_cell(
                    row,
                    EnvField::Name,
                    &var.name,
                    i18n("settings-common-name"),
                    editing == Some((row, EnvField::Name)),
                    window,
                    cx,
                ))
                .child(env_cell(
                    row,
                    EnvField::Value,
                    &var.value,
                    i18n("settings-common-value"),
                    editing == Some((row, EnvField::Value)),
                    window,
                    cx,
                ))
                .child(
                    h_flex()
                        .w(ENV_OPERATION_COLUMN)
                        .flex_none()
                        .justify_end()
                        .child(
                            // Removing a row the user can still cancel out of
                            // by closing the dialog needs no confirmation.
                            Button::new(SharedString::from(format!(
                                "agent-profile-dialog-env-remove-{row}"
                            )))
                            .ghost()
                            .with_size(TABLE_OPERATION_BUTTON)
                            .icon(TrashIcon)
                            .aria_label(i18n("settings-common-delete"))
                            .tooltip(i18n("settings-common-delete"))
                            .on_click(move |_, _, cx: &mut App| {
                                let draft = cx.global_mut::<AgentProfileDraft>();
                                if row < draft.profile.env.len() {
                                    draft.profile.env.remove(row);
                                }
                                // Indices shift under the editor, so the open
                                // cell would follow the wrong variable.
                                draft.editing_env = None;
                            }),
                        ),
                ),
        );
    }

    table.into_any_element()
}

fn agent_profile_dialog_content(window: &mut Window, cx: &mut App) -> Div {
    let profile = cx.global::<AgentProfileDraft>().profile.clone();
    let is_edit = cx.global::<AgentProfileDraft>().target.is_some();

    let kind_label = agent_kind_display_label(profile.kind);
    let key_env = match profile.kind {
        AgentProfileKind::ClaudeCode => "ANTHROPIC_API_KEY",
        AgentProfileKind::Codex => "OPENAI_API_KEY",
        AgentProfileKind::DeepSeek => "DEEPSEEK_API_KEY",
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
        // Reading the registered kinds is what puts a newly added harness in
        // front of the user; a hand-written list here is why one could be
        // selectable everywhere else and still impossible to create.
        let current = profile.kind;
        Button::new("agent-profile-dialog-kind")
            .outline()
            .w_64()
            .label(kind_label)
            .dropdown_caret(true)
            .dropdown_menu(move |menu, _, _| {
                AgentKind::ALL.into_iter().fold(menu, |menu, kind| {
                    let profile_kind = kind.profile_kind();

                    menu.item(
                        PopupMenuItem::new(agent_kind_display_label(profile_kind))
                            .checked(profile_kind == current)
                            .on_click(move |_, _, cx: &mut App| {
                                select_profile_kind(profile_kind, cx)
                            }),
                    )
                })
            })
            .into_any_element()
    };

    // An empty stored effort and the literal `default` are the same state;
    // both mean the profile pins nothing.
    let selected_effort = if profile.effort.trim().is_empty() {
        PROFILE_EFFORT_OPTIONS[0].to_string()
    } else {
        profile.effort.trim().to_string()
    };
    let effort_control = Button::new("agent-profile-dialog-effort")
        .outline()
        .w_64()
        .label(effort_label(&selected_effort))
        .dropdown_caret(true)
        .dropdown_menu(move |menu, _, _| {
            let selected = selected_effort.clone();

            PROFILE_EFFORT_OPTIONS.iter().fold(menu, |menu, option| {
                let option = *option;

                menu.item(
                    PopupMenuItem::new(effort_label(option))
                        .checked(option == selected)
                        .on_click(move |_, _, cx: &mut App| {
                            // `default` is the absence of a choice, so it is
                            // stored empty rather than as a level the agent
                            // would be asked to honor.
                            cx.global_mut::<AgentProfileDraft>().profile.effort =
                                if option == PROFILE_EFFORT_OPTIONS[0] {
                                    String::new()
                                } else {
                                    option.to_string()
                                };
                        }),
                )
            })
        });

    // DeepSeek Harness is published to npm, so a profile can run it straight
    // from its package; every other harness is launched from a binary the user
    // installed and has nothing to pick between.
    let via_npx = profile.kind == AgentProfileKind::DeepSeek && profile.via_npx;
    let launcher_control = Button::new("agent-profile-dialog-launcher")
        .outline()
        .w_64()
        .label(if via_npx {
            i18n("settings-agent-profile-launcher-npx")
        } else {
            i18n("settings-agent-profile-launcher-custom")
        })
        .dropdown_caret(true)
        .dropdown_menu(move |menu, _, _| {
            menu.item(
                PopupMenuItem::new(i18n("settings-agent-profile-launcher-npx"))
                    .checked(via_npx)
                    .on_click(|_, _, cx: &mut App| {
                        cx.global_mut::<AgentProfileDraft>().profile.via_npx = true;
                    }),
            )
            .item(
                PopupMenuItem::new(i18n("settings-agent-profile-launcher-custom"))
                    .checked(!via_npx)
                    .on_click(|_, _, cx: &mut App| {
                        cx.global_mut::<AgentProfileDraft>().profile.via_npx = false;
                    }),
            )
        });

    let sub_models_switch = Switch::new("agent-profile-dialog-sub-models")
        .checked(profile.replace_sub_models)
        .on_click(|checked: &bool, _, cx: &mut App| {
            cx.global_mut::<AgentProfileDraft>()
                .profile
                .replace_sub_models = *checked;
        });

    let endpoint_switch = Switch::new("agent-profile-dialog-endpoint")
        .checked(endpoint_on)
        .on_click(|checked: &bool, _, cx: &mut App| {
            cx.global_mut::<AgentProfileDraft>()
                .profile
                .use_custom_endpoint = *checked;
        });

    let env_section = v_flex()
        .w_full()
        .gap_2()
        .child(Label::new(i18n("settings-agent-profile-environment")).text_sm())
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(i18n("settings-agent-profile-environment-description")),
        )
        .child(env_var_table(&profile.env, window, cx))
        .child(
            h_flex().child(
                Button::new("agent-profile-dialog-env-add")
                    .outline()
                    .label(i18n("settings-agent-profile-add-variable"))
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
            i18n("settings-common-name"),
            i18n("settings-agent-profile-name-description"),
            Input::new(&name_input).w_64(),
            cx,
        ))
        .child(card_row(
            i18n("settings-agent-profile-base-agent"),
            i18n("settings-agent-profile-base-agent-description"),
            kind_control,
            cx,
        ))
        .when(profile.kind == AgentProfileKind::DeepSeek, |this| {
            this.child(card_row(
                i18n("settings-agent-profile-launcher"),
                i18n("settings-agent-profile-launcher-description"),
                launcher_control,
                cx,
            ))
        })
        // The path is what the profile launches, so it is only asked for when
        // the profile launches a path.
        .when(!via_npx, |this| {
            this.child(card_row(
                i18n("settings-agent-profile-executable"),
                i18n("settings-agent-profile-executable-description"),
                Input::new(&exe_input).w_64(),
                cx,
            ))
        })
        .child(card_row(
            i18n("settings-agent-profile-model"),
            match profile.kind {
                AgentProfileKind::ClaudeCode => {
                    i18n("settings-agent-profile-model-claude-description")
                }
                AgentProfileKind::Codex => i18n("settings-agent-profile-model-codex-description"),
                AgentProfileKind::DeepSeek => {
                    i18n("settings-agent-profile-model-deepseek-description")
                }
            },
            Input::new(&model_input).w_64(),
            cx,
        ))
        // Claude Code is the only kind that splits work across model tiers;
        // Codex has no equivalent setting to redirect.
        .when(profile.kind == AgentProfileKind::ClaudeCode, |this| {
            this.child(card_row(
                i18n("settings-agent-profile-replace-sub-models"),
                i18n("settings-agent-profile-replace-sub-models-description"),
                sub_models_switch,
                cx,
            ))
        })
        .child(card_row(
            i18n("settings-agent-profile-effort"),
            i18n("settings-agent-profile-effort-description"),
            effort_control,
            cx,
        ))
        .child(card_row(
            i18n("settings-agent-profile-custom-endpoint"),
            i18n("settings-agent-profile-custom-endpoint-description"),
            endpoint_switch,
            cx,
        ))
        .child(card_row(
            i18n("settings-agent-profile-api-url"),
            match profile.kind {
                AgentProfileKind::ClaudeCode => {
                    i18n("settings-agent-profile-api-url-claude-description")
                }
                AgentProfileKind::Codex => i18n("settings-agent-profile-api-url-codex-description"),
                AgentProfileKind::DeepSeek => {
                    i18n("settings-agent-profile-api-url-deepseek-description")
                }
            },
            Input::new(&url_input).disabled(!endpoint_on).w_64(),
            cx,
        ))
        .child(card_row(
            i18n("settings-agent-profile-api-key"),
            match profile.kind {
                AgentProfileKind::ClaudeCode => {
                    i18n("settings-agent-profile-api-key-claude-description")
                        .replace("{key}", key_env)
                }
                AgentProfileKind::Codex => i18n("settings-agent-profile-api-key-codex-description")
                    .replace("{key}", key_env),
                AgentProfileKind::DeepSeek => {
                    i18n("settings-agent-profile-api-key-deepseek-description")
                        .replace("{key}", key_env)
                }
            },
            Input::new(&key_input).disabled(!endpoint_on).w_64(),
            cx,
        ))
        .child(env_section)
}
