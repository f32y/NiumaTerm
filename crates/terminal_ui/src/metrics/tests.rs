use crate::metrics::CellMetrics;

#[test]
fn content_size_maps_to_terminal_grid() {
    let cell = CellMetrics {
        width_px: 8.0,
        height_px: 18.0,
    };

    assert_eq!(cell.grid_size_for_content(940.0, 600.0), (117, 33));
    assert_eq!(cell.grid_size_for_content(1.0, 1.0), (1, 1));
}
