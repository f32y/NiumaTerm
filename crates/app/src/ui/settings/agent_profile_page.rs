//! Agent profile management, including the profile list, draft dialog, and
//! environment-variable editor.

#[cfg(test)]
#[path = "agent_profile_dialog_tests.rs"]
mod tests;

use std::borrow::Cow;

use app::agent_tab::{AgentKind, AgentKindExt as _};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Div, DragMoveEvent, Entity, Hsla,
    InteractiveElement as _, IntoElement, ParentElement as _, Pixels, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::dialog::Dialog;
use gpui_component::input::InputState;
use gpui_component::list::{List, ListDelegate, ListItem, ListState};
use gpui_component::modern_menu::ModernMenuExt as _;
use gpui_component::{
    ActiveTheme as _, Icon, IconName, IndexPath, Sizable as _, WindowExt as _, h_flex,
};
use rust_i18n::t;

use crate::ui::settings::state::{AgentProfile, AppSettings};
use crate::ui::settings::table::{
    TABLE_HEADER_HEIGHT, TABLE_OPERATION_BUTTON, TABLE_ROW_HEIGHT, TrashIcon, table_frame,
    table_header,
};
use crate::ui::settings::*;

/// Column widths shared by the header and the rows, so the two line up
/// without either having to measure the other.
const TYPE_COLUMN: Pixels = px(56.0);

const OPERATION_COLUMN: Pixels = px(80.0);

/// Thickness of the line marking where a dragged profile would be dropped.
const DROP_LINE_HEIGHT: Pixels = px(2.0);

/// Rows shown before the list starts scrolling instead of growing.
const MAX_VISIBLE_ROWS: f32 = 8.0;

/// Which edge of a row a line is drawn on.
#[derive(Clone, Copy, PartialEq)]
enum RowEdge {
    Top,
    Bottom,
}

/// A line on one edge of a row: the divider to the next row, or the marker for
/// the gap a drop would insert into. It floats over the row rather than sitting
/// in the row's box as a border, because a marker that thickened a border would
/// shrink the space the row centres its contents in and nudge them by a pixel
/// for as long as the drag hovers there.
fn row_line(edge: RowEdge, height: Pixels, color: Hsla) -> Div {
    div()
        .absolute()
        .left_0()
        .right_0()
        .map(|this| match edge {
            RowEdge::Top => this.top_0(),
            RowEdge::Bottom => this.bottom_0(),
        })
        .h(height)
        .bg(color)
}

/// The agent's mark, matching the glyph its tabs carry.
fn agent_icon(profile: &AgentProfile) -> Icon {
    profile.kind.icon().small()
}

fn profile_label(ix: usize, profile: &AgentProfile) -> String {
    if profile.name.trim().is_empty() {
        t!("settings-agent-profile-unnamed", n = (ix + 1)).into_owned()
    } else {
        profile.name.clone()
    }
}

fn delete_profile(ix: usize, window: &mut Window, cx: &mut App) {
    let description = cx
        .global::<AppSettings>()
        .config()
        .agent_profiles
        .list
        .get(ix)
        .map(|profile| profile_label(ix, profile))
        .map(|label| t!("settings-agent-profile-delete-named", name = &label).into_owned())
        .unwrap_or_else(|| t!("settings-agent-profile-delete-current").to_string());

    window.open_alert_dialog(cx, move |alert, _, _| {
        alert
            .confirm()
            .title(t!("settings-agent-profile-delete-title"))
            .description(description.clone())
            .on_ok(move |_, _, cx| {
                cx.global_mut::<AppSettings>().remove_agent_profile(ix);

                true
            })
    });
}

/// Insert a copy of the profile at `ix` directly below it. A duplicate is the
/// starting point for a variant of what it was copied from, and the order of
/// this list is the user's own, so the copy belongs next to its original
/// rather than at the end.
fn duplicate_profile(ix: usize, cx: &mut App) {
    cx.global_mut::<AppSettings>().duplicate_agent_profile(ix);
}

/// Drag payload for reordering rows: the position the drag started from.
struct ProfileDrag {
    from: usize,
}

/// Floating preview under the cursor while a profile row is dragged: the
/// profile's name in a small themed pill.
struct ProfileDragPreview {
    label: SharedString,
}

impl Render for ProfileDragPreview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().border)
            // The preview floats over the list under the cursor, so it needs an
            // opaque fill; the theme's background carries the window
            // translucency, which the Mica materials drive to zero.
            .bg(cx.theme().background.alpha(1.0))
            .text_sm()
            .child(self.label.clone())
    }
}

