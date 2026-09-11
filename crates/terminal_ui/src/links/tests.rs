use nmt_terminal::session::RowText;

use crate::links;

#[test]
fn url_at_col_finds_and_trims_urls() {
    fn url_at(text: &str, col: usize) -> Option<String> {
        links::url_at_col(text, col).map(|(url, _)| url)
    }

    let text = "see https://example.com/a?q=1 for details";

    // Click anywhere inside the URL (col 10 = inside host).
    assert_eq!(
        url_at(text, 10).as_deref(),
        Some("https://example.com/a?q=1")
    );

    // The char range covers exactly the URL.
    assert_eq!(links::url_at_col(text, 10).unwrap().1, 4..29);

    // Click on surrounding text misses.
    assert_eq!(url_at(text, 1), None);
    assert_eq!(url_at(text, 33), None);

    // Sentence-final punctuation is trimmed; a click on it misses.
    let text = "read https://a.b/c.";

    assert_eq!(url_at(text, 8).as_deref(), Some("https://a.b/c"));
    assert_eq!(url_at(text, 18), None);

    // The closer of a surrounding paren is dropped, literal parens kept.
    let text = "(https://en.wikipedia.org/wiki/Rust_(language))";

    assert_eq!(
        url_at(text, 5).as_deref(),
        Some("https://en.wikipedia.org/wiki/Rust_(language)")
    );

    // A URL-ish token without a known scheme is not opened.
    assert_eq!(url_at("run foo://bar now", 6), None);

    // Scheme alone is not a URL.
    assert_eq!(url_at("https://", 3), None);

    // Out-of-range column is safe.
    assert_eq!(url_at("short", 40), None);
}

#[test]
fn wrapped_url_resolution_returns_row_segments_and_prefers_osc8() {
    let rows = ["https://exam", "ple.com/abc "];
    let resolved = links::resolve_link(4, |delta| {
        let index = usize::try_from(1 + delta).ok()?;
        Some(RowText {
            text: rows.get(index)?.to_string(),
            wrapped: index == 0,
            hyperlinks: Vec::new(),
        })
    })
    .unwrap();
    assert_eq!(resolved.url, "https://example.com/abc");
    assert_eq!(
        resolved.segments,
        [
            links::RowSegment {
                delta: -1,
                col: 0,
                cols: 12
            },
            links::RowSegment {
                delta: 0,
                col: 0,
                cols: 11
            },
        ]
    );
    let resolved = links::resolve_link(3, |delta| {
        (delta == 0).then(|| RowText {
            text: "https://text".into(),
            wrapped: false,
            hyperlinks: vec![(0, 11, "https://target".into())],
        })
    })
    .unwrap();
    assert_eq!(resolved.url, "https://target");
    assert!(
        links::resolve_link(3, |delta| (delta == 0).then(|| RowText {
            text: "https://text".into(),
            wrapped: false,
            hyperlinks: vec![(0, 11, "custom://target".into())],
        }))
        .is_none()
    );
}
