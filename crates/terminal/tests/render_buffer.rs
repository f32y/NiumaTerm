use nmt_terminal::ghostty::GhosttyTerminal;
use nmt_terminal::render_buffer::*;
use nmt_terminal::terminal::square::Wide;
use nmt_terminal::terminal::style::StyleFlags;
use nmt_terminal::terminal::{Column, Line, Pos};

#[test]
fn populates_text_styles_and_cursor() {
    let mut engine = GhosttyTerminal::new(20, 2, 100).unwrap();
    engine.write_vt(b"\x1b[1mhi\x1b[0m");
    let mut buf = RenderBuffer::new(20, 2);
    buf.update(&engine.snapshot().unwrap());

    assert_eq!(buf.cell(0, 0).c(), 'h');
    assert_eq!(buf.cell(1, 0).c(), 'i');
    assert!(
        buf.style(buf.cell(0, 0).style_id())
            .flags
            .contains(StyleFlags::BOLD)
    );
    assert_eq!(buf.cursor(), Pos::new(Line(0), Column(2)));
}

#[test]
fn wide_char_marks_spacer() {
    let mut engine = GhosttyTerminal::new(8, 1, 100).unwrap();
    engine.write_vt("中A".as_bytes());
    let mut buf = RenderBuffer::new(8, 1);
    buf.update(&engine.snapshot().unwrap());
    assert_eq!(buf.cell(0, 0).wide(), Wide::Wide);
    assert_eq!(buf.cell(1, 0).wide(), Wide::Spacer);
    assert_eq!(buf.cell(2, 0).c(), 'A');
}

#[test]
fn style_table_indexed_by_style_id() {
    let mut engine = GhosttyTerminal::new(8, 1, 100).unwrap();
    engine.write_vt(b"\x1b[1mB\x1b[0m");
    let mut buf = RenderBuffer::new(8, 1);
    buf.update(&engine.snapshot().unwrap());
    let sid = buf.cell(0, 0).style_id();
    // The exposed style_table resolves the same style `style()` does.
    assert_eq!(buf.style_table()[sid as usize], buf.style(sid));
    assert!(
        buf.style_table()[sid as usize]
            .flags
            .contains(StyleFlags::BOLD)
    );
}

/// `content_changed` is the signal that replaces the mirror's
/// `peek_damage_event()` on the render path. Inits `true` (first frame is Full to
/// fill the zeroed grid); `update()` sets it; `take` reads+clears; a `take` with
/// no intervening `update` stays `false` (the UI-only-frame = no PTY damage
/// invariant); coalesced updates report `true` once.
#[test]
fn content_changed_lifecycle() {
    let mut engine = GhosttyTerminal::new(8, 1, 100).unwrap();
    let mut buf = RenderBuffer::new(8, 1);

    // Inits true (matches mirror TermDamageState::new full:true).
    assert!(buf.take_content_changed(), "inits true for the first frame");
    // Consumed → false; a UI-only frame (no update) must NOT report Full.
    assert!(
        !buf.take_content_changed(),
        "take with no update stays false (UI-only frame)"
    );

    // A batch sets it.
    engine.write_vt(b"a");
    buf.update(&engine.snapshot().unwrap());
    assert!(buf.take_content_changed(), "update sets it");
    assert!(!buf.take_content_changed(), "consumed after one take");

    // Coalesced updates between frames report true exactly once.
    engine.write_vt(b"b");
    buf.update(&engine.snapshot().unwrap());
    engine.write_vt(b"c");
    buf.update(&engine.snapshot().unwrap());
    assert!(buf.take_content_changed(), "coalesced updates → true");
    assert!(!buf.take_content_changed(), "→ false after the single take");
}

/// A base codepoint plus a combining mark is preserved as a full
/// grapheme cluster — base in the `Square`, trailing codepoints in `extras`.
#[test]
fn grapheme_cluster_fidelity() {
    let mut engine = GhosttyTerminal::new(8, 1, 100).unwrap();
    // `e` + U+0301 (combining acute accent) → one grapheme cell.
    engine.write_vt("e\u{0301}".as_bytes());
    let mut buf = RenderBuffer::new(8, 1);
    buf.update(&engine.snapshot().unwrap());

    let sq = buf.cell(0, 0);
    assert_eq!(sq.c(), 'e', "base codepoint preserved");
    let id = sq.extras_id().expect("combining mark must allocate extras");
    let extras = buf.extras().get(&id).expect("extras entry present");
    assert!(
        extras.zerowidth.contains(&'\u{0301}'),
        "trailing combining codepoint preserved, got {:?}",
        extras.zerowidth
    );
}

/// The buffer captures the engine's per-row soft-wrap flag so line
/// selection follows it). A soft-wrapped row reports `true`; a hard-ended
/// (newline-terminated) row reports `false`.
#[test]
fn captures_softwrap() {
    let mut engine = GhosttyTerminal::new(8, 3, 100).unwrap();
    // 13 chars on an 8-wide terminal → row 0 fills and soft-wraps into row 1.
    engine.write_vt(b"aaaaaaaaaabbb");
    let mut buf = RenderBuffer::new(8, 3);
    buf.update(&engine.snapshot().unwrap());
    assert!(buf.row_wrapped(0), "row 0 soft-wraps into row 1");
    assert!(!buf.row_wrapped(1), "row 1 is the (hard) end of the line");

    // A hard newline does NOT set the wrap flag.
    let mut engine2 = GhosttyTerminal::new(8, 3, 100).unwrap();
    engine2.write_vt(b"ab\r\ncd");
    let mut buf2 = RenderBuffer::new(8, 3);
    buf2.update(&engine2.snapshot().unwrap());
    assert!(!buf2.row_wrapped(0), "row 0 ends with a hard newline");
}

/// The buffer follows the engine viewport on resize.
#[test]
fn buffer_resize_follows_engine() {
    let mut engine = GhosttyTerminal::new(20, 4, 100).unwrap();
    engine.write_vt(b"hello");
    let mut buf = RenderBuffer::new(20, 4);
    buf.update(&engine.snapshot().unwrap());
    assert_eq!((buf.cols(), buf.rows()), (20, 4));
    assert_eq!(buf.grid().len(), 4);

    engine.resize(10, 2, 8, 16).unwrap();
    buf.update(&engine.snapshot().unwrap());
    assert_eq!((buf.cols(), buf.rows()), (10, 2));
    assert_eq!(buf.grid().len(), 2);
    for row in buf.grid() {
        assert_eq!(row.inner.len(), 10, "no stale trailing columns");
    }
}
