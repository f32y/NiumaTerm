#[cfg(test)]
#[path = "agent_profile_dialog_tests.rs"]
mod tests;

use std::borrow::Cow;

use app::agent_tab::AgentKind;
use gpui::{AppContext as _, ClickEvent, Context, Entity, IntoElement, Render};
use gpui_component::dialog::Dialog;
use gpui_component::input::InputState;
use rust_i18n::t;

use crate::ui::settings::*;

/// Reasoning-effort choices a profile can pin. `default` is stored as an
/// empty string, which is also what a profile written before this field
/// existed carries, so both mean "leave the effort to the agent".
const PROFILE_EFFORT_OPTIONS: [&str; 6] = ["default", "low", "medium", "high", "xhigh", "max"];

/// Codex takes one level above the shared list. It is the only harness whose
/// top mode a profile can pin: Claude Code reaches its own through a slash
/// command mid-conversation rather than through a launch setting.
const CODEX_EFFORT_OPTION: &str = "ultra";

/// The levels a profile of this kind can pin, in order.
fn profile_effort_options(kind: AgentProfileKind) -> Vec<&'static str> {
    PROFILE_EFFORT_OPTIONS
        .iter()
        .copied()
        .chain((kind == AgentProfileKind::Codex).then_some(CODEX_EFFORT_OPTION))
        .collect()
}

fn effort_label(option: &str) -> Cow<'static, str> {
    match option {
        "low" => t!("settings-agent-profile-effort-low"),
        "medium" => t!("settings-agent-profile-effort-medium"),
        "high" => t!("settings-agent-profile-effort-high"),
        "xhigh" => t!("settings-agent-profile-effort-xhigh"),
        "max" => t!("settings-agent-profile-effort-max"),
        "ultra" => t!("settings-agent-profile-effort-ultra"),
        _ => t!("settings-agent-profile-effort-default"),
    }
}

/// Idle spans a profile can warn at before the next message rebuilds the
/// provider's prompt cache, in minutes. `0` is off, which is also what a
/// profile written before this field existed carries.
const CACHE_WARN_OPTIONS: [u32; 4] = [0, 5, 30, 60];

fn cache_warn_label(minutes: u32) -> Cow<'static, str> {
    match minutes {
        5 => t!("settings-agent-profile-cache-warn-5min"),
        30 => t!("settings-agent-profile-cache-warn-30min"),
        60 => t!("settings-agent-profile-cache-warn-1hour"),
        _ => t!("settings-agent-profile-cache-warn-off"),
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

impl Render for AgentProfileDraft {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        agent_profile_dialog_content(self, window, cx)
    }
}

/// Open the add/edit dialog for an agent profile. `target` is the index in
/// `AppSettings::agent_profiles` for edit mode, `None` for a new profile.
/// The dialog edits an [`AgentProfileDraft`]; Save commits, Cancel discards.
pub(super) fn open_agent_profile_dialog(target: Option<usize>, window: &mut Window, cx: &mut App) {
    let profile = match target {
        Some(ix) => cx
            .global::<AppSettings>()
            .config()
            .agent_profiles
            .list
            .get(ix)
            .cloned()
            .unwrap_or_default(),
        // A new profile starts from the Claude Code built-in with a blank
        // name; Save fills in a unique placeholder.
        None => AgentProfile {
            name: String::new(),
            ..builtin_agent_profile(AgentProfileKind::Claude)
        },
    };

    let draft = cx.new(|_| AgentProfileDraft {
        target,
        profile,
        editing_env: None,
    });

    window.open_dialog(cx, move |dialog, window, _| {
        agent_profile_dialog(dialog, &draft, target, window)
    });
}

