use crate::ui::right_panel::RESIZE_HANDLE as RIGHT_PANEL_HANDLE;
use crate::ui::workspace_sidebar::RESIZE_HANDLE as WORKSPACE_SIDEBAR_HANDLE;

#[test]
fn the_two_resizable_columns_use_distinct_handles() {
    assert_ne!(
        WORKSPACE_SIDEBAR_HANDLE, RIGHT_PANEL_HANDLE,
        "two columns sharing a handle id would resize together"
    );
}
