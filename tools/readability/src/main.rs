mod declarations;
mod spacing;

use std::collections::BTreeSet;
use std::error::Error;
use std::io::Write;
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::{env, fs};

use tempfile::NamedTempFile;

use crate::spacing::{apply, inspect};

const HELP: &str = "Rust readability checks

Usage: cargo readability --check [PATH ...]
       cargo readability --fix [PATH ...]
       cargo readability --staged

Without paths, --check and --fix scan tracked and unignored Rust files in crates/.
Explicit paths are relative to the current directory and may name directories.
--staged checks changed boundaries and declarations in staged crates/ Rust files.
--fix adds blank lines to working files; it never stages changes.
Declaration position, order, visibility, and attribute issues require manual edits.
Exit codes: 0 clean or fixed, 1 readability issues, 2 invalid input or tool failure.";

fn main() -> ExitCode {
    match run() {
        Ok(clean) => ExitCode::from(u8::from(!clean)),

        Err(error) => {
            eprintln!("readability: {error}");

            ExitCode::from(2)
        }
    }
}

fn run() -> Result<bool, Box<dyn Error>> {
    let mut arguments = env::args_os().skip(1);
    let mode = arguments.next().unwrap_or_default();

    if mode == "--help" || mode == "-h" {
        println!("{HELP}");

        return Ok(true);
    }

    if mode != "--check" && mode != "--fix" && mode != "--staged" {
        return Err(HELP.into());
    }

    let paths: Vec<PathBuf> = arguments.map(PathBuf::from).collect();

    if mode == "--staged" && !paths.is_empty() {
        return Err("--staged cannot be combined with paths or other modes".into());
    }

    if paths
        .iter()
        .any(|path| path.as_os_str().to_string_lossy().starts_with('-'))
    {
        return Err("unexpected option; choose one of --check, --fix, or --staged".into());
    }

    let directory = env::current_dir()?;
    let mut files = BTreeSet::new();
    let mut staged = Vec::new();

    if mode == "--staged" || paths.is_empty() {
        let root = git(&directory, &["rev-parse", "--show-toplevel"])?;
        let root = PathBuf::from(root.trim_end_matches(['\r', '\n']));

        env::set_current_dir(&root)?;

        if mode == "--staged" {
            let names = git(
                &root,
                &[
                    "diff",
                    "--cached",
                    "--name-only",
                    "-z",
                    "--no-renames",
                    "--diff-filter=ACM",
                    "--",
                    "crates/",
                ],
            )?;

            for name in names.split('\0').filter(|name| name.ends_with(".rs")) {
                let source = git(&root, &["show", &format!(":{name}")])?;

                let diff = git(
                    &root,
                    &[
                        "diff",
                        "--cached",
                        "--no-ext-diff",
                        "--no-textconv",
                        "--no-renames",
                        "--unified=0",
                        "--inter-hunk-context=0",
                        "--no-color",
                        "--text",
                        "--",
                        name,
                    ],
                )?;

                staged.push((PathBuf::from(name), source, changed_boundaries(&diff)?));
            }
        } else {
            let names = git(
                &root,
                &[
                    "ls-files",
                    "-z",
                    "--cached",
                    "--others",
                    "--exclude-standard",
                    "--",
                    "crates/",
                ],
            )?;

            for name in names.split('\0').filter(|name| name.ends_with(".rs")) {
                let path = PathBuf::from(name);

                if path.try_exists()? {
                    collect_files(&path, &mut files)?;
                }
            }
        }
    } else {
        for path in paths {
            collect_files(&path, &mut files)?;
        }
    }

    let mut count = 0;
    let mut declaration_count = 0;
    let checked = files.len() + staged.len();

    for (path, source, ranges) in staged {
        let parsed = parse(&path, &source)?;
        let issues = inspect(&source, &parsed);

        for (line, issue) in issues {
            if ranges
                .iter()
                .any(|range| *range.start() <= issue.through_line && *range.end() >= line)
            {
                println!(
                    "{}:{}:1: spacing/{}: missing blank line",
                    path.display(),
                    line + 1,
                    issue.rule
                );

                count += 1;
            }
        }

        for issue in declarations::inspect(&parsed.items) {
            if [issue.span, issue.related].iter().any(|span| {
                ranges.iter().any(|range| {
                    *range.start() < span.end().line && *range.end() >= span.start().line - 1
                })
            }) {
                report_declaration(&path, &issue);

                declaration_count += 1;
            }
        }
    }

    for path in files {
        let source = fs::read_to_string(&path)?;

        let mut parsed = parse(&path, &source)?;
        let issues = inspect(&source, &parsed);

        if mode == "--fix" && !issues.is_empty() {
            let modified = apply(&source, &issues)?;

            parsed = parse(&path, &modified)?;

            let parent = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or(Path::new("."));

            let mut temporary = NamedTempFile::new_in(parent)?;

            temporary.write_all(modified.as_bytes())?;

            temporary
                .as_file()
                .set_permissions(fs::metadata(&path)?.permissions())?;

            if fs::read(&path)? != source.as_bytes() {
                return Err(format!(
                    "{}: changed while checking; file left untouched",
                    path.display()
                )
                .into());
            }

            temporary.persist(&path)?;
            println!("{}: added {} blank line(s)", path.display(), issues.len());
        } else {
            for (line, issue) in &issues {
                println!(
                    "{}:{}:1: spacing/{}: missing blank line",
                    path.display(),
                    line + 1,
                    issue.rule
                );
            }
        }

        count += issues.len();

        for issue in declarations::inspect(&parsed.items) {
            report_declaration(&path, &issue);

            declaration_count += 1;
        }
    }

    println!(
        "readability: checked {checked} Rust file(s), {count} spacing issue(s){}, {declaration_count} declaration issue(s)",
        if mode == "--fix" { " fixed" } else { "" }
    );

    if (count > 0 || declaration_count > 0) && mode == "--staged" {
        eprintln!(
            "readability: run cargo readability --fix <path> for spacing, correct declaration issues, review the diff, then stage the intended changes"
        );
    }

    Ok((count == 0 || mode == "--fix") && declaration_count == 0)
}

