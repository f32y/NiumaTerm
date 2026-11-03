use gpui_component::text::MarkdownNode;
use gpui_component::text::markdown_ast::Node;
use markdown::{ParseOptions, to_mdast};

use crate::agent_tab::transcript::mermaid::parse::{DiagramSource, parse_source};

fn diagrams(source: &str) -> Vec<MarkdownNode> {
    let root = to_mdast(source, &ParseOptions::gfm()).unwrap();

    let mut nodes = Vec::new();

    collect(&root, source, &mut nodes);

    nodes
}

fn collect(node: &Node, source: &str, nodes: &mut Vec<MarkdownNode>) {
    if let Node::Code(code) = node {
        let position = code.position.as_ref().unwrap();
        let raw = &source[position.start.offset..position.end.offset];

        if let Some(node) = parse_source(node, raw, 0) {
            nodes.push(node);
        }
    }

    if let Some(children) = node.children() {
        for child in children {
            collect(child, source, nodes);
        }
    }
}

#[test]
fn streamed_fences_only_create_diagrams_after_the_closing_marker() {
    let source = "Reply: 中文\n\n```mermaid\nflowchart TD\nA[Start] --> B[Done]\n```";

    for end in source
        .char_indices()
        .map(|(offset, _)| offset)
        .chain([source.len()])
    {
        let nodes = diagrams(&source[..end]);

        assert_eq!(
            nodes.len(),
            usize::from(end == source.len()),
            "prefix {end}"
        );
    }

    let nodes = diagrams(source);
    let diagram = nodes[0].data::<DiagramSource>().unwrap();

    assert_eq!(diagram.offset, source.find("```mermaid").unwrap());
    assert_eq!(diagram.code, "flowchart TD\nA[Start] --> B[Done]");
    assert_eq!(nodes[0].as_markdown(), &source[diagram.offset..]);
}

#[test]
fn nested_and_tilde_fences_preserve_code_and_independent_positions() {
    let source = "- item\n\n  ~~~mermaid\n  flowchart LR\n  A --> B\n  ~~~\n\n> ```mermaid\n> sequenceDiagram\n> A->>B: Hello\n> ```\n";
    let nodes = diagrams(source);

    assert_eq!(nodes.len(), 2);
    assert_eq!(
        nodes[0].data::<DiagramSource>().unwrap().code,
        "flowchart LR\nA --> B"
    );
    assert_eq!(
        nodes[1].data::<DiagramSource>().unwrap().code,
        "sequenceDiagram\nA->>B: Hello"
    );
    assert_ne!(
        nodes[0].data::<DiagramSource>().unwrap().offset,
        nodes[1].data::<DiagramSource>().unwrap().offset,
    );
}

#[test]
fn incomplete_or_unrelated_code_keeps_the_default_code_renderer() {
    for source in [
        "```rust\nlet x = 1;\n```",
        "    mermaid\n    flowchart TD\n    A --> B",
        "````mermaid\nflowchart TD\nA --> B\n```",
        "```mermaid\nflowchart TD\nA --> B\n~~~",
        "```mermaid\nflowchart TD\nA --> B\n> ```",
        "```mermaid\nflowchart TD\nA --> B\n``` trailing",
    ] {
        assert!(diagrams(source).is_empty(), "{source}");
    }
}
