#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::error::Error;
use std::iter;

use proc_macro2::{Delimiter, Span, TokenStream, TokenTree};
use quote::ToTokens;
use syn::spanned::Spanned;
use syn::visit::{self, Visit};
use syn::{AttrStyle, Attribute, Expr, Item, Pat, Stmt};

use crate::declarations::{group, imports};

pub(crate) type Issues = BTreeMap<usize, Issue>;

pub(crate) struct Issue {
    pub(crate) rule: &'static str,
    pub(crate) through_line: usize,
    edit: BlankLineEdit,
}

#[derive(Clone, Copy)]
enum BlankLineEdit {
    Insert,
    Remove,
}

#[derive(PartialEq, Eq)]
enum CallKind {
    Function,
    Method,
}

impl Issue {
    pub(crate) fn message(&self) -> &'static str {
        match self.edit {
            BlankLineEdit::Insert => "missing blank line",
            BlankLineEdit::Remove => "unexpected blank line",
        }
    }
}

#[derive(Default)]
pub(crate) struct Options {
    pub(crate) match_arms: bool,
    pub(crate) enum_variants: bool,
}

struct Spacing<'a> {
    lines: Vec<&'a str>,
    issues: Issues,
    block_depth: usize,
    options: &'a Options,
}

impl Spacing<'_> {
    fn module_docs(&mut self, attrs: &[Attribute], items: &[Item]) {
        let mut previous = None;

        for attribute in attrs
            .iter()
            .filter(|attribute| matches!(attribute.style, AttrStyle::Inner(_)))
        {
            let start = attribute.span().start();

            let line_doc = attribute.path().is_ident("doc")
                && self.lines[start.line - 1]
                    .chars()
                    .skip(start.column)
                    .take(3)
                    .eq("//!".chars());

            if line_doc {
                previous = Some(attribute.span());
            } else if let Some(previous) = previous.take() {
                self.separate(previous, attribute.span(), "module-docs");
            }
        }

        if let (Some(previous), Some(next)) = (previous, items.first()) {
            self.separate(previous, next.span(), "module-docs");
        }
    }

    fn separate(&mut self, previous: Span, next: Span, reason: &'static str) {
        let end = previous.end().line;
        let start = next.start().line;

        if start <= end {
            return;
        }

        // A trailing block comment can extend across the insertion position.
        // Leave that boundary alone so its text remains unchanged.
        if self.lines[end - 1]
            .chars()
            .skip(previous.end().column)
            .collect::<String>()
            .contains("/*")
        {
            return;
        }

        // Comments between adjacent statements explain the following step.
        // Insert before the comment, leaving it attached to that statement.
        let gap = &self.lines[end..start - 1];

        if gap.iter().any(|line| line.trim().is_empty()) {
            return;
        }

        self.issues.entry(end).or_insert(Issue {
            rule: reason,
            through_line: start - 1,
            edit: BlankLineEdit::Insert,
        });
    }

    fn compact(&mut self, previous: Span, next: Span, rule: &'static str) {
        let end = previous.end();
        let start = next.start();
        let mut comment_depth = 0;

        // Only the gap is scanned, so literal contents inside either item stay
        // untouched. Block comments can start on the previous item's last line
        // and contain nested comments and blank lines that must be preserved.
        for index in end.line - 1..start.line.saturating_sub(1) {
            let line = self.lines[index];

            if index >= end.line && comment_depth == 0 && line.trim().is_empty() {
                self.issues.insert(
                    index,
                    Issue {
                        rule,
                        through_line: index + 1,
                        edit: BlankLineEdit::Remove,
                    },
                );
            }

            let mut bytes = if index == end.line - 1 {
                let offset = line
                    .char_indices()
                    .nth(end.column)
                    .map_or(line.len(), |(offset, _)| offset);

                &line.as_bytes()[offset..]
            } else {
                line.as_bytes()
            };

            while bytes.len() >= 2 {
                match &bytes[..2] {
                    b"//" if comment_depth == 0 => break,
                    b"/*" => {
                        comment_depth += 1;
                        bytes = &bytes[2..];
                    }
                    b"*/" if comment_depth > 0 => {
                        comment_depth -= 1;
                        bytes = &bytes[2..];
                    }
                    _ => bytes = &bytes[1..],
                }
            }
        }
    }

    fn items(&mut self, items: &[Item]) {
        for pair in items.windows(2) {
            let (a, b) = (&pair[0], &pair[1]);

            if self.block_depth == 0
                && let (Some(a_group), Some(b_group)) = (group(a), group(b))
                && a_group != b_group
            {
                self.separate(a.span(), b.span(), "declaration-groups");

                continue;
            }

            if self.block_depth == 0
                && let (Some(a_group), Some(b_group)) = (imports::group(a), imports::group(b))
                && a_group != b_group
            {
                self.separate(a.span(), b.span(), "import-groups");

                continue;
            }

            let grouped = matches!(
                (a, b),
                (Item::Use(_), Item::Use(_)) | (Item::Mod(_), Item::Mod(_))
            ) || matches!(
                (a, b),
                (Item::Const(_), Item::Const(_)) | (Item::Static(_), Item::Static(_))
            ) && a.span().start().line == a.span().end().line
                && b.span().start().line == b.span().end().line;

            if b.span().start().line <= a.span().end().line {
                continue;
            }

            let comment_between = self.lines
                [a.span().end().line..b.span().start().line.saturating_sub(1)]
                .iter()
                .any(|line| line.trim_start().starts_with("//"));

            if !grouped || comment_between && !matches!((a, b), (Item::Use(_), Item::Use(_))) {
                self.separate(a.span(), b.span(), "items");
            }
        }
    }
}

