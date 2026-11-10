//! Agent profile management, including the profile list, draft dialog, and
//! environment-variable editor.

#[cfg(test)]
#[path = "agent_profile_dialog_tests.rs"]
mod tests;

use std::borrow::Cow;
use std::iter;

use app::agent_tab::{AgentKind, AgentKindExt as _};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Div, DragMoveEvent, Entity, Hsla,
    InteractiveElement as _, IntoElement, ParentElement as _, PathPromptOptions, Pixels, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::dialog::{DIALOG_BUTTON_MIN_WIDTH, Dialog, DialogClose, DialogFooter};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::label::Label;
use gpui_component::list::{List, ListDelegate, ListItem, ListState};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::modern_menu::ModernMenuExt as _;
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::switch::Switch;
use gpui_component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, IndexPath, Sizable as _, WindowExt as _,
    h_flex, v_flex,
};
use nmt_agent::claude_code::stream_json;
use nmt_agent::codex::app_server;
use rust_i18n::t;

use crate::ui::settings::card::{card_row, card_text_input, description_hint};
use crate::ui::settings::state::{
    AgentProfile, AgentProfileLauncher, AppSettings, DshVersion, EnvVar, agent_kind_display_label,
    builtin_agent_profile,
};
use crate::ui::settings::table::{
    ENV_OPERATION_COLUMN, TABLE_HEADER_HEIGHT, TABLE_OPERATION_BUTTON, TABLE_ROW_HEIGHT, TrashIcon,
    table_frame, table_header, table_row,
};

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
/// the gap a drop would insert into. It floats over the row instead of being
/// drawn in the row's box as a border, because a marker that thickened a border would
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
            .centered(true)
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
/// and not at the end.
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
            // opaque fill; the theme's background includes the window
            // translucency, which the Mica materials drive to zero.
            .bg(cx.theme().background.alpha(1.0))
            .text_sm()
            .child(self.label.clone())
    }
}

/// Row source for the list. It holds its own copy of the profiles instead of
/// reading the global while rendering, so the settings view can compare and
/// refresh it; that refresh also marks the list dirty after an add, an edit,
/// or a delete.
struct AgentProfileList {
    profiles: Vec<AgentProfile>,

    /// Gap a profile drag currently hovers, counted in row edges: `0` is above
    /// the first row and `profiles.len()` below the last. It is marked with a
    /// line instead of a highlighted row because the row under the pointer
    /// does not show which side of it the profile ends up on. The gap the
    /// profile already occupies is marked too, so a drag that would put it back
    /// where it started still shows where the release drops it.
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
                // spans the row exactly, so a line placed on its edge falls on
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
                            // position ends up one row earlier than it reads.
                            let to = gap.map(|gap| if from < gap { gap - 1 } else { gap });

                            if let Some(to) = to {
                                cx.global_mut::<AppSettings>().move_agent_profile(from, to);
                            }

                            // Refresh the rows directly: the drop happens on
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
                        // The row's buttons stop their clicks from reaching
                        // the row, so a quick double press on edit still opens
                        // one dialog, not two.
                        .on_click(move |event: &ClickEvent, window, cx| {
                            if event.click_count() == 2 {
                                open_agent_profile_dialog(Some(row), window, cx);
                            }
                        })
                        // The same operations as the row's own controls, plus
                        // duplication, which has no button: it is reached
                        // rarely enough that a third icon would cost the name
                        // column more width than such a rare action justifies.
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
        // the list (cancelled via Escape, or released elsewhere); the cancel
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
    // The height is set on the list instead of the frame, because the frame's
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
/// empty string, the same value a profile written before this field existed
/// has, so both mean "leave the effort to the agent".
const PROFILE_EFFORT_OPTIONS: [&str; 6] = ["default", "low", "medium", "high", "xhigh", "max"];

/// Codex takes one level above the shared list. It is the only harness whose
/// top mode a profile can pin: Claude Code reaches its own through a slash
/// command mid-conversation, not through a launch setting.
const CODEX_EFFORT_OPTION: &str = "ultra";

/// The levels a profile of this kind can pin, in order.
fn profile_effort_options(kind: AgentKind) -> Vec<&'static str> {
    PROFILE_EFFORT_OPTIONS
        .iter()
        .copied()
        .chain((kind == AgentKind::Codex).then_some(CODEX_EFFORT_OPTION))
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

/// The label an approval or sandbox value shows, in the words the
/// conversation's own pickers use so the two read alike.
fn approval_value_label(value: &str) -> Cow<'static, str> {
    match value {
        "auto" => t!("agent-setting-value-auto"),
        "acceptEdits" => t!("agent-setting-value-accept-edits"),
        "plan" => t!("agent-setting-value-plan"),
        "bypassPermissions" => t!("agent-setting-value-bypass-permissions"),
        "untrusted" => t!("agent-setting-value-untrusted"),
        "on-request" => t!("agent-setting-value-on-request"),
        "never" => t!("agent-setting-value-never"),
        "readOnly" => t!("agent-setting-value-read-only"),
        "workspaceWrite" => t!("agent-setting-value-workspace-write"),
        "dangerFullAccess" => t!("agent-setting-value-full-access"),
        _ => t!("agent-setting-value-default"),
    }
}

