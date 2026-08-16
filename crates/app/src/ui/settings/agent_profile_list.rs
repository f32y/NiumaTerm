//! Agent profiles rendered as a three-column list: the agent's own mark, the
//! profile name, and the per-row edit and delete controls.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, DragMoveEvent, Entity, InteractiveElement as _,
    IntoElement, ParentElement as _, Pixels, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Window, div, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::list::{List, ListDelegate, ListItem, ListState};
use gpui_component::{
    ActiveTheme as _, Icon, IconName, IndexPath, Sizable as _, WindowExt as _, h_flex,
};
use nmt_i18n::i18n;

use crate::agent_pane::AgentKind;
use crate::ui::settings::agent_profile_dialog::open_agent_profile_dialog;
use crate::ui::settings::state::{AgentProfile, AppSettings};
use crate::ui::settings::table::{
    TABLE_HEADER_HEIGHT, TABLE_OPERATION_BUTTON, TABLE_ROW_HEIGHT, TrashIcon, table_frame,
    table_header,
};

/// Column widths shared by the header and the rows, so the two line up
/// without either having to measure the other.
const TYPE_COLUMN: Pixels = px(56.0);
const OPERATION_COLUMN: Pixels = px(56.0);

/// Rows shown before the list starts scrolling instead of growing.
const MAX_VISIBLE_ROWS: f32 = 8.0;

/// The agent's mark, matching the glyph its tabs carry.
fn agent_icon(profile: &AgentProfile) -> Icon {
    AgentKind::from_profile(profile.kind).icon().small()
}

fn profile_label(ix: usize, profile: &AgentProfile) -> String {
    if profile.name.trim().is_empty() {
        i18n("settings-agent-profile-unnamed").replace("{n}", &(ix + 1).to_string())
    } else {
        profile.name.clone()
    }
}

fn delete_profile(ix: usize, window: &mut Window, cx: &mut App) {
    let description = cx
        .global::<AppSettings>()
        .agent_profiles
        .get(ix)
        .map(|profile| profile_label(ix, profile))
        .map(|label| i18n("settings-agent-profile-delete-named").replace("{name}", &label))
        .unwrap_or_else(|| i18n("settings-agent-profile-delete-current").to_string());

    window.open_alert_dialog(cx, move |alert, _, _| {
        alert
            .confirm()
            .title(i18n("settings-agent-profile-delete-title"))
            .description(description.clone())
            .on_ok(move |_, _, cx| {
                cx.global_mut::<AppSettings>().remove_agent_profile(ix);
                true
            })
    });
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
            .bg(cx.theme().background)
            .text_sm()
            .child(self.label.clone())
    }
}

/// Row source for the list. It holds its own copy of the profiles rather than
/// reading the global while rendering, so the settings view can compare and
/// refresh it, which is also what marks the list dirty after an add, an edit,
/// or a delete.
pub(super) struct AgentProfileList {
    profiles: Vec<AgentProfile>,
    /// Row a profile drag currently hovers, highlighted as the drop target.
    /// Dropping there moves the dragged profile to that position.
    drag_over: Option<usize>,
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
        let drag_target = self.drag_over == Some(row);

        Some(
            // Stating the row height keeps the divider inside it: without one,
            // the row measures its content plus the border, and the rows would
            // then total more than the frame reserves for them.
            ListItem::new(("agent-profile-row", row))
                .h(px(TABLE_ROW_HEIGHT))
                .when(ruled, |this| {
                    this.border_b_1().border_color(cx.theme().border)
                })
                .when(drag_target, |this| this.bg(cx.theme().list_active))
                .child(
                    h_flex()
                        .id(("agent-profile-drag", row))
                        .w_full()
                        .items_center()
                        .gap_2()
                        // Drag a row to reorder it; drop moves the dragged
                        // profile (`from`) to this row's position.
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

                                // No highlight over the drag's own row:
                                // dropping there is a no-op.
                                let target = (e.drag(cx).from != row).then_some(row);

                                if this.delegate().drag_over != target {
                                    this.delegate_mut().drag_over = target;
                                    cx.notify();
                                }
                            },
                        ))
                        .on_drop(cx.listener(move |this, drag: &ProfileDrag, _, cx| {
                            this.delegate_mut().drag_over = None;

                            let from = drag.from;
                            let profiles = &mut cx.global_mut::<AppSettings>().agent_profiles;
                            // Stale indices only appear if the list changed
                            // mid-drag; skip the move rather than panic.
                            if from != row && from < profiles.len() && row < profiles.len() {
                                let profile = profiles.remove(from);
                                profiles.insert(row, profile);
                            }

                            // Refresh the rows directly: the drop lands on
                            // this list, so no outer render is guaranteed to
                            // push the reordered profiles back in.
                            this.delegate_mut().profiles =
                                cx.global::<AppSettings>().agent_profiles.clone();
                            cx.notify();
                        }))
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
                                        .aria_label(i18n("settings-common-edit"))
                                        .tooltip(i18n("settings-common-edit"))
                                        .on_click(move |_, window, cx: &mut App| {
                                            open_agent_profile_dialog(Some(row), window, cx);
                                        }),
                                )
                                .child(
                                    Button::new(("agent-profile-delete", row))
                                        .ghost()
                                        .with_size(TABLE_OPERATION_BUTTON)
                                        .icon(TrashIcon)
                                        .aria_label(i18n("settings-common-delete"))
                                        .tooltip(i18n("settings-common-delete"))
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
                        .child(i18n("settings-common-type")),
                )
                .child(div().flex_1().min_w_0().child(i18n("settings-common-name")))
                .child(
                    div()
                        .w(OPERATION_COLUMN)
                        .flex_none()
                        .text_right()
                        .child(i18n("settings-common-operation")),
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
    let profiles = cx.global::<AppSettings>().agent_profiles.clone();
    let rows = profiles.len() as f32;

    let state: Entity<ListState<AgentProfileList>> =
        window.use_keyed_state("agent-profile-list", cx, |window, cx| {
            ListState::new(
                AgentProfileList {
                    profiles: Vec::new(),
                    drag_over: None,
                },
                window,
                cx,
            )
            .selectable(false)
        });

    state.update(cx, |state, cx| {
        // Drop the target highlight once the drag is gone without a drop on
        // the list (cancelled via Escape, or released elsewhere) — the cancel
        // itself refreshes the window, so this always gets a chance to run.
        if state.delegate().drag_over.is_some() && !cx.has_active_drag() {
            state.delegate_mut().drag_over = None;
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