fn documented(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|a| a.path().is_ident("doc"))
}

fn unwrapped(expr: &Expr) -> &Expr {
    match expr {
        Expr::Try(x) => unwrapped(&x.expr),
        Expr::Await(x) => unwrapped(&x.base),
        Expr::Paren(x) => unwrapped(&x.expr),
        Expr::Group(x) => unwrapped(&x.expr),
        _ => expr,
    }
}

fn flow(expr: &Expr) -> bool {
    matches!(
        unwrapped(expr),
        Expr::If(_)
            | Expr::Match(_)
            | Expr::ForLoop(_)
            | Expr::While(_)
            | Expr::Loop(_)
            | Expr::Block(_)
            | Expr::Unsafe(_)
    )
}

fn assertion(stmt: &Stmt) -> bool {
    let name = match stmt {
        Stmt::Macro(x) => x.mac.path.segments.last().map(|s| s.ident.to_string()),
        Stmt::Expr(expr, _) => match unwrapped(expr) {
            Expr::Macro(x) => x.mac.path.segments.last().map(|s| s.ident.to_string()),
            _ => None,
        },
        _ => None,
    };

    name.is_some_and(|name| name.starts_with("assert") || name.starts_with("debug_assert"))
}

fn multiline(stmt: &Stmt) -> bool {
    stmt.span().end().line > stmt.span().start().line
}

fn stmt_flow(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Expr(expr, _) => flow(expr),

        Stmt::Local(x) => x
            .init
            .as_ref()
            .is_some_and(|init| init.diverge.is_some() || flow(&init.expr)),

        _ => false,
    }
}

fn mutable_binding(pattern: &Pat) -> bool {
    match pattern {
        Pat::Ident(pattern) => {
            pattern.mutability.is_some()
                || pattern
                    .subpat
                    .as_ref()
                    .is_some_and(|(_, pattern)| mutable_binding(pattern))
        }
        Pat::Or(pattern) => pattern.cases.iter().any(mutable_binding),
        Pat::Paren(pattern) => mutable_binding(&pattern.pat),
        Pat::Reference(pattern) => mutable_binding(&pattern.pat),
        Pat::Slice(pattern) => pattern.elems.iter().any(mutable_binding),
        Pat::Struct(pattern) => pattern
            .fields
            .iter()
            .any(|field| mutable_binding(&field.pat)),
        Pat::Tuple(pattern) => pattern.elems.iter().any(mutable_binding),
        Pat::TupleStruct(pattern) => pattern.elems.iter().any(mutable_binding),
        Pat::Type(pattern) => mutable_binding(&pattern.pat),
        _ => false,
    }
}

