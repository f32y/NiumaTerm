use nmt_terminal::search_corpus::*;
use nmt_terminal::terminal::grid::row::Row;
use nmt_terminal::terminal::pos::{Column, Direction, Line, Pos};
use nmt_terminal::terminal::square::{Square, Wide};

/// Build a render-buffer-style grid from `&str` rows. A row that ends in
/// `\u{1}` (sentinel) is marked soft-wrapped into the next.
fn grid(lines: &[&str]) -> (Vec<Row<Square>>, usize, Vec<bool>) {
    let cols = lines
        .iter()
        .map(|l| l.trim_end_matches('\u{1}').chars().count())
        .max()
        .unwrap_or(0);
    let mut rows = Vec::new();
    let mut wrapped = Vec::new();
    for line in lines {
        let soft = line.ends_with('\u{1}');
        let content = line.trim_end_matches('\u{1}');
        let mut row: Row<Square> = Row::new(cols);
        for (x, c) in content.chars().enumerate() {
            row.inner[x].set_c(c);
        }
        rows.push(row);
        wrapped.push(soft);
    }
    (rows, cols, wrapped)
}

#[test]
fn finds_viewport_match() {
    let (rows, cols, wrapped) = grid(&["the needle here"]);
    let corpus = VisibleCorpus::build(&rows, cols, &wrapped);
    let re = compile("needle").unwrap();
    let all = corpus.find_all(&re);
    assert_eq!(all.len(), 1);
    let m = &all[0];
    assert_eq!(*m.start(), Pos::new(Line(0), Column(4)));
    assert_eq!(*m.end(), Pos::new(Line(0), Column(9)));
}

#[test]
fn smart_case_insensitive_until_uppercase() {
    let (rows, cols, wrapped) = grid(&["Foo foo FOO"]);
    let corpus = VisibleCorpus::build(&rows, cols, &wrapped);
    // lowercase pattern → case-insensitive → all three.
    assert_eq!(corpus.find_all(&compile("foo").unwrap()).len(), 3);
    // uppercase present → case-sensitive → only `Foo`.
    let only = corpus.find_all(&compile("Foo").unwrap());
    assert_eq!(only.len(), 1);
    assert_eq!(*only[0].start(), Pos::new(Line(0), Column(0)));
}

#[test]
fn match_spans_soft_wrap() {
    // "hello" split across a soft wrap: "hel" + "lo".
    let (rows, cols, wrapped) = grid(&["hel\u{1}", "lo"]);
    let corpus = VisibleCorpus::build(&rows, cols, &wrapped);
    let m = corpus.find_all(&compile("hello").unwrap());
    assert_eq!(m.len(), 1, "joined across the wrap");
    assert_eq!(*m[0].start(), Pos::new(Line(0), Column(0)));
    assert_eq!(*m[0].end(), Pos::new(Line(1), Column(1)));
}

#[test]
fn no_match_across_hard_newline() {
    // Two hard lines: "foo" / "bar". "foobar" must NOT match across them.
    let (rows, cols, wrapped) = grid(&["foo", "bar"]);
    let corpus = VisibleCorpus::build(&rows, cols, &wrapped);
    assert!(corpus.find_all(&compile("foobar").unwrap()).is_empty());
}

#[test]
fn find_next_prev_wraps() {
    let (rows, cols, wrapped) = grid(&["a x a x a"]);
    let corpus = VisibleCorpus::build(&rows, cols, &wrapped);
    let re = compile("a").unwrap();
    // `a` matches at cols 0, 4, 8. Right from col 1 → next is col 4.
    let next = corpus
        .find(&re, Pos::new(Line(0), Column(1)), Direction::Right)
        .unwrap();
    assert_eq!(*next.start(), Pos::new(Line(0), Column(4)));
    // Left from the same origin lands on the first `a`, then wraps.
    let prev = corpus
        .find(&re, Pos::new(Line(0), Column(1)), Direction::Left)
        .unwrap();
    assert_eq!(*prev.start(), Pos::new(Line(0), Column(0)));
    // Right past the last match wraps to the first.
    let wrapped_match = corpus
        .find(&re, Pos::new(Line(0), Column(9)), Direction::Right)
        .unwrap();
    assert_eq!(*wrapped_match.start(), Pos::new(Line(0), Column(0)));
}