/// A dropdown pinning one of the harness's own values into the profile field
/// `field` selects, currently holding `stored`. `default` heads the list and stands for no pin. A stored
/// value outside `options`, left by a kind switch or a hand edit, shows as
/// `default` because the launch ignores it the same way.
fn pinned_value_choice(
    id: &'static str,
    options: Vec<&'static str>,
    stored: &str,
    field: fn(&mut AgentProfile) -> &mut String,
    cx: &Context<AgentProfileDraft>,
) -> AnyElement {
    let stored = stored.trim();

    let selected = options
        .iter()
        .copied()
        .find(|option| *option == stored)
        .unwrap_or(UNPINNED_VALUE);

    draft_choice(
        id,
        approval_value_label(selected),
        iter::once(UNPINNED_VALUE)
            .chain(
                options
                    .into_iter()
                    .filter(|option| *option != UNPINNED_VALUE),
            )
            .map(|option| (option, approval_value_label(option).into()))
            .collect(),
        move |option| option == selected,
        move |draft, option| {
            // Stored empty instead of as `default`, so the harness keeps
            // resolving its own configured value instead of being handed one.
            *field(&mut draft.profile) = if option == UNPINNED_VALUE {
                String::new()
            } else {
                option.to_string()
            };
        },
        cx,
    )
}

/// The picker's word for "no pin", shared by every pinned-value dropdown.
const UNPINNED_VALUE: &str = "default";

/// Idle spans a profile can warn at before the next message rebuilds the
/// provider's prompt cache, in minutes. `0` is off, the same value a profile
/// written before this field existed has.
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
            ..builtin_agent_profile(AgentKind::Claude)
        },
    };

    let draft = cx.new(|_| AgentProfileDraft {
        target,
        profile,
        env_editor: EnvVarEditor::default(),
    });

    window.open_dialog(cx, move |dialog, window, cx| {
        agent_profile_dialog(dialog, &draft, target, window, cx)
    });
}