fn boundary(a: &Stmt, b: &Stmt, last: bool) -> Option<&'static str> {
    if let (Stmt::Item(a), Stmt::Item(b)) = (a, b) {
        if matches!(
            (a, b),
            (Item::Use(_), Item::Use(_)) | (Item::Mod(_), Item::Mod(_))
        ) {
            return None;
        }

        if matches!((a, b), (Item::Const(_), Item::Const(_)))
            && a.span().start().line == a.span().end().line
            && b.span().start().line == b.span().end().line
        {
            return None;
        }
    }

    if matches!(a, Stmt::Item(_)) || matches!(b, Stmt::Item(_)) {
        return Some("local-items");
    }

    if stmt_flow(a) || stmt_flow(b) {
        return Some("control-flow");
    }

    match (assertion(a), assertion(b)) {
        (true, true) => return None,
        (true, false) | (false, true) => return Some("assertions"),
        (false, false) => {}
    }

    if let Stmt::Expr(expr, semi) = b
        && (matches!(
            unwrapped(expr),
            Expr::Return(_) | Expr::Break(_) | Expr::Continue(_)
        ) || last && semi.is_none())
    {
        return Some("result");
    }

    match (a, b) {
        (Stmt::Local(previous), Stmt::Local(next)) => {
            if mutable_binding(&previous.pat) != mutable_binding(&next.pat) {
                return Some("binding-mutability");
            }

            if multiline(a) || multiline(b) {
                return Some("multiline-binding");
            }
        }

        (Stmt::Local(_), _) | (_, Stmt::Local(_)) => return Some("binding-and-action"),
        _ => {}
    }

    if multiline(a) || multiline(b) {
        return Some("multiline-statement");
    }

    if let (Stmt::Expr(previous, _), Stmt::Expr(expr, _)) = (a, b)
        && let Expr::MethodCall(call) = unwrapped(expr)
        && matches!(
            call.method.to_string().as_str(),
            "notify" | "refresh" | "emit"
        )
        && matches!(
            unwrapped(previous),
            Expr::Call(_) | Expr::MethodCall(_) | Expr::Assign(_) | Expr::Binary(_)
        )
    {
        let already_reacting = matches!(unwrapped(previous), Expr::MethodCall(call) if matches!(call.method.to_string().as_str(), "notify" | "refresh" | "emit"));

        if !already_reacting {
            return Some("view-reaction");
        }
    }

    let call_kind = |statement: &Stmt| match statement {
        Stmt::Expr(expr, _) => match unwrapped(expr) {
            Expr::Call(_) => Some(CallKind::Function),
            Expr::MethodCall(_) => Some(CallKind::Method),
            _ => None,
        },
        _ => None,
    };

    if call_kind(a) != call_kind(b) {
        Some("call-and-statement")
    } else {
        None
    }
}

