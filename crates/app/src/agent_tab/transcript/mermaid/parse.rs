#[cfg(test)]
#[path = "parse_tests.rs"]
mod tests;

use gpui_component::text::markdown_ast::Node;
use gpui_component::text::{MarkdownNode, MarkdownParseContext};

pub(super) struct DiagramSource {
    pub(super) offset: usize,
    pub(super) code: String,
}

pub(super) fn parse(node: &Node, cx: &MarkdownParseContext<'_>) -> Option<MarkdownNode> {
    parse_source(node, cx.node_source(node)?, cx.offset())
}

fn parse_source(node: &Node, raw: &str, offset: usize) -> Option<MarkdownNode> {
    let Node::Code(code) = node else {
        return None;
    };

    if code.lang.as_deref()? != "mermaid" {
        return None;
    }

    let opening = raw.lines().next()?;
    let marker = opening.chars().next()?;

    if !matches!(marker, '`' | '~') {
        return None;
    }

    let length = opening.chars().take_while(|ch| *ch == marker).count();
    let closing = raw.lines().last()?.trim();
    // Container prefixes remain in the source slice of nested Markdown blocks.
    let closing = closing.trim_start_matches(['>', ' ', '\t']);

    if length < 3
        || closing.chars().take_while(|ch| *ch == marker).count() < length
        || !closing.trim_matches(marker).trim().is_empty()
        || raw.lines().count() <= code.value.lines().count() + 1
    {
        return None;
    }

    Some(
        MarkdownNode::new(
            "mermaid",
            DiagramSource {
                offset: offset + code.position.as_ref()?.start.offset,
                code: code.value.clone(),
            },
        )
        .text(code.value.clone())
        .markdown(raw.to_owned()),
    )
}
