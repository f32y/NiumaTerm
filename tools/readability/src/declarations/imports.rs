mod alphabetical;

#[cfg(test)]
mod tests;

use syn::spanned::Spanned;
use syn::{Item, ItemUse, UseTree};

use crate::declarations::{Issue, group as declaration_group};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Group {
    Standard,
    ThirdParty,
    Crate,
}

pub(crate) fn group(item: &Item) -> Option<Group> {
    let Item::Use(item) = item else {
        return None;
    };

    classify(&item.tree).ok().flatten()
}

fn classify(tree: &UseTree) -> Result<Option<Group>, ()> {
    let ident = match tree {
        UseTree::Path(path) => &path.ident,
        UseTree::Name(name) => &name.ident,
        UseTree::Rename(rename) => &rename.ident,

        UseTree::Group(group) => {
            let mut common = None;

            for tree in &group.items {
                if let Some(current) = classify(tree)? {
                    if common.is_some_and(|previous| previous != current) {
                        return Err(());
                    }

                    common = Some(current);
                }
            }

            return Ok(common);
        }

        UseTree::Glob(_) => return Ok(Some(Group::Crate)),
    };

    Ok(Some(match ident.to_string().trim_start_matches("r#") {
        "std" | "core" | "alloc" => Group::Standard,
        "crate" | "self" | "super" => Group::Crate,
        _ => Group::ThirdParty,
    }))
}

pub(super) fn inspect(items: &[Item]) -> Vec<Issue> {
    let mut issues = Vec::new();
    let mut previous_visibility = None;
    let mut highest: Option<(Group, &ItemUse)> = None;

    for item in items {
        let visibility = declaration_group(item);

        if visibility != previous_visibility {
            highest = None;
            previous_visibility = visibility;
        }

        let Item::Use(import) = item else {
            continue;
        };

        match classify(&import.tree) {
            Ok(Some(current)) => {
                issues.extend(alphabetical::inspect(&import.tree));

                if let Some((previous, related)) = highest
                    && current < previous
                {
                    issues.push(Issue {
                        span: item.span(),
                        related: related.span(),
                        rule: "import-order",
                        message: "order imports within each visibility group as std -> third-party -> crate",
                    });
                } else if let Some((previous, related)) = highest
                    && current == previous
                    && alphabetical::compare(&import.tree, &related.tree).is_lt()
                {
                    issues.push(Issue {
                        span: item.span(),
                        related: related.span(),
                        rule: "import-alphabetical",
                        message: "sort imports alphabetically by path within each visibility and source group",
                    });
                } else {
                    highest = Some((current, import));
                }
            }

            Ok(None) => {}

            Err(()) => issues.push(Issue {
                span: item.span(),
                related: item.span(),
                rule: "mixed-imports",
                message: "split std, third-party, and crate imports into separate use declarations",
            }),
        }
    }

    for pair in items.windows(2) {
        if let (Some(previous), Some(next)) = (group(&pair[0]), group(&pair[1]))
            && previous != next
            && declaration_group(&pair[0]) == declaration_group(&pair[1])
            && pair[0].span().end().line == pair[1].span().start().line
        {
            issues.push(Issue {
                span: pair[1].span(),
                related: pair[0].span(),
                rule: "group-spacing",
                message: "put different import groups on separate lines with a blank line between them",
            });
        }
    }

    issues
}
