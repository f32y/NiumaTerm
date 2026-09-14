use std::cmp::Ordering;

use proc_macro2::Ident;
use syn::UseTree;
use syn::spanned::Spanned;

use crate::declarations::Issue;

pub(super) fn inspect(tree: &UseTree) -> Vec<Issue> {
    let mut issues = Vec::new();

    inspect_nested(tree, &mut issues);

    issues
}

fn inspect_nested(tree: &UseTree, issues: &mut Vec<Issue>) {
    match tree {
        UseTree::Path(path) => inspect_nested(&path.tree, issues),

        UseTree::Group(group) => {
            let mut highest = None;

            for item in &group.items {
                if let Some(previous) = highest
                    && compare(item, previous).is_lt()
                {
                    issues.push(Issue {
                        span: item.span(),
                        related: previous.span(),
                        rule: "import-alphabetical",
                        message: "sort entries in each braced import list alphabetically by path",
                    });
                } else {
                    highest = Some(item);
                }

                inspect_nested(item, issues);
            }
        }

        _ => {}
    }
}

pub(super) fn compare(left: &UseTree, right: &UseTree) -> Ordering {
    let (left, right) = (ungroup(left), ungroup(right));

    match (head(left), head(right)) {
        (Some((left_name, left_tail)), Some((right_name, right_tail))) => {
            let left_name = left_name.to_string();
            let right_name = right_name.to_string();
            let left_name = left_name.trim_start_matches("r#");
            let right_name = right_name.trim_start_matches("r#");

            keyword_rank(left_name)
                .cmp(&keyword_rank(right_name))
                .then_with(|| compare_names(left_name, right_name))
                .then_with(|| match (left_tail, right_tail) {
                    (Some(left), Some(right)) => compare(left, right),
                    (None, Some(_)) => Ordering::Less,
                    (Some(_), None) => Ordering::Greater,
                    (None, None) => Ordering::Equal,
                })
        }

        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,

        (None, None) => match (left, right) {
            (UseTree::Group(left), UseTree::Group(right)) => left
                .items
                .iter()
                .zip(&right.items)
                .map(|(left, right)| compare(left, right))
                .find(|order| !order.is_eq())
                .unwrap_or_else(|| left.items.len().cmp(&right.items.len())),

            (UseTree::Glob(_), UseTree::Group(_)) => Ordering::Less,
            (UseTree::Group(_), UseTree::Glob(_)) => Ordering::Greater,
            _ => Ordering::Equal,
        },
    }
}

fn ungroup(tree: &UseTree) -> &UseTree {
    if let UseTree::Group(group) = tree
        && group.items.len() == 1
    {
        ungroup(&group.items[0])
    } else {
        tree
    }
}

fn head(tree: &UseTree) -> Option<(&Ident, Option<&UseTree>)> {
    match tree {
        UseTree::Path(path) => {
            let tail = ungroup(&path.tree);

            // A trailing self imports the parent path, including when renamed.
            let tail = match tail {
                UseTree::Name(name) if name.ident == "self" => None,
                UseTree::Rename(rename) if rename.ident == "self" => None,
                _ => Some(tail),
            };

            Some((&path.ident, tail))
        }

        UseTree::Name(name) => Some((&name.ident, None)),
        UseTree::Rename(rename) => Some((&rename.ident, None)),
        _ => None,
    }
}

fn keyword_rank(name: &str) -> u8 {
    match name {
        "self" => 0,
        "super" => 1,
        "crate" => 2,
        _ => 3,
    }
}

fn compare_names(left: &str, right: &str) -> Ordering {
    let (mut a, mut b) = (left.as_bytes(), right.as_bytes());

    while let (Some(&a_first), Some(&b_first)) = (a.first(), b.first()) {
        if a_first.is_ascii_digit() && b_first.is_ascii_digit() {
            let a_end = a
                .iter()
                .position(|c| !c.is_ascii_digit())
                .unwrap_or(a.len());

            let b_end = b
                .iter()
                .position(|c| !c.is_ascii_digit())
                .unwrap_or(b.len());

            let a_start = a[..a_end].iter().position(|&c| c != b'0').unwrap_or(a_end);
            let b_start = b[..b_end].iter().position(|&c| c != b'0').unwrap_or(b_end);
            let a_number = &a[a_start..a_end];
            let b_number = &b[b_start..b_end];

            let order = a_number
                .len()
                .cmp(&b_number.len())
                .then(a_number.cmp(b_number));

            if !order.is_eq() {
                return order;
            }

            a = &a[a_end..];
            b = &b[b_end..];
        } else {
            // Rust 2024 identifier ordering places underscores before letters.
            let order = (a_first != b'_', a_first).cmp(&(b_first != b'_', b_first));

            if !order.is_eq() {
                return order;
            }

            a = &a[1..];
            b = &b[1..];
        }
    }

    a.len().cmp(&b.len()).then_with(|| left.cmp(right))
}