fn agent_profile_dialog(
    dialog: Dialog,
    draft: &Entity<AgentProfileDraft>,
    target: Option<usize>,
    window: &Window,
    cx: &App,
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
    let profile = &draft.read(cx).profile;

    let missing_prompt_file = profile.kind == AgentKind::Claude
        && profile
            .system_prompt_file
            .as_ref()
            .is_some_and(String::is_empty);

    // Deleting lives in the profile list's own row control, so this
    // dialog stays an editor: everything in it is reversible by cancelling.
    let footer = DialogFooter::new()
        .child(
            div().debug_selector(|| "agent-profile-save".into()).child(
                Button::new("agent-profile-save")
                    .min_w(DIALOG_BUTTON_MIN_WIDTH)
                    .primary()
                    .label(t!("settings-common-save"))
                    .disabled(missing_prompt_file)
                    .on_click(move |_, window, cx: &mut App| {
                        save_agent_profile_draft(&saved_draft, cx);

                        window.close_dialog(cx);
                    }),
            ),
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
        .centered(true)
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
fn select_profile_kind(draft: &mut AgentProfileDraft, profile_kind: AgentKind) {
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

    // Approval and sandbox values are names each harness defines, so a
    // pin made for the previous kind means nothing to the new one.
    draft.profile.approval.clear();
    draft.profile.sandbox.clear();

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

fn select_system_prompt_file(
    _: &mut AgentProfileDraft,
    _: &ClickEvent,
    _: &mut Window,
    cx: &mut Context<AgentProfileDraft>,
) {
    let rx = cx.prompt_for_paths(PathPromptOptions {
        files: true,
        directories: false,
        multiple: false,
        prompt: Some(t!("settings-agent-profile-system-prompt-select-file").into()),
        file_types: Vec::new(),
    });

    cx.spawn(async move |draft, cx| {
        if let Ok(Ok(Some(paths))) = rx.await
            && let Some(path) = paths.first()
        {
            let _ = draft.update(cx, |draft, cx| {
                if draft.profile.kind == AgentKind::Claude
                    && draft.profile.system_prompt_file.is_some()
                {
                    draft.profile.system_prompt_file = Some(path.display().to_string());

                    cx.notify();
                }
            });
        }
    })
    .detach();
}

fn system_prompt_fields(draft: &AgentProfileDraft, cx: &Context<AgentProfileDraft>) -> Div {
    let custom = draft.profile.system_prompt_file.is_some();
    let default_label = t!("settings-agent-profile-system-prompt-default");
    let custom_label = t!("settings-agent-profile-system-prompt-custom");

    let choice = draft_choice(
        "agent-profile-dialog-system-prompt",
        if custom {
            custom_label.clone()
        } else {
            default_label.clone()
        },
        vec![(false, default_label.into()), (true, custom_label.into())],
        move |option| option == custom,
        |draft, custom| {
            if custom != draft.profile.system_prompt_file.is_some() {
                draft.profile.system_prompt_file = custom.then(String::new);
            }
        },
        cx,
    );

    v_flex()
        .child(card_row(
            t!("settings-agent-profile-system-prompt"),
            t!("settings-agent-profile-system-prompt-description"),
            div()
                .debug_selector(|| "agent-profile-system-prompt-choice".into())
                .child(choice),
            cx,
        ))
        .when(custom, |this| {
            let path = draft.profile.system_prompt_file.clone().unwrap_or_default();

            let label: SharedString = if path.is_empty() {
                t!("settings-common-none").into()
            } else {
                path.into()
            };

            this.child(card_row(
                t!("settings-agent-profile-system-prompt-file"),
                "",
                h_flex()
                    .debug_selector(|| "agent-profile-system-prompt-file".into())
                    .w_64()
                    .gap_2()
                    .child(div().flex_1().min_w_0().truncate().child(label))
                    .child(
                        div()
                            .debug_selector(|| "agent-profile-system-prompt-browse".into())
                            .child(
                                Button::new("agent-profile-dialog-system-prompt-browse")
                                    .outline()
                                    .label(t!("settings-common-browse"))
                                    .on_click(cx.listener(select_system_prompt_file)),
                            ),
                    ),
                cx,
            ))
        })
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
        AgentKind::Claude => "ANTHROPIC_API_KEY",
        AgentKind::Codex => "OPENAI_API_KEY",
        AgentKind::DeepSeek => "DEEPSEEK_API_KEY",
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
            // instead of as a level the agent would be asked to honor.
            draft.profile.effort = if option == PROFILE_EFFORT_OPTIONS[0] {
                String::new()
            } else {
                option.to_string()
            };
        },
        cx,
    );

    let claude_permission_control = pinned_value_choice(
        "agent-profile-dialog-permission-mode",
        stream_json::PERMISSION_OPTIONS.to_vec(),
        &profile.approval,
        |profile| &mut profile.approval,
        cx,
    );

    let codex_approval_control = pinned_value_choice(
        "agent-profile-dialog-approval",
        app_server::APPROVAL_OPTIONS.to_vec(),
        &profile.approval,
        |profile| &mut profile.approval,
        cx,
    );

    let codex_sandbox_control = pinned_value_choice(
        "agent-profile-dialog-sandbox",
        app_server::SANDBOX_OPTIONS
            .iter()
            .map(|(value, _)| *value)
            .collect(),
        &profile.sandbox,
        |profile| &mut profile.sandbox,
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

    // A package launcher fetches an exact release; a custom executable runs
    // whatever release is installed, so only the package launchers offer one.
    let dsh_version = profile.dsh_version;

    let dsh_version_label = |version: DshVersion| -> SharedString {
        let release: &str = version.into();

        format!("v{release}").into()
    };

    let dsh_version_control = draft_choice(
        "agent-profile-dialog-dsh-version",
        dsh_version_label(dsh_version),
        DshVersion::ALL
            .into_iter()
            .map(|version| (version, dsh_version_label(version)))
            .collect(),
        move |option| option == dsh_version,
        |draft, option| draft.profile.dsh_version = option,
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
                AgentKind::Claude | AgentKind::Codex => this.child(executable),
                AgentKind::DeepSeek => this
                    .child(card_row(
                        t!("settings-agent-profile-launcher"),
                        t!("settings-agent-profile-launcher-description"),
                        launcher_control,
                        cx,
                    ))
                    .map(|this| match launcher {
                        AgentProfileLauncher::Custom => this.child(executable),
                        AgentProfileLauncher::Npx | AgentProfileLauncher::PnpmDlx => {
                            this.child(card_row(
                                t!("settings-agent-profile-dsh-version"),
                                t!("settings-agent-profile-dsh-version-description"),
                                dsh_version_control,
                                cx,
                            ))
                        }
                    }),
            }
        })
        .child(card_row(
            t!("settings-agent-profile-model"),
            match profile.kind {
                AgentKind::Claude => {
                    t!("settings-agent-profile-model-claude-description")
                }
                AgentKind::Codex => t!("settings-agent-profile-model-codex-description"),
                AgentKind::DeepSeek => {
                    t!("settings-agent-profile-model-deepseek-description")
                }
            },
            Input::new(&model_input).w_64(),
            cx,
        ))
        .map(|this| match profile.kind {
            AgentKind::Claude => this.child(card_row(
                t!("settings-agent-profile-replace-sub-models"),
                t!("settings-agent-profile-replace-sub-models-description"),
                sub_models_switch,
                cx,
            )),
            AgentKind::Codex => this,
            AgentKind::DeepSeek => this.child(card_row(
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
        .map(|this| match profile.kind {
            AgentKind::Claude => this.child(card_row(
                t!("settings-agent-profile-permission-mode"),
                t!("settings-agent-profile-permission-mode-description"),
                claude_permission_control,
                cx,
            )),
            AgentKind::Codex => this
                .child(card_row(
                    t!("settings-agent-profile-approval"),
                    t!("settings-agent-profile-approval-description"),
                    codex_approval_control,
                    cx,
                ))
                .child(card_row(
                    t!("settings-agent-profile-sandbox"),
                    t!("settings-agent-profile-sandbox-description"),
                    codex_sandbox_control,
                    cx,
                )),
            AgentKind::DeepSeek => this,
        })
        .child(card_row(
            t!("settings-agent-profile-custom-endpoint"),
            t!("settings-agent-profile-custom-endpoint-description"),
            endpoint_switch,
            cx,
        ))
        .child(card_row(
            t!("settings-agent-profile-api-url"),
            match profile.kind {
                AgentKind::Claude => {
                    t!("settings-agent-profile-api-url-claude-description")
                }
                AgentKind::Codex => t!("settings-agent-profile-api-url-codex-description"),
                AgentKind::DeepSeek => {
                    t!("settings-agent-profile-api-url-deepseek-description")
                }
            },
            Input::new(&url_input).disabled(!endpoint_on).w_64(),
            cx,
        ))
        .child(card_row(
            t!("settings-agent-profile-api-key"),
            match profile.kind {
                AgentKind::Claude => t!(
                    "settings-agent-profile-api-key-claude-description",
                    key = key_env
                )
                .into_owned(),
                AgentKind::Codex => t!(
                    "settings-agent-profile-api-key-codex-description",
                    key = key_env
                )
                .into_owned(),
                AgentKind::DeepSeek => t!(
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
        .when(profile.kind == AgentKind::Claude, |this| {
            this.child(system_prompt_fields(draft, cx))
        })
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