fn agent_profile_dialog(
    dialog: Dialog,
    draft: &Entity<AgentProfileDraft>,
    target: Option<usize>,
    window: &Window,
) -> Dialog {
    let saved_draft = draft.clone();
    let content_draft = draft.clone();

    let title = if target.is_some() {
        t!("settings-agent-profile-edit-title")
    } else {
        t!("settings-agent-profile-add-title")
    };

    let settings_height = window.viewport_size().height;
    let dialog_height = settings_height * 0.72;
    let dialog_top = (settings_height - dialog_height) * 0.5;

    // Deleting lives in the profile list's own row control, so this
    // dialog stays an editor: everything in it is reversible by cancelling.
    let footer = DialogFooter::new()
        .child(
            Button::new("agent-profile-save")
                .min_w(DIALOG_BUTTON_MIN_WIDTH)
                .primary()
                .label(t!("settings-common-save"))
                .on_click(move |_, window, cx: &mut App| {
                    save_agent_profile_draft(&saved_draft, cx);

                    window.close_dialog(cx);
                }),
        )
        .child(
            DialogClose::new().child(
                Button::new("agent-profile-cancel")
                    .min_w(DIALOG_BUTTON_MIN_WIDTH)
                    .label(t!("settings-common-cancel")),
            ),
        );

    dialog
        .title(title)
        .overlay_closable(false)
        .margin_top(dialog_top)
        .w(px(1000.))
        .h(dialog_height)
        .content(move |content, _, _| {
            content.overflow_hidden().child(
                div().flex_1().overflow_hidden().child(
                    v_flex()
                        .size_full()
                        .overflow_y_scrollbar()
                        .child(div().pr_2().child(content_draft.clone())),
                ),
            )
        })
        .footer(footer)
}

/// Commit the dialog draft into `AppSettings`: dedupe the name, then update
/// the edited entry or append a new one.
fn save_agent_profile_draft(draft: &Entity<AgentProfileDraft>, cx: &mut App) {
    let draft = draft.read(cx);
    let target = draft.target;
    let profile = draft.profile.clone();

    cx.global_mut::<AppSettings>()
        .save_agent_profile(target, profile);
}

/// Point the draft at another agent type, as picked in the add dialog.
fn select_profile_kind(draft: &mut AgentProfileDraft, profile_kind: AgentProfileKind) {
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
            .any(|other| builtin_agent_profile(other).executable == executable);

    if follows_default {
        let builtin = builtin_agent_profile(profile_kind);

        draft.profile.executable = builtin.executable;

        // How the harness is launched belongs to the harness, so the choice
        // follows the kind for as long as the executable does.
        draft.profile.launcher = builtin.launcher;
    }

    draft.profile.kind = profile_kind;
}

fn draft_text_input(
    key: String,
    value: SharedString,
    apply: impl Fn(&mut AgentProfileDraft, String) + 'static,
    window: &mut Window,
    cx: &mut Context<AgentProfileDraft>,
) -> Entity<InputState> {
    let draft = cx.weak_entity();

    card_text_input(
        key,
        value,
        false,
        move |value, cx| {
            let _ = draft.update(cx, |draft, cx| {
                apply(draft, value);

                cx.notify();
            });
        },
        window,
        cx,
    )
}

