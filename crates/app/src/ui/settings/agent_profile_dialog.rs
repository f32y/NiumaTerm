mod env;

#[cfg(test)]
#[path = "agent_profile_dialog_tests.rs"]
mod tests;

use std::borrow::Cow;

use app::agent_tab::AgentKind;
use gpui::{AppContext as _, Context, Entity, IntoElement, Render};
use gpui_component::dialog::Dialog;
use gpui_component::input::InputState;
use rust_i18n::t;

use crate::ui::settings::agent_profile_dialog::env::EnvVarEditor;
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

/// Draft edited in the agent-profile dialog: `target` is the list index in
/// edit mode, `None` while adding. Inputs write here; only Save commits the
/// draft into `AppSettings`, so Cancel is a plain close.
#[derive(Default)]
struct AgentProfileDraft {
    target: Option<usize>,
    profile: AgentProfile,

    env_editor: EnvVarEditor,
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
        env_editor: EnvVarEditor::default(),
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

/// A picker of `options` for one draft field, labelled with the current
/// choice. `is_selected` marks the checked option, and `apply` writes a picked
/// option into the draft.
fn draft_choice<T: Copy + 'static>(
    id: &'static str,
    label: impl Into<SharedString>,
    options: Vec<(T, SharedString)>,
    is_selected: impl Fn(T) -> bool + 'static,
    apply: impl Fn(&mut AgentProfileDraft, T) + Copy + 'static,
    cx: &Context<AgentProfileDraft>,
) -> AnyElement {
    let owner = cx.weak_entity();

    Button::new(id)
        .outline()
        .w_64()
        .label(label)
        .dropdown_caret(true)
        .dropdown_menu(move |menu, _, cx| {
            let Some(owner) = owner.upgrade() else {
                return menu;
            };

            owner.update(cx, |_, cx| {
                options.iter().fold(menu, |menu, (option, label)| {
                    let option = *option;

                    menu.item(
                        PopupMenuItem::new(label.clone())
                            .checked(is_selected(option))
                            .on_click(cx.listener(move |draft, _, _, cx| {
                                apply(draft, option);

                                cx.notify();
                            })),
                    )
                })
            })
        })
        .into_any_element()
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

        draft_choice(
            "agent-profile-dialog-kind",
            kind_label,
            AgentKind::ALL
                .into_iter()
                .map(|kind| (kind, agent_kind_display_label(kind).into()))
                .collect(),
            move |kind| kind == current,
            select_profile_kind,
            cx,
        )
    };

    // An empty stored effort and the literal `default` are the same state;
    // both mean the profile pins nothing.
    let selected_effort = if profile.effort.trim().is_empty() {
        PROFILE_EFFORT_OPTIONS[0].to_string()
    } else {
        profile.effort.trim().to_string()
    };

    let effort_control = draft_choice(
        "agent-profile-dialog-effort",
        effort_label(&selected_effort),
        profile_effort_options(profile.kind)
            .into_iter()
            .map(|option| (option, effort_label(option).into()))
            .collect(),
        move |option| option == selected_effort,
        |draft, option| {
            // `default` is the absence of a choice, so it is stored empty
            // rather than as a level the agent would be asked to honor.
            draft.profile.effort = if option == PROFILE_EFFORT_OPTIONS[0] {
                String::new()
            } else {
                option.to_string()
            };
        },
        cx,
    );

    let cache_warn_minutes = profile.cache_warn_minutes;

    let cache_warn_control = draft_choice(
        "agent-profile-dialog-cache-warn",
        cache_warn_label(cache_warn_minutes),
        CACHE_WARN_OPTIONS
            .into_iter()
            .map(|minutes| (minutes, cache_warn_label(minutes).into()))
            .collect(),
        move |minutes| minutes == cache_warn_minutes,
        |draft, minutes| draft.profile.cache_warn_minutes = minutes,
        cx,
    );

    // DeepSeek Harness is published as a package, so it can run through either
    // package manager; every other harness is launched from a binary the user
    // installed and has nothing to pick between.
    let launcher = profile.launcher;

    let launcher_label = match launcher {
        AgentProfileLauncher::Custom => t!("settings-agent-profile-launcher-custom"),
        AgentProfileLauncher::Npx => t!("settings-agent-profile-launcher-npx"),
        AgentProfileLauncher::PnpmDlx => t!("settings-agent-profile-launcher-pnpm-dlx"),
    };

    let launcher_control = draft_choice(
        "agent-profile-dialog-launcher",
        launcher_label,
        vec![
            (
                AgentProfileLauncher::Npx,
                t!("settings-agent-profile-launcher-npx").into(),
            ),
            (
                AgentProfileLauncher::PnpmDlx,
                t!("settings-agent-profile-launcher-pnpm-dlx").into(),
            ),
            (
                AgentProfileLauncher::Custom,
                t!("settings-agent-profile-launcher-custom").into(),
            ),
        ],
        move |option| option == launcher,
        |draft, option| draft.profile.launcher = option,
        cx,
    );

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

    let env_section = draft.env_editor.render(&profile.env, window, cx);

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
