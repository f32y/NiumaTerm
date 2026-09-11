use std::ops::Range;

use nmt_terminal::session::RowText;

#[derive(Debug, PartialEq)]
pub(crate) struct RowSegment {
    pub delta: i64,
    pub col: usize,
    pub cols: usize,
}

#[derive(Debug, PartialEq)]
pub(crate) struct ResolvedLink {
    pub url: String,
    pub segments: Vec<RowSegment>,
}

/// Schemes Ctrl+click will open. A gate, not just a matcher: OSC 8 URIs come
/// from whatever program printed them, and an escape sequence must not be
/// able to launch arbitrary protocol handlers.
const URL_SCHEMES: [&str; 4] = ["https://", "http://", "file://", "mailto:"];

fn open_allowed(url: &str) -> bool {
    URL_SCHEMES
        .iter()
        .any(|scheme| url.len() > scheme.len() && url[..scheme.len()].eq_ignore_ascii_case(scheme))
}

/// Characters that can appear inside a URL (RFC 3986 plus `%`). ASCII-only,
/// so byte and char offsets coincide within a matched token.
fn is_url_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || "-._~:/?#[]@!$&'()*+,;=%".contains(c)
}

/// The URL covering char index `col` of `text`, if any, plus its char range
/// in `text`: expand over URL characters around the click, anchor at a known
/// scheme, and trim trailing punctuation that in practice ends the sentence
/// rather than the URL.
fn url_at_col(text: &str, col: usize) -> Option<(String, Range<usize>)> {
    let chars: Vec<char> = text.chars().collect();

    if !chars.get(col).copied().is_some_and(is_url_char) {
        return None;
    }

    let start = (0..col)
        .rev()
        .take_while(|&i| is_url_char(chars[i]))
        .last()
        .unwrap_or(col);

    let end = (col..chars.len())
        .take_while(|&i| is_url_char(chars[i]))
        .last()
        .map_or(col, |i| i + 1);

    let token: String = chars[start..end].iter().collect();

    // The scheme anchors the URL start; anything before it in the token
    // (quotes, parens, "url=") is surrounding text.
    let lower = token.to_ascii_lowercase();

    let scheme_at = URL_SCHEMES
        .iter()
        .filter_map(|scheme| lower.find(scheme))
        .min()?;

    let mut url = token[scheme_at..].trim_end_matches(['.', ',', ';', ':', '!', '?', '\'']);

    // Trailing closers are kept only while balanced, so a URL with literal
    // parens survives but the closer of a surrounding "(...)" is dropped.
    for (open, close) in [('(', ')'), ('[', ']')] {
        while url.ends_with(close) && url.matches(open).count() < url.matches(close).count() {
            url = &url[..url.len() - 1];
        }
    }

    // The click must land inside the URL itself, past any trimmed tail.
    let url_range = start + scheme_at..start + scheme_at + url.len();

    (url_range.contains(&col) && open_allowed(url)).then(|| (url.to_string(), url_range))
}

pub(crate) fn resolve_link(
    col: usize,
    row_at: impl Fn(i64) -> Option<RowText>,
) -> Option<ResolvedLink> {
    let segment = |delta, col, cols| Some(RowSegment { delta, col, cols });
    let pointed = row_at(0)?;

    if let Some((start, end, uri)) = pointed
        .hyperlinks
        .iter()
        .find(|(start, end, _)| (*start as usize..=*end as usize).contains(&col))
    {
        if !open_allowed(uri) {
            return None;
        }

        return Some(ResolvedLink {
            url: uri.clone(),
            segments: segment(0, *start as usize, (*end - *start) as usize + 1)
                .into_iter()
                .collect(),
        });
    }

    // Join cap bounds the engine row reads per hover/click: a wrapped
    // logical line can chain through the whole scrollback (e.g. `cat` of
    // a minified file), and each joined row is a locked engine read. A
    // URL wrapping further than ±8 rows truncates at the cap.
    const JOIN_CAP: i64 = 8;

    let width = pointed.text.chars().count();

    let mut text = pointed.text;
    let mut col = col;
    let mut wrapped_down = pointed.wrapped;

    for delta in 1..=JOIN_CAP {
        if !wrapped_down {
            break;
        }

        let Some(next) = row_at(delta) else { break };

        text.push_str(&next.text);

        wrapped_down = next.wrapped;
    }

    let mut back = 0i64;

    for delta in 1..=JOIN_CAP {
        let Some(prev) = row_at(-delta).filter(|prev| prev.wrapped) else {
            break;
        };

        back = delta;

        col += prev.text.chars().count();

        text.insert_str(0, &prev.text);
    }

    let (url, range) = url_at_col(&text, col)?;

    // Every joined segment is exactly `width` chars (rows are padded to
    // the grid width), so the URL's char range maps directly onto rows.
    let mut rects = Vec::new();

    if let (Some(first_seg), Some(last_seg)) = (
        range.start.checked_div(width),
        (range.end - 1).checked_div(width),
    ) {
        for seg in first_seg..=last_seg {
            let start = range.start.max(seg * width);
            let end = range.end.min((seg + 1) * width);

            rects.extend(segment(seg as i64 - back, start - seg * width, end - start));
        }
    }

    Some(ResolvedLink {
        url,
        segments: rects,
    })
}

#[cfg(test)]
mod tests;
