use crate::agent_tab::transcript::measurement::Measurements;
use crate::agent_tab::transcript::rows::{MessageBlock, RowGap, RowSpec, TranscriptRow};

#[test]
fn repeated_block_updates_are_merged_and_resolved_after_rows_move() {
    let mut measurements = Measurements::default();

    let mut rows = (0..4)
        .map(|index| TranscriptRow {
            spec: RowSpec::Entry {
                index: index / 2,
                fingerprint: 0,
            },
            gap: RowGap::Block,
            block: Some(MessageBlock {
                index: index % 2,
                key: (index % 2 * 20, 20, 0, false),
            }),
        })
        .collect::<Vec<_>>();

    measurements.sync_tail(0, &rows);
    measurements.invalidate_block(1, 0);
    measurements.invalidate_block(1, 0);

    rows.insert(
        0,
        TranscriptRow {
            spec: RowSpec::Working { compacting: false },
            gap: RowGap::Step,
            block: None,
        },
    );

    measurements.sync_tail(0, &rows);

    assert_eq!(measurements.take_ranges(), vec![3..4]);
    assert!(measurements.take_ranges().is_empty());

    measurements.invalidate_block(1, 0);
    measurements.invalidate_block(1, 20);

    assert_eq!(measurements.take_ranges(), vec![3..5]);
}
