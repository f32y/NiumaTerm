use crate::text::node::*;

#[test]
fn code_block_equality_includes_code_content() {
    let theme = HighlightTheme::default_light();
    let first = CodeBlock::new(
        "let value = 1;".into(),
        Some("rust".into()),
        &theme,
        None::<Span>,
    );
    let second = CodeBlock::new(
        "let value = 2;".into(),
        Some("rust".into()),
        &theme,
        None::<Span>,
    );

    assert_ne!(first, second);
}

#[test]
fn prose_measure_targets_reading_blocks_and_leaves_technical_blocks_full_width() {
    let style = TextViewStyle::default().prose_max_width(rems(48.));
    let paragraph = BlockNode::Paragraph(Paragraph::new("paragraph".to_string()));
    let heading = BlockNode::Heading {
        level: 2,
        children: Paragraph::new("heading".to_string()),
        span: None,
    };
    let quote = BlockNode::Blockquote {
        children: vec![],
        span: None,
    };
    let list = BlockNode::List {
        children: vec![],
        ordered: false,
        span: None,
    };
    let code = BlockNode::CodeBlock(CodeBlock::new(
        "wide technical output".into(),
        Some("text".into()),
        &HighlightTheme::default_light(),
        None::<Span>,
    ));
    let table = BlockNode::Table(Table {
        children: vec![],
        column_aligns: vec![],
        span: None,
    });
    let list_with_code = BlockNode::List {
        children: vec![BlockNode::ListItem {
            children: vec![code.clone()],
            spread: false,
            checked: None,
            span: None,
        }],
        ordered: false,
        span: None,
    };

    for node in [&paragraph, &heading, &quote, &list] {
        assert_eq!(node.prose_max_width(&style), Some(rems(48.)));
    }
    for node in [&code, &table, &list_with_code] {
        assert_eq!(node.prose_max_width(&style), None);
    }

    // max-width remains optional rather than a fixed width, allowing a
    // prose block to shrink with a narrower parent container.
    assert_eq!(paragraph.prose_max_width(&TextViewStyle::default()), None);
}

#[cfg(feature = "tree-sitter")]
#[test]
fn code_block_highlighter_cache_refreshes_after_language_registration() {
    let lang = SharedString::from("json-cache-test");
    let theme = HighlightTheme::default_light();

    CODE_BLOCK_HIGHLIGHTERS.with(|cache| {
        cache.borrow_mut().remove(&lang);
    });

    let unknown_block = CodeBlock::new(
        "{\"value\": 1}".into(),
        Some(lang.clone()),
        &theme,
        None::<Span>,
    );
    _ = unknown_block.styles();

    let cached_language = CODE_BLOCK_HIGHLIGHTERS.with(|cache| {
        cache
            .borrow()
            .get(&lang)
            .map(|highlighter| highlighter.language().clone())
    });
    assert_eq!(cached_language.as_deref(), Some("text"));

    LanguageRegistry::singleton().register(
        lang.as_ref(),
        &crate::highlighter::LanguageConfig::new(
            lang.clone(),
            tree_sitter_json::LANGUAGE.into(),
            vec![],
            r#"
                (string) @string
                (number) @number
                (pair key: (string) @property)
            "#,
            "",
            "",
        ),
    );

    let registered_block = CodeBlock::new(
        "{\"value\": 2}".into(),
        Some(lang.clone()),
        &theme,
        None::<Span>,
    );
    _ = registered_block.styles();

    let cached_language = CODE_BLOCK_HIGHLIGHTERS.with(|cache| {
        cache
            .borrow()
            .get(&lang)
            .map(|highlighter| highlighter.language().clone())
    });
    assert_eq!(cached_language.as_deref(), Some(lang.as_ref()));
}
