use gpui::SharedString;

use crate::text::{
    document::ParsedDocument,
    node::{BlockNode, NodeContext, Paragraph, Span},
};

/// Parse plain text into a document: a single paragraph holding the raw text
/// verbatim, newlines included (the inline layer treats `\n` as a hard line
/// break, same as a Markdown `Break`). Characters that are significant in
/// Markdown (`*`, `#`, backticks) render as-is, which is the point — this
/// format exists for user-authored text that must not be reinterpreted as
/// markup.
pub(crate) fn parse(source: &str, cx: &mut NodeContext) -> Result<ParsedDocument, SharedString> {
    let mut blocks = Vec::new();
    if !source.is_empty() {
        let mut paragraph = Paragraph::new(source.to_string());
        paragraph.set_span(Span {
            start: cx.offset,
            end: cx.offset + source.len(),
        });
        blocks.push(BlockNode::Paragraph(paragraph));
    }
    Ok(ParsedDocument {
        source: source.to_string().into(),
        blocks,
    })
}
