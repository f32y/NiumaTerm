use crate::terminal_tab::pane_model::viewport::LocalPoint;

pub(crate) fn selection_drag_started(
    origin: LocalPoint,
    position: LocalPoint,
    cell_width: f32,
) -> bool {
    let dx = position.x - origin.x;
    let dy = position.y - origin.y;

    dx * dx + dy * dy >= cell_width * cell_width / 16.0
}
