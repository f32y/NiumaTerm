use std::collections::{BTreeMap, HashSet};

use crate::ui::git_status::FileEntry;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TreeRow {
    pub(super) path: String,
    pub(super) label: String,
    pub(super) depth: usize,
    pub(super) file: Option<usize>,
    pub(super) expanded: bool,
}

#[derive(Default)]
struct Directory {
    children: BTreeMap<String, Directory>,
    files: Vec<usize>,
}

pub(super) fn rows(files: &[FileEntry], collapsed: &HashSet<String>, query: &str) -> Vec<TreeRow> {
    let query = query.trim().to_lowercase();

    if !query.is_empty() {
        return files
            .iter()
            .enumerate()
            .filter(|(_, file)| file.path.to_lowercase().contains(&query))
            .map(|(index, file)| TreeRow {
                path: file.path.clone(),
                label: file.path.clone(),
                depth: 0,
                file: Some(index),
                expanded: false,
            })
            .collect();
    }

    let mut root = Directory::default();

    for (index, file) in files.iter().enumerate() {
        let mut node = &mut root;
        let mut parts = file.path.split('/').peekable();

        while let Some(part) = parts.next() {
            if parts.peek().is_some() {
                node = node.children.entry(part.to_string()).or_default();
            }
        }

        node.files.push(index);
    }

    let mut result = Vec::new();

    append(&root, "", 0, files, collapsed, &mut result);

    result
}

fn append(
    node: &Directory,
    parent: &str,
    depth: usize,
    files: &[FileEntry],
    collapsed: &HashSet<String>,
    result: &mut Vec<TreeRow>,
) {
    for (name, directory) in &node.children {
        let mut label = name.clone();
        let mut directory = directory;

        while directory.files.is_empty() && directory.children.len() == 1 {
            let (name, child) = directory
                .children
                .first_key_value()
                .expect("one child exists");

            label.push('/');
            label.push_str(name);

            directory = child;
        }

        let path = if parent.is_empty() {
            label.clone()
        } else {
            format!("{parent}/{label}")
        };

        let expanded = !collapsed.contains(&path);

        result.push(TreeRow {
            path: path.clone(),
            label,
            depth,
            file: None,
            expanded,
        });

        if expanded {
            append(directory, &path, depth + 1, files, collapsed, result);
        }
    }

    for &index in &node.files {
        let file = &files[index];

        result.push(TreeRow {
            path: file.path.clone(),
            label: file
                .path
                .rsplit('/')
                .next()
                .unwrap_or(&file.path)
                .to_string(),
            depth,
            file: Some(index),
            expanded: false,
        });
    }
}