impl<'ast> Visit<'ast> for Spacing<'_> {
    fn visit_item(&mut self, item: &'ast Item) {
        let attrs = match item {
            Item::Const(item) => &item.attrs,
            Item::Enum(item) => &item.attrs,
            Item::ExternCrate(item) => &item.attrs,
            Item::Fn(item) => &item.attrs,
            Item::ForeignMod(item) => &item.attrs,
            Item::Impl(item) => &item.attrs,
            Item::Macro(item) => &item.attrs,
            Item::Mod(item) => &item.attrs,
            Item::Static(item) => &item.attrs,
            Item::Struct(item) => &item.attrs,
            Item::Trait(item) => &item.attrs,
            Item::TraitAlias(item) => &item.attrs,
            Item::Type(item) => &item.attrs,
            Item::Union(item) => &item.attrs,
            Item::Use(item) => &item.attrs,
            _ => &[][..],
        };

        if skips_formatting(attrs) {
            return;
        }

        visit::visit_item(self, item);
    }

    fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
        if !skips_formatting(&item.attrs) {
            visit::visit_impl_item_fn(self, item);
        }
    }

    fn visit_trait_item_fn(&mut self, item: &'ast syn::TraitItemFn) {
        if !skips_formatting(&item.attrs) {
            visit::visit_trait_item_fn(self, item);
        }
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        let name = mac
            .path
            .segments
            .last()
            .map(|s| s.ident.to_string())
            .unwrap_or_default();

        if name == "select" {
            let tokens: Vec<_> = mac.tokens.clone().into_iter().collect();
            let mut previous: Option<Span> = None;
            let mut start = 0;

            for (index, token) in tokens.iter().enumerate() {
                if matches!(token, TokenTree::Punct(p) if p.as_char() == ';') {
                    start = index + 1;
                }

                if let TokenTree::Group(group) = token {
                    let arrow = index >= 2
                        && matches!(&tokens[index - 2], TokenTree::Punct(p) if p.as_char() == '=')
                        && matches!(&tokens[index - 1], TokenTree::Punct(p) if p.as_char() == '>');

                    if arrow && group.delimiter() == Delimiter::Brace {
                        while matches!(tokens.get(start), Some(TokenTree::Punct(p)) if p.as_char() == ',')
                        {
                            start += 1;
                        }

                        if let Some(previous) = previous {
                            self.separate(previous, tokens[start].span(), "async-branches");
                        }

                        if let Ok(block) =
                            syn::parse2::<syn::Block>(iter::once(token.clone()).collect())
                        {
                            self.visit_block(&block);
                        }

                        previous = Some(group.span());
                        start = index + 1;
                    }
                }
            }
        } else if name == "bitflags" {
            for token in mac.tokens.clone() {
                if let TokenTree::Group(group) = token
                    && group.delimiter() == Delimiter::Brace
                {
                    let tokens: Vec<_> = group.stream().into_iter().collect();
                    let mut start = 0;
                    let mut previous: Option<Span> = None;

                    for (index, token) in tokens.iter().enumerate() {
                        if matches!(token, TokenTree::Punct(p) if p.as_char() == ';') {
                            let span = tokens[start]
                                .span()
                                .join(token.span())
                                .unwrap_or(tokens[start].span());

                            if let Some(previous) = previous {
                                let has_comment = span.start().line > previous.end().line
                                    && self.lines[previous.end().line..span.start().line - 1]
                                        .iter()
                                        .any(|line| line.trim_start().starts_with("//"));

                                if has_comment
                                    || previous.start().line != previous.end().line
                                    || span.start().line != span.end().line
                                {
                                    self.separate(previous, span, "flag-constants");
                                }
                            }

                            previous = Some(span);
                            start = index + 1;
                        }
                    }
                }
            }
        }

        visit::visit_macro(self, mac);
    }

    fn visit_file(&mut self, file: &'ast syn::File) {
        self.module_docs(&file.attrs, &file.items);
        self.items(&file.items);
        visit::visit_file(self, file);
    }

    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        if let Some((_, items)) = &item.content {
            self.module_docs(&item.attrs, items);
            self.items(items);
        }

        visit::visit_item_mod(self, item);
    }

    fn visit_item_impl(&mut self, item: &'ast syn::ItemImpl) {
        for pair in item.items.windows(2) {
            self.separate(pair[0].span(), pair[1].span(), "impl-items");
        }

        visit::visit_item_impl(self, item);
    }

    fn visit_item_trait(&mut self, item: &'ast syn::ItemTrait) {
        for pair in item.items.windows(2) {
            self.separate(pair[0].span(), pair[1].span(), "trait-items");
        }

        visit::visit_item_trait(self, item);
    }

    fn visit_item_foreign_mod(&mut self, item: &'ast syn::ItemForeignMod) {
        for pair in item.items.windows(2) {
            self.separate(pair[0].span(), pair[1].span(), "external-items");
        }

        visit::visit_item_foreign_mod(self, item);
    }

    fn visit_block(&mut self, block: &'ast syn::Block) {
        self.block_depth += 1;

        for (index, pair) in block.stmts.windows(2).enumerate() {
            if let Some(reason) = boundary(&pair[0], &pair[1], index + 2 == block.stmts.len()) {
                self.separate(pair[0].span(), pair[1].span(), reason);
            }
        }

        visit::visit_block(self, block);

        self.block_depth -= 1;
    }

    fn visit_expr_match(&mut self, expr: &'ast syn::ExprMatch) {
        for pair in expr.arms.windows(2) {
            let (a, b) = (&pair[0], &pair[1]);

            if self.options.match_arms {
                if a.span().start().line != a.span().end().line
                    || b.span().start().line != b.span().end().line
                {
                    self.separate(a.span(), b.span(), "match-arms");
                }
            } else {
                self.compact(a.span(), b.span(), "match-arm-blank-lines");
            }
        }

        visit::visit_expr_match(self, expr);
    }

    fn visit_fields_named(&mut self, fields: &'ast syn::FieldsNamed) {
        let fields_list: Vec<_> = fields.named.iter().collect();

        for pair in fields_list.windows(2) {
            if documented(&pair[0].attrs) || documented(&pair[1].attrs) {
                self.separate(pair[0].span(), pair[1].span(), "documented-fields");
            }
        }

        visit::visit_fields_named(self, fields);
    }

    fn visit_item_enum(&mut self, item: &'ast syn::ItemEnum) {
        let variants: Vec<_> = item.variants.iter().collect();

        for pair in variants.windows(2) {
            let (a, b) = (pair[0], pair[1]);

            if self.options.enum_variants {
                if documented(&a.attrs)
                    || documented(&b.attrs)
                    || a.attrs
                        .iter()
                        .chain(&b.attrs)
                        .any(|attr| attr.path().is_ident("error"))
                    || a.fields.span().start().line != a.fields.span().end().line
                    || b.fields.span().start().line != b.fields.span().end().line
                {
                    self.separate(a.span(), b.span(), "enum-variants");
                }
            } else {
                self.compact(a.span(), b.span(), "enum-variant-blank-lines");
            }
        }

        visit::visit_item_enum(self, item);
    }
}

