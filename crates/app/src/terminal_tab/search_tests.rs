use futures::executor::block_on;
use gpui_base::text::search::LiteralSearch;
use nmt_terminal::ghostty::{CellWide, RowCell, ScreenRowMeta, ScreenRowRead, SnapshotStyle};
use nmt_terminal::session::page::PageSource;

use crate::terminal_tab::pane_model::test_session::controller;
use crate::terminal_tab::search::{LogicalLine, scan_source};

fn row(text: &str, wrapped: bool) -> ScreenRowRead {
    ScreenRowRead {
        cells: text
            .chars()
            .enumerate()
            .map(|(index, ch)| RowCell {
                x: index as u16,
                text: ch.to_string().into(),
                wide: CellWide::Narrow,
                style: SnapshotStyle::default(),
            })
            .collect(),
        meta: ScreenRowMeta {
            wrapped,
            ..Default::default()
        },
    }
}

#[test]
fn terminal_search_joins_soft_wraps_and_maps_wide_cells() {
    let source = PageSource::Screen { revision: 1 };

    let mut line = LogicalLine::default();

    line.push(&row("abc", true), 12, 3);

    let mut next = row("de", false);

    next.cells.push(RowCell {
        x: 2,
        text: "中".into(),
        wide: CellWide::Wide,
        style: SnapshotStyle::default(),
    });

    next.cells.push(RowCell {
        x: 3,
        text: "".into(),
        wide: CellWide::SpacerTail,
        style: SnapshotStyle::default(),
    });

    line.push(&next, 13, 4);

    let mut hits = Vec::new();

    line.finish(source, &LiteralSearch::new("cde中"), &mut hits);

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].start, (12, 2));
    assert_eq!(hits[0].end, (13, 4));
    assert_eq!(hits[0].columns(12, 3), Some(2..3));
    assert_eq!(hits[0].columns(13, 4), Some(0..4));
}

#[test]
fn terminal_search_reads_scrollback_and_excludes_control_sequences() {
    let mut output = String::from("\x1b[31mneedle\x1b[0m\r\n");

    for index in 0..35 {
        output.push_str(&format!("line {index}\r\n"));
    }

    let (pane, _) = controller(output.as_bytes(), false);
    let snapshot = pane.source.session.snapshot();

    let source = PageSource::Screen {
        revision: snapshot.revision(),
    };

    let hits = block_on(scan_source(
        &pane.source.session.messenger(),
        source,
        snapshot.scrollbar().total as usize,
        &LiteralSearch::new("NEEDLE"),
    ))
    .unwrap();

    assert_eq!(hits.len(), 1);
    assert!(hits[0].start.0 < snapshot.scrollbar().offset as usize);
    assert_eq!(hits[0].start.1, 0);
    assert_eq!(hits[0].end.1, 6);

    let codes = block_on(scan_source(
        &pane.source.session.messenger(),
        source,
        snapshot.scrollbar().total as usize,
        &LiteralSearch::new("[31m"),
    ))
    .unwrap();

    assert!(codes.is_empty());
}

#[test]
fn terminal_search_rejects_stale_screen_pages() {
    let (pane, _) = controller(b"needle", false);
    let snapshot = pane.source.session.snapshot();

    let source = PageSource::Screen {
        revision: snapshot.revision() + 1,
    };

    assert!(
        block_on(scan_source(
            &pane.source.session.messenger(),
            source,
            snapshot.scrollbar().total as usize,
            &LiteralSearch::new("needle")
        ))
        .is_none()
    );
}
