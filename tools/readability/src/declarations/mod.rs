pub(crate) mod imports;

#[cfg(test)]
mod tests;

use proc_macro2::Span;
use quote::ToTokens;
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::visit::Visit;
use syn::{AttrStyle, Item, ItemMod, Meta, Path, Token, Visibility};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Group {
    PublicUse,
    CrateUse,
    SuperUse,
    PublicMod,
    CrateMod,
    SuperMod,
    PrivateMod,
    TestMod,
    PrivateUse,
}

pub(crate) struct Issue {
    pub(crate) span: Span,
    pub(crate) related: Span,
    pub(crate) rule: &'static str,
    pub(crate) message: &'static str,
}

pub(crate) fn group(item: &Item) -> Option<Group> {
    let (is_use, visibility) = match item {
        Item::Use(item) => (true, &item.vis),

        Item::Mod(item) if item.ident.to_string().contains("test") => {
            return item.content.is_none().then_some(Group::TestMod);
        }

        Item::Mod(item) if item.content.is_some() && matches!(item.vis, Visibility::Inherited) => {
            return None;
        }

        Item::Mod(item) => (false, &item.vis),
        _ => return None,
    };

    Some(match (is_use, visibility) {
        (true, Visibility::Public(_)) => Group::PublicUse,
        (false, Visibility::Public(_)) => Group::PublicMod,

        (true, Visibility::Restricted(visibility))
            if visibility.in_token.is_none() && visibility.path.is_ident("super") =>
        {
            Group::SuperUse
        }

        (false, Visibility::Restricted(visibility))
            if visibility.in_token.is_none() && visibility.path.is_ident("super") =>
        {
            Group::SuperMod
        }

        (true, Visibility::Restricted(_)) => Group::CrateUse,
        (false, Visibility::Restricted(_)) => Group::CrateMod,
        (true, Visibility::Inherited) => Group::PrivateUse,
        (false, Visibility::Inherited) => Group::PrivateMod,
    })
}

pub(crate) fn inspect(items: &[Item]) -> Vec<Issue> {
    let mut issues = inspect_module(items);
    let mut restrictions = Restrictions(&mut issues);

    for item in items {
        restrictions.visit_item(item);
    }

    issues
}

struct Restrictions<'a>(&'a mut Vec<Issue>);

impl<'ast> Visit<'ast> for Restrictions<'_> {
    fn visit_item_mod(&mut self, module: &'ast ItemMod) {
        let test_cfg = has_test_cfg(module);

        for attribute in &module.attrs {
            if matches!(attribute.style, AttrStyle::Outer) {
                if !test_cfg {
                    let start = self.0.len();

                    self.visit_attribute(attribute);

                    for issue in &mut self.0[start..] {
                        issue.related = module_header(module);
                    }
                }
            } else {
                self.visit_attribute(attribute);
            }
        }

        self.visit_visibility(&module.vis);

        if let Some((_, items)) = &module.content {
            for item in items {
                self.visit_item(item);
            }
        }
    }

    fn visit_visibility(&mut self, visibility: &'ast Visibility) {
        let Visibility::Restricted(visibility) = visibility else {
            return;
        };

        let message = if visibility.in_token.is_some() {
            Some("replace pub(in ...) with pub(crate)")
        } else if visibility.path.is_ident("self") {
            Some("remove pub(self) from this declaration")
        } else {
            None
        };

        if let Some(message) = message {
            self.0.push(Issue {
                span: visibility.span(),
                related: visibility.span(),
                rule: "visibility",
                message,
            });
        }
    }

    fn visit_meta(&mut self, meta: &'ast Meta) {
        if meta.path().is_ident("path") {
            self.0.push(Issue {
                span: meta.span(),
                related: meta.span(),
                rule: "path-attribute",
                message: "use the standard module file layout; #[path = ...] is only allowed on modules with #[cfg(test)]",
            });
        } else if let Meta::List(list) = meta
            && list.path.is_ident("cfg_attr")
            && let Ok(arguments) =
                list.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)
        {
            for attribute in arguments.iter().skip(1) {
                self.visit_meta(attribute);
            }
        }
    }
}

fn has_test_cfg(module: &ItemMod) -> bool {
    module.attrs.iter().any(|attribute| {
        matches!(attribute.style, AttrStyle::Outer)
            && attribute.path().is_ident("cfg")
            && attribute
                .parse_args::<Path>()
                .is_ok_and(|path| path.is_ident("test"))
    })
}

fn module_header(module: &ItemMod) -> Span {
    let start = module
        .attrs
        .iter()
        .find(|attribute| matches!(attribute.style, AttrStyle::Outer))
        .map_or_else(
            || match &module.vis {
                Visibility::Inherited => module.mod_token.span,
                visibility => visibility.span(),
            },
            Spanned::span,
        );

    start.join(module.ident.span()).unwrap_or(start)
}

fn inspect_module(items: &[Item]) -> Vec<Issue> {
    let mut issues = imports::inspect(items);
    let mut body = None;
    let mut highest: Option<(Group, Span)> = None;

    for pair in items.windows(2) {
        if let (Some(previous), Some(next)) = (group(&pair[0]), group(&pair[1]))
            && previous != next
            && pair[0].span().end().line == pair[1].span().start().line
        {
            issues.push(Issue {
                span: pair[1].span(),
                related: pair[0].span(),
                rule: "group-spacing",
                message: "put different declaration groups on separate lines with a blank line between them",
            });
        }
    }

    for item in items {
        if let Item::Mod(module) = item
            && let Some((_, nested)) = &module.content
        {
            issues.extend(inspect_module(nested));
        }

        let Some(current) = group(item) else {
            // Body edits must not expose unrelated old header issues. Module
            // declarations include attributes because a bodyless module can
            // become inline without changing its leading attribute.
            body.get_or_insert_with(|| {
                if let Item::Mod(module) = item {
                    module_header(module)
                } else {
                    item.to_token_stream()
                        .into_iter()
                        .next()
                        .map_or(item.span(), |token| token.span())
                }
            });

            continue;
        };

        let span = match item {
            // An inline module's body can change without changing its header.
            // Limit staged order checks to the declaration itself.
            Item::Mod(module) => module_header(module),

            _ => item.span(),
        };

        if let Some(related) = body {
            issues.push(Issue {
                span,
                related,
                rule: "header",
                message: "move module-level mod and use declarations before other items",
            });
        } else if let Some((previous, related)) = highest
            && current < previous
        {
            issues.push(Issue {
                span,
                related,
                rule: "order",
                message: "expected pub use -> pub(crate) use -> pub(super) use -> pub mod -> pub(crate) mod -> pub(super) mod -> mod -> test modules -> use",
            });
        }

        if highest.is_none_or(|(previous, _)| current >= previous) {
            highest = Some((current, span));
        }

        if let Item::Mod(module) = item
            && current == Group::TestMod
            && module.ident.to_string().trim_start_matches("r#") != "test_support"
            && !has_test_cfg(module)
        {
            issues.push(Issue {
                span,
                related: span,
                rule: "test-module-cfg",
                message: "add #[cfg(test)] to bodyless modules whose names contain test",
            });
        }
    }

    issues
}
