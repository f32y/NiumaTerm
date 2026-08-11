//! Agent profiles rendered as a three-column list: the agent's own mark, the
//! profile name, and the per-row edit and delete controls.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, Context, Entity, IntoElement, ParentElement as _, Pixels, Styled as _, Window,
    div, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::list::{List, ListDelegate, ListItem, ListState};
use gpui_component::{
    ActiveTheme as _, Icon, IconName, IndexPath, Sizable as _, WindowExt as _, h_flex,
};

use crate::agent_pane::AgentKind;
use crate::agent_pane::usage::{ClaudeIcon, CodexIcon};
use crate::ui::settings::agent_profile_dialog::open_agent_profile_dialog;
use crate::ui::settings::state::{AgentProfile, AppSettings};
use crate::ui::settings::table::{
    TABLE_HEADER_HEIGHT, TABLE_OPERATION_BUTTON, TABLE_ROW_CONTENT_HEIGHT, TABLE_ROW_HEIGHT,
    TrashIcon, table_frame, table_header,
};

/// Column widths shared by the header and the rows, so the two line up
/// without either having to measure the other.
const TYPE_COLUMN: Pixels = px(56.0);
const OPERATION_COLUMN: Pixels = px(56.0);

/// Rows shown before the list starts scrolling instead of growing.
const MAX_VISIBLE_ROWS: f32 = 8.0;

/// The agent's mark, matching the glyph its tabs carry.
fn agent_icon(profile: &AgentProfile) -> Icon {
    match AgentKind::from_profile(profile.kind) {
        AgentKind::Claude => Icon::new(ClaudeIcon),
        AgentKind::Codex => Icon::new(CodexIcon),
    }
    .small()
}

fn profile_label(ix: usize, profile: &AgentProfile) -> String {
    if profile.name.trim().is_empty() {
        format!("Agent Profile {}", ix + 1)
    } else {
        profile.name.clone()
    }
}

fn delete_profile(ix: usize, window: &mut Window, cx: &mut App) {
    let subject = cx
        .global::<AppSettings>()
        .agent_profiles
        .get(ix)
        .map(|profile| profile_label(ix, profile))
        .map(|label| format!("profile \"{label}\""))
        .unwrap_or_else(|| "this profile".to_string());

    window.open_alert_dialog(cx, move |alert, _, _| {
        alert
            .confirm()
            .title("Delete Agent Profile")
            .description(format!("Delete {subject}? This cannot be undone."))
            .on_ok(move |_, _, cx| {
                cx.global_mut::<AppSettings>().remove_agent_profile(ix);
                true
            })
    });
}

/// Row source for the list. It holds its own copy of the profiles rather than
/// reading the global while rendering, so the settings view can compare and
/// refresh it, which is also what marks the list dirty after an add, an edit,
/// or a delete.
pub(super) struct AgentProfileList {
    profiles: Vec<AgentProfile>,
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
        // The frame around the list supplies the last row's bottom edge, so
        // repeating it here would double the line.
        let ruled = row + 1 < self.profiles.len();

        Some(
            ListItem::new(("agent-profile-row", row))
                .when(ruled, |this| {
                    this.border_b_1().border_color(cx.theme().border)
                })
                .child(
                    h_flex()
                        .w_full()
                        .h(TABLE_ROW_CONTENT_HEIGHT)
                        .items_center()
                        .gap_2()
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
                                        .aria_label("Edit")
                                        .tooltip("Edit")
                                        .on_click(move |_, window, cx: &mut App| {
                                            open_agent_profile_dialog(Some(row), window, cx);
                                        }),
                                )
                                .child(
                                    Button::new(("agent-profile-delete", row))
                                        .ghost()
                                        .with_size(TABLE_OPERATION_BUTTON)
                                        .icon(TrashIcon)
                                        .aria_label("Delete")
                                        .tooltip("Delete")
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
                .child(div().w(TYPE_COLUMN).flex_none().child("Type"))
                .child(div().flex_1().min_w_0().child("Name"))
                .child(
                    div()
                        .w(OPERATION_COLUMN)
                        .flex_none()
                        .text_right()
                        .child("Operation"),
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
                },
                window,
                cx,
            )
            .selectable(false)
        });

    state.update(cx, |state, cx| {
        if state.delegate().profiles != profiles {
            state.delegate_mut().profiles = profiles;
            cx.notify();
        }
    });

    // The header plus the profiles, until the list is tall enough to scroll
    // on its own. An empty list still reserves one row for its empty state.
    let height = TABLE_HEADER_HEIGHT + TABLE_ROW_HEIGHT * rows.clamp(1.0, MAX_VISIBLE_ROWS);

    table_frame(cx)
        .h(px(height))
        .child(List::new(&state).scrollbar_visible(rows > MAX_VISIBLE_ROWS))
        .into_any_element()
}
