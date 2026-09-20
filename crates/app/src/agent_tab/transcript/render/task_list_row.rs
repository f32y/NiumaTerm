//! The agent's task list as it stood when a task was completed, as a card in
//! the transcript.

use gpui::prelude::*;
use gpui::{AnyElement, Context, div, px};
use gpui_component::modern_menu::ModernMenuExt as _;
use gpui_component::{ActiveTheme as _, IconName, v_flex};
use nmt_agent::progress::TaskList;
use rust_i18n::t;

use crate::agent_tab::transcript::TranscriptView;
use crate::agent_tab::transcript::disclosure_row::{
    AGENT_CARD_BODY_PADDING_Y, AGENT_CARD_PADDING_X, AgentDisclosureRow, agent_card,
};
use crate::agent_tab::transcript::render::menus::copy_entry_menu;
use crate::agent_tab::view::progress_panel::task_row;

/// A snapshot of the task list. The heading carries the tally, the body the
/// checklist drawn the way the progress panel draws it, so the list reads the
/// same above the composer and in the stream.
pub(crate) fn render_task_list_row(
    index: usize,
    tasks: &TaskList,
    cx: &mut Context<TranscriptView>,
) -> AnyElement {
    let tally = tasks
        .tally()
        .map(|(done, total)| format!("{done}/{total}"))
        .unwrap_or_default();

    let header = AgentDisclosureRow::new(("task-list-head", index), t!("agent-progress-tasks"))
        .type_icon(IconName::Map)
        .preview(tally.clone())
        .accessible_label(format!("{} {tally}", t!("agent-progress-tasks")))
        .render(cx);

    let body = v_flex()
        .debug_selector(|| "agent-task-list-body".into())
        .w_full()
        .px(px(AGENT_CARD_PADDING_X))
        .pb(px(AGENT_CARD_BODY_PADDING_Y))
        .gap_2()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .children(
            tasks
                .explanation
                .as_ref()
                .map(|text| div().whitespace_normal().child(text.clone())),
        )
        .children(tasks.items.iter().map(|task| task_row(task, false, cx)));

    v_flex()
        .id(("entry", index))
        .debug_selector(|| "agent-task-list".into())
        .w_full()
        .modern_context_menu(copy_entry_menu(cx.entity().downgrade(), index))
        .child(agent_card().child(header).child(body))
        .into_any_element()
}