fn parse(path: &Path, source: &str) -> Result<syn::File, Box<dyn Error>> {
    syn::parse_file(source).map_err(|error| {
        format!(
            "{}:{}:{}: syntax: {error}",
            path.display(),
            error.span().start().line,
            error.span().start().column + 1
        )
        .into()
    })
}

fn report_declaration(path: &Path, issue: &declarations::Issue) {
    println!(
        "{}:{}:{}: declarations/{}: {}",
        path.display(),
        issue.span.start().line,
        issue.span.start().column + 1,
        issue.rule,
        issue.message
    );
}

fn git(directory: &Path, arguments: &[&str]) -> Result<String, Box<dyn Error>> {
    let output = Command::new("git")
        .current_dir(directory)
        .env("GIT_LITERAL_PATHSPECS", "1")
        .args(arguments)
        .output()?;

    if !output.status.success() {
        return Err(format!(
            "git {} failed: {}",
            arguments[0],
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }

    Ok(String::from_utf8(output.stdout)?)
}

fn changed_boundaries(diff: &str) -> Result<Vec<RangeInclusive<usize>>, Box<dyn Error>> {
    let mut ranges = Vec::new();

    for line in diff.lines().filter(|line| line.starts_with("@@ ")) {
        let location = line
            .split_whitespace()
            .nth(2)
            .and_then(|part| part.strip_prefix('+'))
            .ok_or("invalid Git hunk header")?;

        let (start, count) = location.split_once(',').unwrap_or((location, "1"));
        let start: usize = start.parse()?;
        let count: usize = count.parse()?;

        // A deletion can remove the only blank line without adding any text.
        // Include both edges of additions and the join left by a deletion.
        ranges.push(if count == 0 {
            start..=start
        } else {
            start.saturating_sub(1)..=start + count - 1
        });
    }

    Ok(ranges)
}

fn collect_files(path: &Path, files: &mut BTreeSet<PathBuf>) -> Result<(), Box<dyn Error>> {
    let metadata = fs::symlink_metadata(path)?;

    if metadata.file_type().is_symlink() {
        return Err(format!("{}: symbolic links are not supported", path.display()).into());
    }

    if metadata.is_dir() {
        for entry in fs::read_dir(path)? {
            let entry = entry?;

            if entry.file_name() != "target" && entry.file_name() != ".git" {
                collect_files(&entry.path(), files)?;
            }
        }
    } else if path.extension().is_some_and(|extension| extension == "rs") {
        files.insert(path.to_path_buf());
    }

    Ok(())
}
