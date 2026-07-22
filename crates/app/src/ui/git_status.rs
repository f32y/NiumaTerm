//! All git invocations run on the background executor with a hidden console
//! (`CREATE_NO_WINDOW`), following the `token_usage.rs` template.

use std::collections::HashMap;
use std::os::windows::process::CommandExt as _;
use std::time::Duration;
use std::{fs, io, path, process};

use gpui::prelude::*;
use gpui::{Context, Entity, SharedString, Window, div};
use gpui_component::{ActiveTheme, h_flex};
use tracing::warn;
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

use crate::ui::AppSettings;

const MAX_DIFF_LINES: usize = 100_000;

/// One changed path from `git status`, with its summed staged+unstaged line
/// counts (binary files count 0/0).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileEntry {
    pub(crate) path: String,
    /// The two-letter porcelain XY code (`??` for untracked).
    pub(crate) status: String,
    pub(crate) added: u64,
    pub(crate) removed: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GitSnapshot {
    pub(crate) repo_root: String,
    pub(crate) files: Vec<FileEntry>,
    pub(crate) total_added: u64,
    pub(crate) total_removed: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DiffLineKind {
    Added,
    Removed,
    Hunk,
    FileHeader,
    Context,
    Truncated,
}

#[derive(Debug, Clone)]
pub(crate) struct DiffLine {
    pub(crate) kind: DiffLineKind,
    pub(crate) text: SharedString,
}

fn run_git(dir: &str, args: &[&str]) -> Result<Vec<u8>, String> {
    let output = process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|err| format!("failed to run git: {err}"))?;

    if !output.status.success() {
        return Err(format!(
            "git {} exited with {}: {}",
            args.first().unwrap_or(&""),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    Ok(output.stdout)
}

pub(crate) fn resolve_repo_root(cwd: &str) -> Option<String> {
    let out = run_git(cwd, &["rev-parse", "--show-toplevel"]).ok()?;
    let root = String::from_utf8_lossy(&out).trim().to_string();
    (!root.is_empty()).then_some(root)
}

/// Full status snapshot for `root`: porcelain file list joined with summed
/// unstaged + staged numstat counts; untracked files counted as all-added.
pub(crate) fn fetch_snapshot(root: &str) -> Result<GitSnapshot, String> {
    let status = run_git(
        root,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?;

    let entries = parse_status_z(&status);
    let mut counts: HashMap<String, (u64, u64)> = HashMap::new();

    for args in [
        &["diff", "--numstat", "-z"][..],
        &["diff", "--numstat", "-z", "--cached"][..],
    ] {
        if let Ok(out) = run_git(root, args) {
            for (path, added, removed) in parse_numstat_z(&out) {
                let entry = counts.entry(path).or_default();
                entry.0 += added;
                entry.1 += removed;
            }
        }
    }

    let mut files = Vec::with_capacity(entries.len());
    let (mut total_added, mut total_removed) = (0u64, 0u64);

    for (status, path) in entries {
        let (added, removed) = if status == "??" {
            (count_file_lines(root, &path), 0)
        } else {
            counts.get(&path).copied().unwrap_or((0, 0))
        };

        total_added += added;
        total_removed += removed;

        files.push(FileEntry {
            path,
            status,
            added,
            removed,
        });
    }

    Ok(GitSnapshot {
        repo_root: root.to_string(),
        files,
        total_added,
        total_removed,
    })
}

/// Line count of an untracked file (its "all added" count); 0 for binary
/// (NUL-containing) or unreadable files. Streams in fixed chunks — untracked
/// files can be huge (build artifacts, datasets) and this runs every refresh
/// tick, so the whole file must never be pulled into memory at once.
fn count_file_lines(root: &str, path: &str) -> u64 {
    use std::io::Read as _;

    let Ok(file) = fs::File::open(path::Path::new(root).join(path)) else {
        return 0;
    };

    let mut reader = io::BufReader::new(file);
    let mut buf = [0u8; 64 * 1024];
    let mut newlines = 0u64;
    let mut last = b'\n';

    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let chunk = &buf[..n];

                if chunk.contains(&0) {
                    return 0;
                }

                newlines += chunk.iter().filter(|b| **b == b'\n').count() as u64;
                last = chunk[n - 1];
            }
            Err(_) => return 0,
        }
    }

    // A trailing fragment without a newline is still a line (matches numstat);
    // `last` starts as '\n' so an empty file counts zero.
    newlines + u64::from(last != b'\n')
}

/// Fetch and classify the unified diff of one file. Untracked files render
/// their full content as added lines; binary content gets a placeholder.
pub(crate) fn fetch_file_diff(root: &str, path: &str, untracked: bool) -> Vec<DiffLine> {
    if untracked {
        let Ok(bytes) = fs::read(path::Path::new(root).join(path)) else {
            return vec![line(DiffLineKind::FileHeader, "unreadable file")];
        };

        if bytes.contains(&0) {
            return vec![line(DiffLineKind::FileHeader, "Binary file")];
        }

        let text = String::from_utf8_lossy(&bytes);

        return cap_lines(
            text.lines()
                .map(|l| line(DiffLineKind::Added, format!("+{l}"))),
        );
    }

    match run_git(root, &["diff", "HEAD", "--", path]) {
        Ok(out) => {
            let text = String::from_utf8_lossy(&out);
            parse_diff(&text)
        }
        Err(err) => vec![line(DiffLineKind::FileHeader, err)],
    }
}

fn line(kind: DiffLineKind, text: impl Into<SharedString>) -> DiffLine {
    DiffLine {
        kind,
        text: text.into(),
    }
}

fn cap_lines(iter: impl Iterator<Item = DiffLine>) -> Vec<DiffLine> {
    let mut lines: Vec<DiffLine> = iter.take(MAX_DIFF_LINES + 1).collect();

    if lines.len() > MAX_DIFF_LINES {
        lines.truncate(MAX_DIFF_LINES);
        lines.push(line(DiffLineKind::Truncated, "··· diff truncated ···"));
    }
    lines
}

/// Parse `git status --porcelain=v1 -z` into `(XY, path)` pairs. Rename and
/// copy entries carry the original path in a second NUL-separated token,
/// which is consumed and dropped (the list shows the new path).
pub(crate) fn parse_status_z(raw: &[u8]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut tokens = raw.split(|b| *b == 0).filter(|t| !t.is_empty());
    while let Some(token) = tokens.next() {
        if token.len() < 4 {
            continue;
        }

        let status = String::from_utf8_lossy(&token[..2]).to_string();
        let path = String::from_utf8_lossy(&token[3..]).to_string();

        if status.contains('R') || status.contains('C') {
            tokens.next(); // the pre-rename path
        }

        out.push((status, path));
    }
    out
}

/// Parse `git diff --numstat -z` into `(path, added, removed)` triples.
/// Binary entries (`-\t-\t`) count as 0/0. In `-z` mode a rename entry is
/// `added\tremoved\t\0old\0new\0`; the new path is used.
pub(crate) fn parse_numstat_z(raw: &[u8]) -> Vec<(String, u64, u64)> {
    let mut out = Vec::new();
    let mut tokens = raw.split(|b| *b == 0).filter(|t| !t.is_empty());

    while let Some(token) = tokens.next() {
        let text = String::from_utf8_lossy(token);

        let mut fields = text.splitn(3, '\t');

        let (Some(added), Some(removed)) = (fields.next(), fields.next()) else {
            continue;
        };

        // `-` marks a binary file.
        let added = added.parse::<u64>().unwrap_or(0);
        let removed = removed.parse::<u64>().unwrap_or(0);

        let path = match fields.next() {
            // Rename: the counts token ends at the tab, old and new paths
            // follow as their own NUL tokens.
            None | Some("") => {
                tokens.next(); // old path
                match tokens.next() {
                    Some(new) => String::from_utf8_lossy(new).to_string(),
                    None => continue,
                }
            }
            Some(path) => path.to_string(),
        };

        out.push((path, added, removed));
    }
    out
}

/// Classify unified-diff lines by prefix, capped at [`MAX_DIFF_LINES`].
pub(crate) fn parse_diff(text: &str) -> Vec<DiffLine> {
    cap_lines(text.lines().map(|l| {
        let kind = if l.starts_with("@@") {
            DiffLineKind::Hunk
        } else if l.starts_with("+++") || l.starts_with("---") {
            DiffLineKind::FileHeader
        } else if l.starts_with('+') {
            DiffLineKind::Added
        } else if l.starts_with('-') {
            DiffLineKind::Removed
        } else if l.starts_with(' ') {
            DiffLineKind::Context
        } else {
            // diff --git, index, mode, similarity, "\ No newline" …
            DiffLineKind::FileHeader
        };

        line(kind, l.to_string())
    }))
}

/// Owns the latest [`GitSnapshot`] and the refresh loop. The titlebar
/// [`GitStatusView`] and the git sidebar both `cx.observe` this entity.
pub(crate) struct GitStatusModel {
    target_cwd: Option<String>,
    pub(crate) snapshot: Option<GitSnapshot>,
    /// Bumped each time a snapshot lands, so observers can tell data changes
    /// apart from `refreshing` flag flips.
    pub(crate) snapshot_seq: u64,
    /// Bumped on target change; in-flight results from older generations are
    /// discarded on arrival.
    generation: u64,
    refreshing: bool,
    enabled: bool,
    pub(crate) sidebar_open: bool,
}

impl GitStatusModel {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        let enabled = cx.global::<AppSettings>().show_git_status_on_title_bar;

        cx.observe_global::<AppSettings>(|this, cx| {
            let enabled = cx.global::<AppSettings>().show_git_status_on_title_bar;

            if enabled && !this.enabled {
                this.refresh(cx);
            }

            this.enabled = enabled;
        })
        .detach();

        // Interval loop; the period is re-read each tick so the settings
        // dropdown takes effect at the next tick without restart plumbing.
        cx.spawn(async move |this, cx| {
            loop {
                let Ok(interval) = this.update(cx, |_, cx| {
                    cx.global::<AppSettings>().git_status_refresh_interval
                }) else {
                    break;
                };

                cx.background_executor()
                    .timer(Duration::from_secs(interval.max(1)))
                    .await;

                let alive = this.update(cx, |this, cx| {
                    if this.active() {
                        this.refresh(cx);
                    }
                });

                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();

        Self {
            target_cwd: None,
            snapshot: None,
            snapshot_seq: 0,
            generation: 0,
            refreshing: false,
            enabled,
            sidebar_open: false,
        }
    }

    fn active(&self) -> bool {
        self.enabled || self.sidebar_open
    }

    /// Idempotent target sync; `Shell` calls this on every render. A real
    /// change bumps the generation and (when a consumer is active) refreshes.
    pub(crate) fn set_target_cwd(&mut self, cwd: Option<String>, cx: &mut Context<Self>) {
        if cwd == self.target_cwd {
            return;
        }

        self.target_cwd = cwd;
        self.generation += 1;

        if self.active() {
            self.refresh(cx);
        }
    }

    /// Kick one refresh; no-op while one is in flight. Resolve the repository root
    /// first, dropping any snapshot when the root changes, then fetch and apply the
    /// new snapshot.
    pub(crate) fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.refreshing {
            return;
        }

        let Some(cwd) = self.target_cwd.clone() else {
            if self.snapshot.take().is_some() {
                self.snapshot_seq += 1;

                cx.notify();
            }

            return;
        };

        self.refreshing = true;

        let generation = self.generation;

        cx.notify();

        cx.spawn(async move |this, cx| {
            let root = cx
                .background_executor()
                .spawn(async move { resolve_repo_root(&cwd) })
                .await;

            let proceed = this
                .update(cx, |this, cx| {
                    if this.generation != generation {
                        // Retargeted mid-flight: restart for the new target.
                        this.refreshing = false;
                        this.refresh(cx);
                        return None;
                    }

                    match root {
                        None => {
                            // Not a repo: clear and stop.
                            this.refreshing = false;
                            if this.snapshot.take().is_some() {
                                this.snapshot_seq += 1;
                            }
                            cx.notify();
                            None
                        }
                        Some(root) => {
                            // Different repo: drop the stale snapshot now so
                            // the old repo's data never shows for the new one.
                            if this.snapshot.as_ref().is_some_and(|s| s.repo_root != root) {
                                this.snapshot = None;
                                this.snapshot_seq += 1;
                                cx.notify();
                            }
                            Some(root)
                        }
                    }
                })
                .ok()
                .flatten();

            let Some(root) = proceed else {
                return;
            };

            let snapshot = cx
                .background_executor()
                .spawn(async move { fetch_snapshot(&root) })
                .await;

            this.update(cx, |this, cx| {
                this.refreshing = false;

                if this.generation != generation {
                    this.refresh(cx);
                    return;
                }

                match snapshot {
                    Ok(snapshot) => {
                        this.snapshot = Some(snapshot);
                        this.snapshot_seq += 1;
                    }
                    Err(err) => warn!("git status refresh failed: {err}"),
                }

                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

pub(crate) struct GitStatusView {
    model: Entity<GitStatusModel>,
}

impl GitStatusView {
    pub(crate) fn new(model: Entity<GitStatusModel>, cx: &mut Context<Self>) -> Self {
        cx.observe(&model, |_, _, cx| cx.notify()).detach();

        cx.observe_global::<AppSettings>(|_, cx| cx.notify())
            .detach();

        Self { model }
    }
}

impl Render for GitStatusView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !cx.global::<AppSettings>().show_git_status_on_title_bar {
            return div().into_any_element();
        }

        let Some(snapshot) = self.model.read(cx).snapshot.clone() else {
            return div().into_any_element();
        };

        if snapshot.total_added == 0 && snapshot.total_removed == 0 {
            return div().into_any_element();
        }

        h_flex()
            .gap_1()
            .px_2()
            .text_sm()
            .child(
                div()
                    .text_color(cx.theme().green)
                    .child(format!("+{}", snapshot.total_added)),
            )
            .child(
                div()
                    .text_color(cx.theme().red)
                    .child(format!("-{}", snapshot.total_removed)),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use std::env;

    use super::*;

    #[test]
    fn status_z_parses_plain_untracked_and_rename() {
        let raw =
            b" M src/main.rs\0?? new file.txt\0R  new-name.rs\0old-name.rs\0A  a\xC3\xA9.txt\0";
        let entries = parse_status_z(raw);
        assert_eq!(
            entries,
            vec![
                (" M".to_string(), "src/main.rs".to_string()),
                ("??".to_string(), "new file.txt".to_string()),
                ("R ".to_string(), "new-name.rs".to_string()),
                ("A ".to_string(), "aé.txt".to_string()),
            ]
        );
    }

    #[test]
    fn numstat_z_parses_counts_binary_and_rename() {
        let raw = b"3\t1\tsrc/lib.rs\0-\t-\tassets/logo.png\05\t0\t\0old.rs\0new.rs\0";
        let entries = parse_numstat_z(raw);
        assert_eq!(
            entries,
            vec![
                ("src/lib.rs".to_string(), 3, 1),
                ("assets/logo.png".to_string(), 0, 0),
                ("new.rs".to_string(), 5, 0),
            ]
        );
    }

    #[test]
    fn numstat_z_handles_unicode_paths() {
        let raw = "2\t0\tdocs/héllo wörld.md\0".as_bytes();
        assert_eq!(
            parse_numstat_z(raw),
            vec![("docs/héllo wörld.md".to_string(), 2, 0)]
        );
    }

    #[test]
    fn diff_lines_classify_by_prefix() {
        let text = "diff --git a/f b/f\nindex 123..456 100644\n--- a/f\n+++ b/f\n@@ -1,2 +1,2 @@\n context\n-removed\n+added\n";
        let kinds: Vec<DiffLineKind> = parse_diff(text).iter().map(|l| l.kind).collect();
        assert_eq!(
            kinds,
            vec![
                DiffLineKind::FileHeader,
                DiffLineKind::FileHeader,
                DiffLineKind::FileHeader,
                DiffLineKind::FileHeader,
                DiffLineKind::Hunk,
                DiffLineKind::Context,
                DiffLineKind::Removed,
                DiffLineKind::Added,
            ]
        );
    }

    #[test]
    fn diff_truncates_past_cap() {
        let text = "+x\n".repeat(MAX_DIFF_LINES + 10);
        let lines = parse_diff(&text);
        assert_eq!(lines.len(), MAX_DIFF_LINES + 1);
        assert_eq!(lines.last().unwrap().kind, DiffLineKind::Truncated);
    }

    #[test]
    fn count_lines_handles_trailing_newline_and_binary() {
        let dir = env::temp_dir().join(format!("nmt-count-lines-{}", process::id()));
        fs::create_dir_all(&dir).unwrap();
        let root = dir.to_string_lossy().to_string();

        let check = |name: &str, bytes: &[u8], expected: u64| {
            fs::write(dir.join(name), bytes).unwrap();
            assert_eq!(count_file_lines(&root, name), expected, "{name}");
        };

        check("empty", b"", 0);
        check("trailing", b"one\ntwo\n", 2);
        check("no-trailing", b"one\ntwo", 2);
        check("binary", b"bin\0ary", 0);
        assert_eq!(count_file_lines(&root, "missing"), 0);

        let _ = fs::remove_dir_all(&dir);
    }
}