/// Row source for the list. It holds its own copy of the profiles rather than
/// reading the global while rendering, so the settings view can compare and
/// refresh it, which is also what marks the list dirty after an add, an edit,
/// or a delete.
struct AgentProfileList {
    profiles: Vec<AgentProfile>,

    /// Gap a profile drag currently hovers, counted in row edges: `0` is above
    /// the first row and `profiles.len()` below the last. It is marked with a
    /// line rather than a highlighted row because the row under the pointer
    /// says nothing about which side of it the profile ends up on. The gap the
    /// profile already occupies is marked too, so a drag that would put it back
    /// where it started still shows where the release lands it.
    drop_gap: Option<usize>,
}

impl ListDelegate for AgentProfileList {
    type Item = ListItem;

    fn items_count(&self, _section: usize, _cx: &App) -> usize {
        self.profiles.len()
    }

    fn render_item(
        &mut self,
        ix: IndexPath,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<ListItem> {
        let row = ix.row;
        let profile = self.profiles.get(row)?;
        let label = profile_label(row, profile);
        let drag_label: SharedString = label.clone().into();

        // The frame around the list supplies the last row's bottom edge, so
        // repeating it here would double the line.
        let ruled = row + 1 < self.profiles.len();

        // A gap is marked on the bottom edge of the row above it; the gap
        // before the first row has no such row, so it goes on that row's top
        // edge instead.
        let drop_line = if self.drop_gap == Some(row + 1) {
            Some(RowEdge::Bottom)
        } else if row == 0 && self.drop_gap == Some(0) {
            Some(RowEdge::Top)
        } else {
            None
        };

        Some(
            // Stating the row height keeps the rows totalling what the frame
            // reserves for them; measured rows would each add their content's
            // own height instead.
            ListItem::new(("agent-profile-row", row))
                .h(px(TABLE_ROW_HEIGHT))
                // The row's padding moves onto the content below, which then
                // spans the row exactly, so a line placed on its edge lands on
                // the row's edge.
                .p_0()
                .child(
                    h_flex()
                        .id(("agent-profile-drag", row))
                        .relative()
                        .w_full()
                        .h(px(TABLE_ROW_HEIGHT))
                        .px_3()
                        .items_center()
                        .gap_2()
                        // Drag a row to reorder it; drop moves the dragged
                        // profile (`from`) into the gap the pointer marks.
                        .on_drag(ProfileDrag { from: row }, move |_, _, _, cx| {
                            cx.new(|_| ProfileDragPreview {
                                label: drag_label.clone(),
                            })
                        })
                        .on_drag_move(cx.listener(
                            move |this, e: &DragMoveEvent<ProfileDrag>, _, cx| {
                                if !e.bounds.contains(&e.event.position) {
                                    return;
                                }

                                // The pointer's half of the row picks the
                                // edge it is closest to, which is the gap the
                                // release inserts into.
                                let gap = if e.event.position.y < e.bounds.center().y {
                                    row
                                } else {
                                    row + 1
                                };

                                if this.delegate().drop_gap != Some(gap) {
                                    this.delegate_mut().drop_gap = Some(gap);

                                    cx.notify();
                                }
                            },
                        ))
                        .on_drop(cx.listener(move |this, drag: &ProfileDrag, _, cx| {
                            let gap = this.delegate_mut().drop_gap.take();

                            let from = drag.from;

                            // Removing the profile first shifts every gap below
                            // it up by one, so a gap past the profile's own
                            // position lands one row earlier than it reads.
                            let to = gap.map(|gap| if from < gap { gap - 1 } else { gap });

                            if let Some(to) = to {
                                cx.global_mut::<AppSettings>().move_agent_profile(from, to);
                            }

                            // Refresh the rows directly: the drop lands on
                            // this list, so no outer render is guaranteed to
                            // push the reordered profiles back in.
                            this.delegate_mut().profiles = cx
                                .global::<AppSettings>()
                                .config()
                                .agent_profiles
                                .list
                                .clone();

                            cx.notify();
                        }))
                        // The same operations as the row's own controls, plus
                        // duplication, which has no button: it is reached
                        // rarely enough that a third icon would cost the name
                        // column more width than it is worth.
                        .modern_context_menu(move |menu, _, _| {
                            menu.item(t!("settings-common-edit"), move |window, cx| {
                                open_agent_profile_dialog(Some(row), window, cx);
                            })
                            .icon(IconName::PenLine)
                            .item(t!("settings-common-duplicate"), move |_, cx| {
                                duplicate_profile(row, cx);
                            })
                            .icon(IconName::Copy)
                            .item(t!("settings-common-delete"), move |window, cx| {
                                delete_profile(row, window, cx);
                            })
                            .icon(IconName::Delete)
                        })
                        .when(ruled, |this| {
                            this.child(row_line(RowEdge::Bottom, px(1.0), cx.theme().border))
                        })
                        .when_some(drop_line, |this, edge| {
                            this.child(row_line(edge, DROP_LINE_HEIGHT, cx.theme().primary))
                        })
                        .child(div().w(TYPE_COLUMN).flex_none().child(agent_icon(profile)))
                        .child(div().flex_1().min_w_0().truncate().child(label))
                        .child(
                            h_flex()
                                .w(OPERATION_COLUMN)
                                .flex_none()
                                .gap_1()
                                .justify_end()
                                .child(
                                    Button::new(("agent-profile-edit", row))
                                        .ghost()
                                        .with_size(TABLE_OPERATION_BUTTON)
                                        .icon(IconName::PenLine)
                                        .accessibility_label(t!("settings-common-edit"))
                                        .tooltip(t!("settings-common-edit"))
                                        .on_click(move |_, window, cx: &mut App| {
                                            open_agent_profile_dialog(Some(row), window, cx);
                                        }),
                                )
                                .child(
                                    Button::new(("agent-profile-delete", row))
                                        .ghost()
                                        .with_size(TABLE_OPERATION_BUTTON)
                                        .icon(TrashIcon)
                                        .accessibility_label(t!("settings-common-delete"))
                                        .tooltip(t!("settings-common-delete"))
                                        .on_click(move |_, window, cx: &mut App| {
                                            delete_profile(row, window, cx);
                                        }),
                                ),
                        ),
                ),
        )
    }

    fn render_section_header(
        &mut self,
        _section: usize,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<impl IntoElement> {
        Some(
            table_header(cx)
                .child(
                    div()
                        .w(TYPE_COLUMN)
                        .flex_none()
                        .child(t!("settings-common-type")),
                )
                .child(div().flex_1().min_w_0().child(t!("settings-common-name")))
                .child(
                    div()
                        .w(OPERATION_COLUMN)
                        .flex_none()
                        .text_right()
                        .whitespace_nowrap()
                        .child(t!("settings-common-operation")),
                ),
        )
    }

    fn set_selected_index(
        &mut self,
        _ix: Option<IndexPath>,
        _window: &mut Window,
        _cx: &mut Context<ListState<Self>>,
    ) {
    }
}

/// The list element for the current agent profiles. The state is keyed
/// element state, so it lives as long as the settings surface renders; the
/// profiles are pushed in from here on every render, which keeps the rows
/// current after an add, an edit, or a delete.
pub(super) fn agent_profile_list(window: &mut Window, cx: &mut App) -> AnyElement {
    let profiles = cx
        .global::<AppSettings>()
        .config()
        .agent_profiles
        .list
        .clone();

    let rows = profiles.len() as f32;

    let state: Entity<ListState<AgentProfileList>> =
        window.use_keyed_state("agent-profile-list", cx, |window, cx| {
            ListState::new(
                AgentProfileList {
                    profiles: Vec::new(),
                    drop_gap: None,
                },
                window,
                cx,
            )
            .selectable(false)
        });

    state.update(cx, |state, cx| {
        // Drop the insertion line once the drag is gone without a drop on
        // the list (cancelled via Escape, or released elsewhere) — the cancel
        // itself refreshes the window, so this always gets a chance to run.
        if state.delegate().drop_gap.is_some() && !cx.has_active_drag() {
            state.delegate_mut().drop_gap = None;

            cx.notify();
        }

        if state.delegate().profiles != profiles {
            state.delegate_mut().profiles = profiles;

            cx.notify();
        }
    });

    // The header plus the profiles, until the list is tall enough to scroll
    // on its own. An empty list still reserves one row for its empty state.
    // The height sits on the list rather than the frame, because the frame's
    // border counts against a height set on it and would shrink the list by
    // those two pixels, leaving it scrollable by that much.
    let height = TABLE_HEADER_HEIGHT + TABLE_ROW_HEIGHT * rows.clamp(1.0, MAX_VISIBLE_ROWS);

    table_frame(cx)
        .child(
            List::new(&state)
                .h(px(height))
                .scrollbar_visible(rows > MAX_VISIBLE_ROWS),
        )
        .into_any_element()
}

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
struct EnvVarEditor {
    editing: Option<(usize, EnvField)>,
}

impl EnvVarEditor {
    /// The environment section over `env`: its heading, the table, and the
    /// control that adds a variable.
    fn render(
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
