use std::borrow::Cow;

use gpui::{ClickEvent, Context};
use rust_i18n::t;

use crate::ui::settings::agent_profile_dialog::{AgentProfileDraft, draft_text_input};
use crate::ui::settings::*;

/// Which half of an environment-variable row is open for editing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EnvField {
    Name,
    Value,
}

/// The draft's environment variables as an editable Name / Value table. The
/// table shows plain text until a cell is double-clicked, so only one input
/// exists at a time and the rows stay readable; the editor remembers which
/// cell that is.
#[derive(Default)]
pub(super) struct EnvVarEditor {
    editing: Option<(usize, EnvField)>,
}

impl EnvVarEditor {
    /// The environment section over `env`: its heading, the table, and the
    /// control that adds a variable.
    pub(super) fn render(
        &self,
        env: &[EnvVar],
        window: &mut Window,
        cx: &mut Context<AgentProfileDraft>,
    ) -> Div {
        v_flex()
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
            .child(env_var_table(env, self.editing, window, cx))
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
            )
    }
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
                            draft.env_editor.editing = None;

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
                draft.env_editor.editing = Some((row, field));

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
                                    draft.env_editor.editing = None;

                                    cx.notify();
                                })),
                        ),
                ),
        );
    }

    table.into_any_element()
}