/// One editable cell of the environment-variable table. It shows plain text
/// until double-clicked, then swaps in an input that writes straight into the
/// draft; leaving the field closes the editor, so there is nothing to commit.
fn env_cell(
    row: usize,
    field: EnvField,
    text: &str,
    placeholder: Cow<'static, str>,
    editing: bool,
    window: &mut Window,
    cx: &mut Context<AgentProfileDraft>,
) -> AnyElement {
    let key = match field {
        EnvField::Name => "name",
        EnvField::Value => "value",
    };

    if editing {
        let input = draft_text_input(
            format!("agent-profile-dialog-env-{row}-{key}"),
            text.to_string().into(),
            move |draft, value| {
                if let Some(var) = draft.profile.env.get_mut(row) {
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
        let draft = cx.weak_entity();
        let subscribed_input = input.clone();

        window.use_keyed_state(
            format!("agent-profile-dialog-env-{row}-{key}-close"),
            cx,
            move |_, cx| {
                cx.subscribe(&subscribed_input, move |_, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                        let _ = draft.update(cx, |draft, cx| {
                            draft.editing_env = None;

                            cx.notify();
                        });
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
        .on_click(cx.listener(move |draft, event: &ClickEvent, _, cx| {
            if event.click_count() == 2 {
                draft.editing_env = Some((row, field));

                cx.notify();
            }
        }))
        .into_any_element()
}

/// The environment variables of the draft as a Name / Value / Operation
/// table, matching the agent-profile table on the Profiles page.
fn env_var_table(
    env: &[EnvVar],
    editing: Option<(usize, EnvField)>,
    window: &mut Window,
    cx: &mut Context<AgentProfileDraft>,
) -> AnyElement {
    let mut table = table_frame(cx).child(
        table_header(cx)
            .child(div().flex_1().min_w_0().child(t!("settings-common-name")))
            .child(div().flex_1().min_w_0().child(t!("settings-common-value")))
            .child(
                div()
                    .w(ENV_OPERATION_COLUMN)
                    .flex_none()
                    .text_right()
                    .child(t!("settings-common-operation")),
            ),
    );

    if env.is_empty() {
        return table
            .child(
                table_row(false, cx)
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(t!("settings-agent-profile-no-variables")),
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
                    t!("settings-common-name"),
                    editing == Some((row, EnvField::Name)),
                    window,
                    cx,
                ))
                .child(env_cell(
                    row,
                    EnvField::Value,
                    &var.value,
                    t!("settings-common-value"),
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
                            Button::new(format!("agent-profile-dialog-env-remove-{row}"))
                                .ghost()
                                .with_size(TABLE_OPERATION_BUTTON)
                                .icon(TrashIcon)
                                .accessibility_label(t!("settings-common-delete"))
                                .tooltip(t!("settings-common-delete"))
                                .on_click(cx.listener(move |draft, _, _, cx| {
                                    if row < draft.profile.env.len() {
                                        draft.profile.env.remove(row);
                                    }

                                    // Indices shift under the editor, so the open
                                    // cell would follow the wrong variable.
                                    draft.editing_env = None;

                                    cx.notify();
                                })),
                        ),
                ),
        );
    }

    table.into_any_element()
}

fn agent_profile_dialog_content(
    draft: &AgentProfileDraft,
    window: &mut Window,
    cx: &mut Context<AgentProfileDraft>,
) -> Div {
    let profile = draft.profile.clone();
    let is_edit = draft.target.is_some();

    let kind_label = agent_kind_display_label(profile.kind);

    let key_env = match profile.kind {
        AgentProfileKind::Claude => "ANTHROPIC_API_KEY",
        AgentProfileKind::Codex => "OPENAI_API_KEY",
        AgentProfileKind::DeepSeek => "DEEPSEEK_API_KEY",
    };

    let endpoint_on = profile.use_custom_endpoint;

    let name_input = draft_text_input(
        "agent-profile-dialog-name".to_string(),
        profile.name.clone().into(),
        |draft, value| draft.profile.name = value,
        window,
        cx,
    );

    let exe_input = draft_text_input(
        "agent-profile-dialog-exe".to_string(),
        profile.executable.clone().into(),
        |draft, value| draft.profile.executable = value,
        window,
        cx,
    );

    let model_input = draft_text_input(
        "agent-profile-dialog-model".to_string(),
        profile.model.clone().into(),
        |draft, value| draft.profile.model = value,
        window,
        cx,
    );

    let url_input = draft_text_input(
        "agent-profile-dialog-url".to_string(),
        profile.api_base_url.clone().into(),
        |draft, value| draft.profile.api_base_url = value,
        window,
        cx,
    );

    let key_input = draft_text_input(
        "agent-profile-dialog-key".to_string(),
        profile.api_key.clone().into(),
        |draft, value| draft.profile.api_key = value,
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
        let owner = cx.weak_entity();

        Button::new("agent-profile-dialog-kind")
            .outline()
            .w_64()
            .label(kind_label)
            .dropdown_caret(true)
            .dropdown_menu(move |menu, _, cx| {
                let Some(owner) = owner.upgrade() else {
                    return menu;
                };

                owner.update(cx, |_, cx| {
                    AgentKind::ALL.into_iter().fold(menu, |menu, kind| {
                        let profile_kind = kind;

                        menu.item(
                            PopupMenuItem::new(agent_kind_display_label(profile_kind))
                                .checked(profile_kind == current)
                                .on_click(cx.listener(move |draft, _, _, cx| {
                                    select_profile_kind(draft, profile_kind);

                                    cx.notify();
                                })),
                        )
                    })
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

    let effort_owner = cx.weak_entity();

    let effort_control = Button::new("agent-profile-dialog-effort")
        .outline()
        .w_64()
        .label(effort_label(&selected_effort))
        .dropdown_caret(true)
        .dropdown_menu(move |menu, _, cx| {
            let Some(owner) = effort_owner.upgrade() else {
                return menu;
            };

            owner.update(cx, |_, cx| {
                let selected = selected_effort.clone();

                profile_effort_options(profile.kind)
                    .into_iter()
                    .fold(menu, |menu, option| {
                        menu.item(
                            PopupMenuItem::new(effort_label(option))
                                .checked(option == selected)
                                .on_click(cx.listener(move |draft, _, _, cx| {
                                    // `default` is the absence of a choice, so it
                                    // is stored empty rather than as a level the
                                    // agent would be asked to honor.
                                    draft.profile.effort = if option == PROFILE_EFFORT_OPTIONS[0] {
                                        String::new()
                                    } else {
                                        option.to_string()
                                    };

                                    cx.notify();
                                })),
                        )
                    })
            })
        });

    let cache_warn_minutes = profile.cache_warn_minutes;

    let cache_owner = cx.weak_entity();

    let cache_warn_control = Button::new("agent-profile-dialog-cache-warn")
        .outline()
        .w_64()
        .label(cache_warn_label(cache_warn_minutes))
        .dropdown_caret(true)
        .dropdown_menu(move |menu, _, cx| {
            let Some(owner) = cache_owner.upgrade() else {
                return menu;
            };

            owner.update(cx, |_, cx| {
                CACHE_WARN_OPTIONS.into_iter().fold(menu, |menu, minutes| {
                    menu.item(
                        PopupMenuItem::new(cache_warn_label(minutes))
                            .checked(minutes == cache_warn_minutes)
                            .on_click(cx.listener(move |draft, _, _, cx| {
                                draft.profile.cache_warn_minutes = minutes;

                                cx.notify();
                            })),
                    )
                })
            })
        });

    // DeepSeek Harness is published as a package, so it can run through either
    // package manager; every other harness is launched from a binary the user
    // installed and has nothing to pick between.
    let launcher = profile.launcher;

    let launcher_label = match launcher {
        AgentProfileLauncher::Custom => t!("settings-agent-profile-launcher-custom"),
        AgentProfileLauncher::Npx => t!("settings-agent-profile-launcher-npx"),
        AgentProfileLauncher::PnpmDlx => t!("settings-agent-profile-launcher-pnpm-dlx"),
    };

    let launcher_owner = cx.weak_entity();

    let launcher_control = Button::new("agent-profile-dialog-launcher")
        .outline()
        .w_64()
        .label(launcher_label)
        .dropdown_caret(true)
        .dropdown_menu(move |menu, _, cx| {
            let Some(owner) = launcher_owner.upgrade() else {
                return menu;
            };

            owner.update(cx, |_, cx| {
                [
                    (
                        AgentProfileLauncher::Npx,
                        t!("settings-agent-profile-launcher-npx"),
                    ),
                    (
                        AgentProfileLauncher::PnpmDlx,
                        t!("settings-agent-profile-launcher-pnpm-dlx"),
                    ),
                    (
                        AgentProfileLauncher::Custom,
                        t!("settings-agent-profile-launcher-custom"),
                    ),
                ]
                .into_iter()
                .fold(menu, |menu, (option, label)| {
                    menu.item(
                        PopupMenuItem::new(label)
                            .checked(launcher == option)
                            .on_click(cx.listener(move |draft, _, _, cx| {
                                draft.profile.launcher = option;

                                cx.notify();
                            })),
                    )
                })
            })
        });

    let sub_models_switch = Switch::new("agent-profile-dialog-sub-models")
        .checked(profile.replace_sub_models)
        .on_click(cx.listener(|draft, checked: &bool, _, cx| {
            draft.profile.replace_sub_models = *checked;

            cx.notify();
        }));

    let vision_switch = Switch::new("agent-profile-dialog-vision-model")
        .checked(profile.vision_model)
        .on_click(cx.listener(|draft, checked: &bool, _, cx| {
            draft.profile.vision_model = *checked;

            cx.notify();
        }));

    let endpoint_switch = Switch::new("agent-profile-dialog-endpoint")
        .checked(endpoint_on)
        .on_click(cx.listener(|draft, checked: &bool, _, cx| {
            draft.profile.use_custom_endpoint = *checked;

            cx.notify();
        }));

    let env_section = v_flex()
        .w_full()
        .gap_2()
        .child(
            h_flex()
                .gap_1()
                .items_center()
                .child(Label::new(t!("settings-agent-profile-environment")).text_sm())
                .child(description_hint(
                    "environment",
                    t!("settings-agent-profile-environment-description").into(),
                    cx,
                )),
        )
        .child(env_var_table(&profile.env, draft.editing_env, window, cx))
        .child(
            h_flex().child(
                Button::new("agent-profile-dialog-env-add")
                    .outline()
                    .label(t!("settings-agent-profile-add-variable"))
                    .on_click(cx.listener(|draft, _, _, cx| {
                        draft.profile.env.push(EnvVar::default());

                        cx.notify();
                    })),
            ),
        );

    v_flex()
        .w_full()
        .gap_4()
        .child(card_row(
            t!("settings-common-name"),
            t!("settings-agent-profile-name-description"),
            Input::new(&name_input).w_64(),
            cx,
        ))
        .child(card_row(
            t!("settings-agent-profile-base-agent"),
            t!("settings-agent-profile-base-agent-description"),
            kind_control,
            cx,
        ))
        .map(|this| {
            let executable = card_row(
                t!("settings-agent-profile-executable"),
                t!("settings-agent-profile-executable-description"),
                Input::new(&exe_input).w_64(),
                cx,
            );

            match profile.kind {
                AgentProfileKind::Claude | AgentProfileKind::Codex => this.child(executable),
                AgentProfileKind::DeepSeek => this
                    .child(card_row(
                        t!("settings-agent-profile-launcher"),
                        t!("settings-agent-profile-launcher-description"),
                        launcher_control,
                        cx,
                    ))
                    .when(launcher == AgentProfileLauncher::Custom, |this| {
                        this.child(executable)
                    }),
            }
        })
        .child(card_row(
            t!("settings-agent-profile-model"),
            match profile.kind {
                AgentProfileKind::Claude => {
                    t!("settings-agent-profile-model-claude-description")
                }
                AgentProfileKind::Codex => t!("settings-agent-profile-model-codex-description"),
                AgentProfileKind::DeepSeek => {
                    t!("settings-agent-profile-model-deepseek-description")
                }
            },
            Input::new(&model_input).w_64(),
            cx,
        ))
        .map(|this| match profile.kind {
            AgentProfileKind::Claude => this.child(card_row(
                t!("settings-agent-profile-replace-sub-models"),
                t!("settings-agent-profile-replace-sub-models-description"),
                sub_models_switch,
                cx,
            )),
            AgentProfileKind::Codex => this,
            AgentProfileKind::DeepSeek => this.child(card_row(
                t!("settings-agent-profile-vision-model"),
                t!("settings-agent-profile-vision-model-description"),
                vision_switch,
                cx,
            )),
        })
        .child(card_row(
            t!("settings-agent-profile-effort"),
            t!("settings-agent-profile-effort-description"),
            effort_control,
            cx,
        ))
        .child(card_row(
            t!("settings-agent-profile-custom-endpoint"),
            t!("settings-agent-profile-custom-endpoint-description"),
            endpoint_switch,
            cx,
        ))
        .child(card_row(
            t!("settings-agent-profile-api-url"),
            match profile.kind {
                AgentProfileKind::Claude => {
                    t!("settings-agent-profile-api-url-claude-description")
                }
                AgentProfileKind::Codex => t!("settings-agent-profile-api-url-codex-description"),
                AgentProfileKind::DeepSeek => {
                    t!("settings-agent-profile-api-url-deepseek-description")
                }
            },
            Input::new(&url_input).disabled(!endpoint_on).w_64(),
            cx,
        ))
        .child(card_row(
            t!("settings-agent-profile-api-key"),
            match profile.kind {
                AgentProfileKind::Claude => t!(
                    "settings-agent-profile-api-key-claude-description",
                    key = key_env
                )
                .into_owned(),
                AgentProfileKind::Codex => t!(
                    "settings-agent-profile-api-key-codex-description",
                    key = key_env
                )
                .into_owned(),
                AgentProfileKind::DeepSeek => t!(
                    "settings-agent-profile-api-key-deepseek-description",
                    key = key_env
                )
                .into_owned(),
            },
            Input::new(&key_input).disabled(!endpoint_on).w_64(),
            cx,
        ))
        .child(card_row(
            t!("settings-agent-profile-cache-warn"),
            t!("settings-agent-profile-cache-warn-description"),
            cache_warn_control,
            cx,
        ))
        .child(env_section)
}