#[test]
fn trailing_blanks_trimmed() {
    // 10-wide rows, content shorter than the row → trailing spaces padded.
    let (rows, cols, wrapped) = grid(&["hi", "  there   "]);
    let corpus = VisibleCorpus::build(&rows, cols, &wrapped);
    // Regex `.` matches each real char but not the blank padding: row 0 has
    // 2, row 1 has "  there" = 7 (leading spaces kept, trailing trimmed).
    let dots = corpus.find_all(&compile_with(".", false).unwrap());
    assert_eq!(dots.len(), 2 + 7, "no matches on trailing blank padding");
    // The last match sits on `e` of `there`, not in the padding.
    assert_eq!(*dots.last().unwrap().end(), Pos::new(Line(1), Column(6)));
}

#[test]
fn literal_mode_dot_matches_only_dots() {
    // Literal mode: `.` is a literal dot, not "any char".
    let (rows, cols, wrapped) = grid(&["a.b c.d"]);
    let corpus = VisibleCorpus::build(&rows, cols, &wrapped);
    let lit = corpus.find_all(&compile_with(".", true).unwrap());
    assert_eq!(lit.len(), 2, "only the two literal dots");
    assert_eq!(*lit[0].start(), Pos::new(Line(0), Column(1)));
    assert_eq!(*lit[1].start(), Pos::new(Line(0), Column(5)));
    // Regex mode would match every char instead.
    let re = corpus.find_all(&compile_with(".", false).unwrap());
    assert_eq!(re.len(), 7, "regex `.` matches all 7 chars");
}

#[test]
fn deep_corpus_finds_wrap_spanning_match_row() {
    // "barbaz" spans the soft wrap (row 1 → row 2). The match begins on row 1.
    let unwrapped = "foo\nbar\nbaz\nqux\n";
    let wrapped = "foo\nbarbaz\nqux\n".to_string();
    let corpus = DeepCorpus::build(wrapped, unwrapped);
    let re = compile_with("barbaz", true).unwrap();
    // From the top, the match's row is 1 (where it starts).
    assert_eq!(corpus.find_row(&re, 0, Direction::Right), Some(1));
}

#[test]
fn deep_corpus_counts_all_matches() {
    // Three grid rows; "aaa" on rows 0 and 2 → 2 buffer-wide matches.
    let unwrapped = "aaa\nbbb\naaa\n";
    let corpus = DeepCorpus::build(unwrapped.to_string(), unwrapped);
    assert_eq!(corpus.count(&compile_with("aaa", true).unwrap()), 2);
    assert_eq!(corpus.count(&compile_with("zzz", true).unwrap()), 0);
}

#[test]
fn deep_corpus_directional_nearest() {
    let unwrapped = "aaa\nbbb\naaa\nbbb\naaa\n";
    let wrapped = unwrapped.to_string(); // no wraps here
    let corpus = DeepCorpus::build(wrapped, unwrapped);
    let re = compile_with("aaa", true).unwrap(); // rows 0, 2, 4
    assert_eq!(corpus.find_row(&re, 1, Direction::Right), Some(2));
    assert_eq!(corpus.find_row(&re, 3, Direction::Left), Some(2));
    // Past the last match, Right wraps to the first.
    assert_eq!(corpus.find_row(&re, 5, Direction::Right), Some(0));
}

#[test]
fn wide_char_columns_preserved() {
    // `中` is a wide char (base at col 0, spacer at col 1); `X` at col 2.
    let mut row: Row<Square> = Row::new(3);
    row.inner[0].set_c('中');
    row.inner[0].set_wide(Wide::Wide);
    row.inner[1].set_wide(Wide::Spacer);
    row.inner[2].set_c('X');
    let rows = vec![row];
    let corpus = VisibleCorpus::build(&rows, 3, &[false]);
    let m = corpus.find_all(&compile("X").unwrap());
    assert_eq!(m.len(), 1);
    // `X` keeps grid column 2 despite the spacer being skipped in the text.
    assert_eq!(*m[0].start(), Pos::new(Line(0), Column(2)));
}