fn skips_formatting(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| {
        let path = attr.path();

        path.segments.len() == 2
            && path.segments[0].ident == "rustfmt"
            && path.segments[1].ident == "skip"
    })
}

pub(crate) fn inspect(source: &str, parsed: &syn::File, options: &Options) -> Issues {
    let mut spacing = Spacing {
        lines: source.lines().collect(),
        issues: BTreeMap::new(),
        block_depth: 0,
        options,
    };

    spacing.visit_file(parsed);

    spacing.issues
}

pub(crate) fn apply(source: &str, issues: &Issues) -> Result<String, Box<dyn Error>> {
    let mut modified = String::with_capacity(source.len() + issues.len() * 2);
    let mut newline = "\n";

    for (index, line) in source.split_inclusive('\n').enumerate() {
        match issues.get(&index).map(|issue| issue.edit) {
            Some(BlankLineEdit::Insert) => modified.push_str(newline),
            Some(BlankLineEdit::Remove) => {
                if !line.trim().is_empty() {
                    return Err("cannot remove a nonblank line; file left untouched".into());
                }

                continue;
            }
            None => {}
        }

        modified.push_str(line);
        newline = if line.ends_with("\r\n") { "\r\n" } else { "\n" };
    }

    let before = syn::parse_file(&source.replace("\r\n", "\n"))?;
    let after = syn::parse_file(&modified.replace("\r\n", "\n"))?;

    if !same_tokens(before.to_token_stream(), after.to_token_stream()) {
        return Err("changing blank lines would change Rust tokens; file left untouched".into());
    }

    Ok(modified)
}

fn same_tokens(before: TokenStream, after: TokenStream) -> bool {
    let mut before = before.into_iter();
    let mut after = after.into_iter();

    loop {
        let equal = match (before.next(), after.next()) {
            (None, None) => return true,

            (Some(TokenTree::Group(a)), Some(TokenTree::Group(b))) => {
                a.delimiter() == b.delimiter() && same_tokens(a.stream(), b.stream())
            }

            (Some(TokenTree::Ident(a)), Some(TokenTree::Ident(b))) => a == b,

            (Some(TokenTree::Punct(a)), Some(TokenTree::Punct(b))) => {
                a.as_char() == b.as_char() && a.spacing() == b.spacing()
            }

            (Some(TokenTree::Literal(a)), Some(TokenTree::Literal(b))) => {
                a.to_string() == b.to_string()
            }

            _ => false,
        };

        if !equal {
            return false;
        }
    }
}
