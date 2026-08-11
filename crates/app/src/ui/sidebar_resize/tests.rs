use crate::ui::right_panel::RESIZE_HANDLE as RIGHT_PANEL_HANDLE;
use crate::ui::sidebar_resize::ResizeDrag;
use crate::ui::workspace_sidebar::RESIZE_HANDLE as WORKSPACE_SIDEBAR_HANDLE;

/// Drag-move events reach every listener registered for a drag type,
/// matched on the type alone with no bounds test. Two resizable columns
/// therefore see each other's gestures, and only the originating handle
/// tells them apart — without it, dragging one column resizes both.
#[test]
fn a_gesture_is_claimed_only_by_the_handle_that_started_it() {
    let left = ResizeDrag {
        handle: "workspace-sidebar-resize",
    };
    let right = ResizeDrag {
        handle: "right-panel-resize",
    };

    assert!(left.is_from("workspace-sidebar-resize"));
    assert!(!left.is_from("right-panel-resize"));

    assert!(right.is_from("right-panel-resize"));
    assert!(!right.is_from("workspace-sidebar-resize"));
}

#[test]
fn the_two_resizable_columns_use_distinct_handles() {
    assert_ne!(
        WORKSPACE_SIDEBAR_HANDLE, RIGHT_PANEL_HANDLE,
        "two columns sharing a handle id would resize together"
    );
}
