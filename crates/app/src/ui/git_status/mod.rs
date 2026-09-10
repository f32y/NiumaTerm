//! All git invocations run on the background executor without opening a console.

use std::collections::HashMap;
use std::time::Duration;
use std::{fs, io, path};

use gpui::prelude::*;
use gpui::{Context, Entity, SharedString, Window, div};
use gpui_component::h_flex;
use nmt_agent_utils::git::{CheckedOut, current_branch, run_git};
use nmt_i18n::i18n;
use tracing::warn;

use crate::ui::AppSettings;
use crate::ui::composition::GitColors;

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
    /// What `HEAD` points at, for the chrome that names it. `None` on a
    /// repository whose `HEAD` cannot be resolved at all, such as one with no
    /// commits yet.
    pub(crate) branch: Option<String>,
    pub(crate) files: Vec<FileEntry>,
    pub(crate) total_added: u64,
    pub(crate) total_removed: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DiffLineKind {
    Added,
    Removed,
    Hunk,
    Notice,
    Context,
    Truncated,
}

#[derive(Debug, Clone)]
pub(crate) struct DiffLine {
    pub(crate) kind: DiffLineKind,
    pub(crate) text: SharedString,
    pub(crate) old_line: Option<u64>,
    pub(crate) new_line: Option<u64>,
}

pub(crate) fn resolve_repo_root(cwd: &str) -> Option<String> {
    let out = run_git(cwd, &["rev-parse", "--show-toplevel"]).ok()?;
    let root = String::from_utf8_lossy(&out).trim().to_string();
    (!root.is_empty()).then_some(root)
}

/// Full status snapshot for `root`: porcelain file list joined with summed
/// unstaged + staged numstat counts; untracked files counted as all-added.
pub(crate) fn fetch_snapshot(root: &str, branch_max_age: Duration) -> Result<GitSnapshot, String> {
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
        branch: current_branch(root, branch_max_age).map(|checked_out| match checked_out {
            CheckedOut::Branch(branch) => branch,
            CheckedOut::Detached(commit) => commit,
        }),
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
            return vec![line(
                DiffLineKind::Notice,
                i18n("git-status-unreadable-file"),
            )];
        };

        if bytes.contains(&0) {
            return vec![line(DiffLineKind::Notice, i18n("git-status-binary-file"))];
        }

        let text = String::from_utf8_lossy(&bytes);

        return cap_lines(text.lines().enumerate().map(|(index, text)| DiffLine {
            new_line: Some(index as u64 + 1),
            ..line(DiffLineKind::Added, text.to_string())
        }));
    }

    match run_git(
        root,
        &[
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "HEAD",
            "--",
            path,
        ],
    ) {
        Ok(out) => {
            let text = String::from_utf8_lossy(&out);
            parse_diff(&text)
        }
        Err(err) => vec![line(DiffLineKind::Notice, err)],
    }
}

fn line(kind: DiffLineKind, text: impl Into<SharedString>) -> DiffLine {
    DiffLine {
        kind,
        text: text.into(),
        old_line: None,
        new_line: None,
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

/// Decode content inside unified hunks; file metadata is presented by the file list.
pub(crate) fn parse_diff(text: &str) -> Vec<DiffLine> {
    let mut hunk: Option<Hunk> = None;
    cap_lines(text.lines().filter_map(|text| {
        if let Some(parsed) = Hunk::parse(text) {
            hunk = Some(parsed);
            return Some(line(DiffLineKind::Hunk, text.to_string()));
        }
        if text.starts_with("\\ No newline at end of file") {
            return Some(line(DiffLineKind::Notice, text.to_string()));
        }
        if text.starts_with("Binary files ") || text == "GIT binary patch" {
            return Some(line(DiffLineKind::Notice, i18n("git-status-binary-file")));
        }
        let hunk = hunk.as_mut()?;
        let (prefix, content) = text.split_at_checked(1)?;
        let (kind, old, new) = match prefix {
            " " if hunk.old_remaining > 0 && hunk.new_remaining > 0 => {
                (DiffLineKind::Context, true, true)
            }
            "-" if hunk.old_remaining > 0 => (DiffLineKind::Removed, true, false),
            "+" if hunk.new_remaining > 0 => (DiffLineKind::Added, false, true),
            _ => return None,
        };
        let row = DiffLine {
            old_line: old.then_some(hunk.old),
            new_line: new.then_some(hunk.new),
            ..line(kind, content.to_string())
        };
        if old {
            hunk.old += 1;
            hunk.old_remaining -= 1;
        }
        if new {
            hunk.new += 1;
            hunk.new_remaining -= 1;
        }
        Some(row)
    }))
}

struct Hunk {
    old: u64,
    new: u64,
    old_remaining: u64,
    new_remaining: u64,
}

impl Hunk {
    fn parse(text: &str) -> Option<Self> {
        let mut parts = text.strip_prefix("@@ ")?.split_whitespace();
        let old = parts.next()?.strip_prefix('-')?;
        let new = parts.next()?.strip_prefix('+')?;
        if parts.next()? != "@@" {
            return None;
        }
        let range = |value: &str| -> Option<(u64, u64)> {
            let (start, count) = value.split_once(',').unwrap_or((value, "1"));
            Some((start.parse().ok()?, count.parse().ok()?))
        };
        let (old, old_remaining) = range(old)?;
        let (new, new_remaining) = range(new)?;
        Some(Self {
            old,
            new,
            old_remaining,
            new_remaining,
        })
    }
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

        // The conversation tabs watch the branch of their own directories on
        // this same interval, so an answer read within one is shared with them
        // rather than read again here.
        let branch_max_age = Duration::from_secs(
            cx.global::<AppSettings>()
                .git_status_refresh_interval
                .max(1),
        );

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
                .spawn(async move { fetch_snapshot(&root, branch_max_age) })
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

        let colors = GitColors::new(cx);
        h_flex()
            .gap_1()
            .px_2()
            .text_sm()
            .child(
                div()
                    .text_color(colors.added)
                    .child(format!("+{}", snapshot.total_added)),
            )
            .child(
                div()
                    .text_color(colors.removed)
                    .child(format!("-{}", snapshot.total_removed)),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests;
