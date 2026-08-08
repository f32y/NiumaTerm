use crate::agent_pane::*;

/// A transcript entry plus the local wall-clock time it first appeared
/// (shown on hover) and the turn it belongs to (drives turn folding).
/// Streamed items keep their start time.
pub(super) struct Entry {
    pub(super) at: String,
    pub(super) turn: u64,
    pub(super) item: SessionItem,
}

pub(super) fn should_show_jump_to_latest(
    is_following_tail: bool,
    is_scrolled_to_end: Option<bool>,
    max_scroll_offset: Pixels,
) -> bool {
    if is_following_tail {
        return false;
    }

    match is_scrolled_to_end {
        Some(at_end) => !at_end,
        // A short bottom-aligned list can leave tail-follow after an upward
        // wheel gesture even though it has no scroll range. Unknown-height
        // scrollable lists still fall back to the established follow state.
        None => max_scroll_offset > px(0.),
    }
}

/// The live turn-duration label.
pub(super) fn working_label(started: Instant) -> String {
    format!("Working for {}s", started.elapsed().as_secs())
}

/// Work-log rows: the single-line tool/thinking entries that participate in
/// "+N tool calls" run-collapsing. Conversation text never does.
pub(super) fn is_work_row(item: &SessionItem) -> bool {
    matches!(
        item,
        SessionItem::CommandExecution { .. }
            | SessionItem::FileChange { .. }
            | SessionItem::Other { .. }
            | SessionItem::Reasoning { .. }
    )
}

/// Entries with nothing to show (yet): an agent bubble before its first delta,
/// or a reasoning item that never streamed a summary. They render no row and
/// are transparent to work-run grouping, so an invisible entry can't split a
/// run of tool calls into two summary lines.
pub(super) fn hidden(item: &SessionItem) -> bool {
    match item {
        SessionItem::UserMessage { text }
        | SessionItem::AgentMessage { text, .. }
        | SessionItem::Reasoning { summary: text, .. } => {
            text.as_deref().is_none_or(|text| text.trim().is_empty())
        }
        _ => false,
    }
}

/// Collapsed head of an oversized user prompt, cut at a line boundary when
/// possible, or `None` when the whole prompt fits under the caps. The character
/// cap bounds visual wrapping for giant single-line pastes; a byte cap alone
/// can still produce dozens of wrapped lines in a narrow bubble.
pub(super) fn truncated_user_prompt(text: &str) -> Option<&str> {
    const MAX_SOURCE_LINES: usize = 3;
    const MAX_CHARS: usize = 512;

    let mut chars = 0;
    let mut completed_lines = 0;
    for (index, ch) in text.char_indices() {
        if chars == MAX_CHARS {
            return Some(&text[..index]);
        }
        chars += 1;

        if ch == '\n' {
            completed_lines += 1;
            let end = index + ch.len_utf8();
            if completed_lines == MAX_SOURCE_LINES && end < text.len() {
                return Some(&text[..end]);
            }
        }
    }

    None
}

/// Wrap tool output in a Markdown fence with an explicit language tag. The
/// fence grows past any backtick run in the body, so raw output can never
/// terminate the syntax-highlighted block early.
pub(super) fn fenced_code_block_as(output: &str, lang: &str) -> String {
    let mut fence = String::from("```");
    while output.contains(fence.as_str()) {
        fence.push('`');
    }
    format!("{fence}{lang}\n{output}\n{fence}")
}

/// First-bytes sniff covering the two formats tool output actually produces
/// in bulk (diffs and JSON payloads); anything else renders as an unhighlighted
/// code block. Extend per-tool only if more grammars earn their keep.
pub(super) fn detect_output_language(output: &str) -> &'static str {
    let trimmed = output.trim_start();
    if trimmed.starts_with("diff --git") || trimmed.starts_with("@@ ") {
        "diff"
    } else if trimmed.starts_with('{') || trimmed.starts_with('[') {
        "json"
    } else {
        ""
    }
}

/// Claude's Read tool returns cat -n style lines ("   12→text"); strip the
/// gutter so the source underneath highlights as its own language. Any line
/// without the gutter leaves the text untouched (format drift safety).
pub(super) fn strip_read_gutter(output: &str) -> Option<String> {
    let mut body = String::with_capacity(output.len());

    for line in output.lines() {
        let (gutter, text) = line.split_once('→')?;
        let number = gutter.trim_start();
        if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        body.push_str(text);
        body.push('\n');
    }

    (!body.is_empty()).then_some(body)
}

/// Fence-language tag for a file path. The highlight registry accepts file
/// extensions as language aliases (rs, py, yml, …) and resolves unknown ones
/// to plain, so the extension itself is the tag.
pub(super) fn file_extension_lang(path: &str) -> String {
    Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

const VIRTUAL_TRANSCRIPT_MIN_BYTES: usize = 16 * 1024;
const VIRTUAL_TRANSCRIPT_MIN_ROWS: usize = 128;
const VIRTUAL_TRANSCRIPT_MAX_SEGMENT_BYTES: usize = 4 * 1024;

pub(super) fn should_virtualize_transcript(code: bool, text: &str) -> bool {
    code && (text.len() >= VIRTUAL_TRANSCRIPT_MIN_BYTES
        || text.lines().take(VIRTUAL_TRANSCRIPT_MIN_ROWS + 1).count() > VIRTUAL_TRANSCRIPT_MIN_ROWS)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct TranscriptSourceKey {
    allocation: usize,
    len: usize,
    edge_hash: u64,
    strip_read_gutter: bool,
}

pub(super) fn transcript_source_key(text: &str, strip_read_gutter: bool) -> TranscriptSourceKey {
    // Allocation identity and bounded edges catch streamed appends and full
    // payload replacements without rescanning a potentially multi-megabyte
    // transcript whenever unrelated pane state triggers a render.
    let bytes = text.as_bytes();
    let mut edge_hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes.iter().take(64).chain(bytes.iter().rev().take(64)) {
        edge_hash ^= u64::from(*byte);
        edge_hash = edge_hash.wrapping_mul(0x100_0000_01b3);
    }

    TranscriptSourceKey {
        allocation: bytes.as_ptr() as usize,
        len: bytes.len(),
        edge_hash,
        strip_read_gutter,
    }
}

pub(super) fn transcript_segments(text: &str) -> Vec<Range<usize>> {
    let mut segments = Vec::new();
    let mut line_start = 0;

    for line in text.split_inclusive('\n') {
        let mut line_end = line_start + line.len();
        if line.ends_with('\n') {
            line_end -= 1;
            if line_end > line_start && text.as_bytes()[line_end - 1] == b'\r' {
                line_end -= 1;
            }
        }

        if line_start == line_end {
            segments.push(line_start..line_start);
        } else {
            let mut segment_start = line_start;
            while segment_start < line_end {
                let mut segment_end =
                    (segment_start + VIRTUAL_TRANSCRIPT_MAX_SEGMENT_BYTES).min(line_end);
                while !text.is_char_boundary(segment_end) {
                    segment_end -= 1;
                }
                segments.push(segment_start..segment_end);
                segment_start = segment_end;
            }
        }

        line_start += line.len();
    }

    if segments.is_empty() && !text.is_empty() {
        segments.push(0..text.len());
    }

    segments
}

pub(super) struct VirtualTranscriptState {
    source_key: TranscriptSourceKey,
    source: SharedString,
    segments: Rc<Vec<Range<usize>>>,
    widest_segment: usize,
    scroll: UniformListScrollHandle,
}

impl VirtualTranscriptState {
    fn new(text: &str, strip_gutter: bool) -> Self {
        let source_key = transcript_source_key(text, strip_gutter);
        let source = normalized_virtual_transcript(text, strip_gutter);
        let segments = transcript_segments(&source);
        let widest_segment = segments
            .iter()
            .enumerate()
            .max_by_key(|(_, range)| range.len())
            .map_or(0, |(index, _)| index);

        Self {
            source_key,
            source: SharedString::from(source),
            segments: Rc::new(segments),
            widest_segment,
            scroll: UniformListScrollHandle::default(),
        }
    }

    fn sync(&mut self, text: &str, strip_gutter: bool) {
        let source_key = transcript_source_key(text, strip_gutter);
        if self.source_key == source_key {
            return;
        }

        let source = normalized_virtual_transcript(text, strip_gutter);
        let segments = transcript_segments(&source);
        self.widest_segment = segments
            .iter()
            .enumerate()
            .max_by_key(|(_, range)| range.len())
            .map_or(0, |(index, _)| index);
        self.source_key = source_key;
        self.source = SharedString::from(source);
        self.segments = Rc::new(segments);
    }
}

pub(super) fn normalized_virtual_transcript(text: &str, strip_gutter: bool) -> String {
    if strip_gutter {
        strip_read_gutter(text).unwrap_or_else(|| text.to_owned())
    } else {
        text.to_owned()
    }
}

/// Returns the syntax language and whether a Read gutter needs stripping.
/// `None` identifies Markdown-native transcript details that retain rich text
/// rendering instead of entering the code-oriented virtual list.
pub(super) fn code_transcript_format(
    item: &SessionItem,
    detail: &str,
) -> Option<(Cow<'static, str>, bool)> {
    match item {
        SessionItem::CommandExecution { .. } => {
            Some((Cow::Borrowed(detect_output_language(detail)), false))
        }
        SessionItem::FileChange { .. } => Some((Cow::Borrowed("diff"), false)),
        SessionItem::Other { kind, title, .. } => match kind.as_str() {
            "TodoWrite" | "ExitPlanMode" | "Task" => None,
            "Read" => Some((Cow::Owned(file_extension_lang(title)), true)),
            _ => Some((Cow::Borrowed(detect_output_language(detail)), false)),
        },
        SessionItem::Reasoning { .. } => None,
        _ => None,
    }
}

const COMMAND_EXECUTION_HEADING: &str = "Run Command";

pub(super) fn command_execution_detail(command: &str, aggregated_output: Option<&str>) -> String {
    let mut detail = String::with_capacity(
        command.len() + aggregated_output.map_or(0, str::len) + "$ \n\n".len(),
    );
    detail.push_str("$ ");
    detail.push_str(command);

    if let Some(output) = aggregated_output.filter(|output| !output.is_empty()) {
        detail.push_str("\n\n");
        detail.push_str(output);
    }

    detail
}

/// Full text of an entry for the right-click Copy action — the whole message,
/// independent of any partial selection or truncated preview.
pub(super) fn entry_copy_text(item: &SessionItem) -> String {
    match item {
        SessionItem::UserMessage { text }
        | SessionItem::AgentMessage { text, .. }
        | SessionItem::Reasoning { summary: text, .. } => text.clone().unwrap_or_default(),
        SessionItem::Error { text } => text.clone(),
        SessionItem::CommandExecution {
            command,
            aggregated_output,
            ..
        } => command_execution_detail(command, aggregated_output.as_deref()),
        SessionItem::FileChange {
            paths,
            diff,
            status,
            ..
        } => match diff {
            Some(diff) => format!(
                "Edit {paths} — {}\n{diff}",
                status.as_deref().unwrap_or("inProgress")
            ),
            None => format!(
                "Edit {paths} — {}",
                status.as_deref().unwrap_or("inProgress")
            ),
        },
        SessionItem::Other {
            kind,
            title,
            output,
            status,
            ..
        } => match output {
            Some(output) => format!(
                "{kind} {title} — {}\n{output}",
                status.as_deref().unwrap_or("inProgress")
            ),
            None => format!(
                "{kind} {title} — {}",
                status.as_deref().unwrap_or("inProgress")
            ),
        },
        SessionItem::Compaction { detail, .. } => {
            let head = format!(
                "{}\n{}",
                compaction_label(detail),
                compaction_accounting(detail).join(" · ")
            );

            match &detail.summary {
                Some(summary) => format!("{head}\n\n{summary}"),
                None => head,
            }
        }
    }
}

/// Heading of a compaction row. An unprompted compaction is named as such
/// because it explains a context-gauge jump the user did not ask for.
pub(super) fn compaction_label(detail: &Compaction) -> &'static str {
    match detail.trigger {
        Some(CompactionTrigger::Automatic) => "Context auto-compacted",
        Some(CompactionTrigger::Manual) | None => "Context compacted",
    }
}

pub(super) fn compaction_row_is_expandable(kind: AgentKind) -> bool {
    kind == AgentKind::Claude
}

/// Token and message accounting of a compaction, as display-ready fragments.
/// Only what the backend actually reported appears, so a partially described
/// compaction shows fewer fragments instead of zeros.
pub(super) fn compaction_accounting(detail: &Compaction) -> Vec<String> {
    let mut parts = Vec::new();

    match (detail.pre_tokens, detail.post_tokens) {
        (Some(pre), Some(post)) => parts.push(format!(
            "{} → {}",
            compact_token_count(pre),
            compact_token_count(post)
        )),
        (Some(pre), None) => parts.push(format!("from {}", compact_token_count(pre))),
        (None, Some(post)) => parts.push(format!("to {}", compact_token_count(post))),
        (None, None) => {}
    }

    if let Some(pre) = detail.pre_tokens
        && let Some(post) = detail.post_tokens
        && let Some(freed) = pre.checked_sub(post).filter(|freed| *freed > 0)
    {
        parts.push(format!("{} freed", compact_token_count(freed)));
    }

    if let Some(messages) = detail.messages_summarized {
        parts.push(format!("{messages} messages summarized"));
    }

    if let Some(trigger) = detail.trigger {
        parts.push(trigger.label().to_string());
    }

    parts
}

/// Compact "how long ago" label for a history row ("now", "5m", "3h", "2d").
pub(super) fn relative_time(at: SystemTime) -> String {
    let seconds = at.elapsed().map(|d| d.as_secs()).unwrap_or(0);

    match seconds {
        0..60 => "now".to_string(),
        60..3600 => format!("{}m", seconds / 60),
        3600..86400 => format!("{}h", seconds / 3600),
        _ => format!("{}d", seconds / 86400),
    }
}

pub(super) fn compact_token_count(tokens: u64) -> String {
    match tokens {
        0..=999 => tokens.to_string(),
        1_000..=9_999 => format!("{:.1}k", tokens as f64 / 1_000.0).replace(".0k", "k"),
        10_000..=999_999 => format!("{}k", tokens / 1_000),
        _ => format!("{:.1}m", tokens as f64 / 1_000_000.0).replace(".0m", "m"),
    }
}

/// Icon mirroring the current permission/approval choice (t3code's runtime
/// mode iconography): closed lock = prompts on, pen = edits auto-approved,
/// pencil-ruler = plan mode, open lock = no prompts. Covers both Claude's
/// permission modes and Codex's approval policies.
pub(super) fn permission_icon(value: Option<&str>) -> IconName {
    match value {
        Some("acceptEdits") => IconName::PenLine,
        Some("plan") => IconName::PencilRuler,
        Some("bypassPermissions") | Some("never") => IconName::LockOpen,
        _ => IconName::Lock,
    }
}

const AGENT_DISCLOSURE_SLOT: f32 = 16.0;
const AGENT_DISCLOSURE_GAP: f32 = 4.0;
const AGENT_DISCLOSURE_PADDING: f32 = 4.0;
const AGENT_DISCLOSURE_DETAIL_INSET: f32 =
    AGENT_DISCLOSURE_PADDING + AGENT_DISCLOSURE_SLOT * 2.0 + AGENT_DISCLOSURE_GAP * 2.0;
const AGENT_TEXT_MEASURE_REMS: f32 = 48.0;

/// Shared geometry for expandable transcript rows. Empty chevron, type-icon,
/// and trailing slots keep labels aligned by default; summary toggles can omit
/// the unused type-icon slot so their label follows the chevron directly.
pub(super) struct AgentDisclosureRow {
    id: ElementId,
    expanded: Option<bool>,
    type_icon: Option<IconName>,
    reserve_type_icon_slot: bool,
    label: String,
    preview: Option<String>,
    trailing: Option<AnyElement>,
    accessible_label: String,
    /// Tint for the label and type icon, replacing the quiet work-log default.
    /// The row sets its own text colors per slot, so a caller cannot override
    /// them from the outside.
    accent: Option<Hsla>,
}

impl AgentDisclosureRow {
    fn new(id: impl Into<ElementId>, label: impl Into<String>) -> Self {
        let label = label.into();
        Self {
            id: id.into(),
            expanded: None,
            type_icon: None,
            reserve_type_icon_slot: true,
            accessible_label: label.clone(),
            label,
            preview: None,
            trailing: None,
            accent: None,
        }
    }

    /// Mark the row as a structural break rather than one step of the work log.
    fn accent(mut self, color: Hsla) -> Self {
        self.accent = Some(color);
        self
    }

    fn expanded(mut self, expanded: bool) -> Self {
        self.expanded = Some(expanded);
        self
    }

    fn type_icon(mut self, icon: IconName) -> Self {
        self.type_icon = Some(icon);
        self
    }

    fn without_type_icon_slot(mut self) -> Self {
        self.reserve_type_icon_slot = false;
        self
    }

    fn preview(mut self, preview: impl Into<String>) -> Self {
        self.preview = Some(preview.into());
        self
    }

    fn trailing(mut self, trailing: Option<AnyElement>) -> Self {
        self.trailing = trailing;
        self
    }

    fn accessible_label(mut self, label: impl Into<String>) -> Self {
        self.accessible_label = label.into();
        self
    }

    fn render(self, cx: &mut Context<AgentPane>) -> Stateful<Div> {
        let hover_bg = cx.theme().muted.opacity(0.4);
        let expandable = self.expanded.is_some();
        let chevron = self.expanded.map(|expanded| {
            Icon::new(if expanded {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            })
            .size_3()
            .text_color(cx.theme().muted_foreground.opacity(0.7))
        });
        let show_type_icon_slot = self.type_icon.is_some() || self.reserve_type_icon_slot;
        let icon_color = self
            .accent
            .unwrap_or_else(|| cx.theme().muted_foreground.opacity(0.8));
        let label_color = self
            .accent
            .unwrap_or_else(|| cx.theme().foreground.opacity(0.82));
        let type_icon = self
            .type_icon
            .map(|icon| Icon::new(icon).size_3p5().text_color(icon_color));
        h_flex()
            .id(self.id)
            .w_full()
            .min_h(px(24.))
            .gap(px(AGENT_DISCLOSURE_GAP))
            .items_center()
            .px(px(AGENT_DISCLOSURE_PADDING))
            .py_0p5()
            .rounded(UI_RADIUS)
            .aria_label(self.accessible_label)
            .when(expandable, |this| {
                this.cursor_pointer()
                    .role(gpui::Role::Button)
                    .hover(move |style| style.bg(hover_bg))
            })
            .child(
                div()
                    .w(px(AGENT_DISCLOSURE_SLOT))
                    .h(px(AGENT_DISCLOSURE_SLOT))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .children(chevron),
            )
            .when(show_type_icon_slot, |this| {
                this.child(
                    div()
                        .w(px(AGENT_DISCLOSURE_SLOT))
                        .h(px(AGENT_DISCLOSURE_SLOT))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .children(type_icon),
                )
            })
            .child(
                div()
                    .flex_none()
                    .max_w(relative(0.6))
                    .truncate()
                    .text_sm()
                    .text_color(label_color)
                    .child(self.label),
            )
            .children(self.preview.map(|preview| {
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground.opacity(0.55))
                    .child(preview)
            }))
            .child(
                div()
                    .w(px(AGENT_DISCLOSURE_SLOT))
                    .h(px(AGENT_DISCLOSURE_SLOT))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .children(self.trailing),
            )
    }
}

/// One transcript row for the virtualized list. `PartialEq` powers the
/// render-time diff: kind + indices catch structural changes (fold, collapse,
/// appended rows), the fingerprint catches in-place content changes that move
/// a row's height (streamed text, status flips, detail expansion).
#[derive(Clone, PartialEq, Eq)]
pub(super) enum RowSpec {
    Entry {
        index: usize,
        fingerprint: u64,
    },
    Work {
        index: usize,
        fingerprint: u64,
    },
    FoldHeader {
        turn: u64,
        seconds: u64,
        folded: bool,
    },
    RunToggle {
        run_start: usize,
        tool_count: usize,
        expanded: bool,
    },
    /// The live progress line. `compacting` is part of the spec because the
    /// compaction form is a different, taller row, so flipping it has to
    /// remeasure rather than only repaint.
    Working {
        compacting: bool,
    },
}

/// Height-relevant signature of a transcript entry. Lengths and small fields
/// instead of hashing full text: O(1) per row per frame, and every real
/// mutation (streamed append, status transition, exit code, expansion) moves
/// at least one component.
pub(super) fn entry_fingerprint(item: &SessionItem, detail_expanded: bool) -> u64 {
    let (content_len, status_len, extra) = match item {
        SessionItem::UserMessage { text }
        | SessionItem::AgentMessage { text, .. }
        | SessionItem::Reasoning { summary: text, .. } => {
            (text.as_ref().map_or(0, String::len), 0, 0)
        }
        SessionItem::Error { text } => (text.len(), 0, 0),
        SessionItem::CommandExecution {
            command,
            aggregated_output,
            status,
            exit_code,
            ..
        } => (
            command.len() + aggregated_output.as_ref().map_or(0, String::len),
            status.as_ref().map_or(0, String::len),
            exit_code.map_or(0, |code| (code as u64) ^ (1 << 20)),
        ),
        SessionItem::FileChange {
            paths,
            diff,
            status,
            ..
        } => (
            paths.len() + diff.as_ref().map_or(0, String::len),
            status.as_ref().map_or(0, String::len),
            0,
        ),
        SessionItem::Other {
            kind,
            title,
            output,
            status,
            ..
        } => (
            kind.len() + title.len() + output.as_ref().map_or(0, String::len),
            status.as_ref().map_or(0, String::len),
            0,
        ),
        SessionItem::Compaction { detail, .. } => (
            detail.summary.as_ref().map_or(0, String::len),
            compaction_accounting(detail).len(),
            detail.user_context.as_ref().map_or(0, String::len) as u64,
        ),
    };

    (content_len as u64)
        ^ ((status_len as u64) << 48)
        ^ (extra << 24)
        ^ ((detail_expanded as u64) << 63)
}

impl AgentPane {
    pub(super) fn transcript_has_hidden_content_below(&self) -> bool {
        should_show_jump_to_latest(
            self.transcript_list.is_following_tail(),
            self.transcript_list.is_scrolled_to_end(),
            self.transcript_list.max_offset_for_scrollbar().y,
        )
    }

    pub(super) fn transcript_has_hidden_content_above(&self) -> bool {
        let logical = self.transcript_list.logical_scroll_top();
        let has_logical_offset = logical.item_ix > 0 || logical.offset_in_item > px(0.);
        // A short bottom-aligned list uses an end anchor even though no content
        // is clipped. The pixel offset distinguishes that alignment sentinel
        // from a genuine logical offset into earlier rows.
        has_logical_offset && self.transcript_list.scroll_px_offset_for_scrollbar().y < px(0.)
    }

    /// Re-engaging tail mode also scrolls to the very end (past the last
    /// item), which stays correct while the last row is still growing.
    pub(super) fn scroll_transcript_to_bottom(&self) {
        self.transcript_list.set_follow_mode(FollowMode::Tail);
    }

    pub(super) fn push(&mut self, item: SessionItem, cx: &mut Context<Self>) {
        // Submitting a user message explicitly returns to the live tail.
        // Agent output preserves a manually chosen reading position via the
        // list's own tail-follow state.
        if matches!(&item, SessionItem::UserMessage { .. }) {
            self.scroll_transcript_to_bottom();
        }

        self.items.push(Entry {
            at: Local::now().format("%H:%M").to_string(),
            turn: self.turn_seq,
            item,
        });
        cx.notify();
    }

    /// Render one turn: user rows, then (for settled turns) a clickable
    /// "Worked for Ns" fold header hiding the intermediate work rows by
    /// default, then the final reply. Running turns render chronologically.
    pub(super) fn entry_spec(&self, index: usize) -> RowSpec {
        RowSpec::Entry {
            index,
            fingerprint: entry_fingerprint(
                &self.items[index].item,
                self.expanded_rows.contains(&index),
            ),
        }
    }

    pub(super) fn work_spec(&self, index: usize) -> RowSpec {
        RowSpec::Work {
            index,
            fingerprint: entry_fingerprint(
                &self.items[index].item,
                self.expanded_rows.contains(&index),
            ),
        }
    }

    /// Data-only description of every transcript row, in render order. This
    /// is the single source of truth for the transcript's structure; the
    /// virtualized list builds elements only for the visible slice of it.
    pub(super) fn build_row_specs(&self, collapse: bool) -> Vec<RowSpec> {
        let mut rows = Vec::new();
        let mut start = 0;

        while start < self.items.len() {
            let turn = self.items[start].turn;
            let mut end = start + 1;

            while end < self.items.len() && self.items[end].turn == turn {
                end += 1;
            }

            self.turn_specs(turn, start, end, collapse, &mut rows);
            start = end;
        }

        // Live progress row, pinned below everything the running turn has
        // produced; replaced by the turn's fold header on completion.
        if self.working_started.is_some() {
            rows.push(RowSpec::Working {
                compacting: self.compacting,
            });
        }

        rows
    }

    pub(super) fn turn_specs(
        &self,
        turn: u64,
        start: usize,
        end: usize,
        collapse: bool,
        rows: &mut Vec<RowSpec>,
    ) {
        let Some(&seconds) = self.completed_turn_seconds.get(&turn) else {
            // Running (or pre-thread) turn: plain chronological stream.
            self.stream_specs(start, end, &|_| false, collapse, rows);
            return;
        };

        let folded = !self.expanded_turns.contains(&turn);

        // The final reply stays visible when the turn folds; everything
        // between the prompt and the answer is what the fold hides.
        let final_agent = (start..end).rev().find(|&i| {
            matches!(&self.items[i].item, SessionItem::AgentMessage { .. })
                && !hidden(&self.items[i].item)
        });

        for i in start..end {
            if matches!(&self.items[i].item, SessionItem::UserMessage { .. }) {
                rows.push(self.entry_spec(i));
            }
        }

        rows.push(RowSpec::FoldHeader {
            turn,
            seconds,
            folded,
        });

        if folded {
            // Errors and compaction boundaries stay visible inside a folded
            // turn: an error is what the user needs to act on, and a boundary
            // marks where the conversation above it stopped being verbatim.
            for i in start..end {
                if matches!(
                    &self.items[i].item,
                    SessionItem::Error { .. } | SessionItem::Compaction { .. }
                ) {
                    rows.push(self.entry_spec(i));
                }
            }
        } else {
            let skip = |i: usize| {
                Some(i) == final_agent
                    || matches!(&self.items[i].item, SessionItem::UserMessage { .. })
            };
            self.stream_specs(start, end, &skip, collapse, rows);
        }

        if let Some(i) = final_agent {
            rows.push(self.entry_spec(i));
        }
    }

    /// Chronological rows for a slice of the transcript, collapsing runs of
    /// consecutive work-log rows into a "+N tool calls" toggle (when the
    /// collapse setting is on). Hidden entries are transparent: they neither
    /// render nor split a run.
    pub(super) fn stream_specs(
        &self,
        start: usize,
        end: usize,
        skip: &dyn Fn(usize) -> bool,
        collapse: bool,
        rows: &mut Vec<RowSpec>,
    ) {
        let mut i = start;

        while i < end {
            let item = &self.items[i].item;

            if skip(i) || hidden(item) {
                i += 1;
                continue;
            }

            if !is_work_row(item) {
                rows.push(self.entry_spec(i));
                i += 1;
                continue;
            }

            // Extend the run across consecutive (possibly hidden) work rows.
            let run_start = i;
            let mut visible: Vec<usize> = Vec::new();
            let mut j = i;

            while j < end
                && !skip(j)
                && (hidden(&self.items[j].item) || is_work_row(&self.items[j].item))
            {
                if !hidden(&self.items[j].item) {
                    visible.push(j);
                }
                j += 1;
            }

            if collapse && visible.len() > 1 {
                let expanded = self.expanded_groups.contains(&run_start);

                rows.push(RowSpec::RunToggle {
                    run_start,
                    tool_count: visible.len(),
                    expanded,
                });

                if expanded {
                    for &k in &visible {
                        rows.push(self.work_spec(k));
                    }
                }
            } else {
                for &k in &visible {
                    rows.push(self.work_spec(k));
                }
            }

            i = j;
        }
    }

    /// Diff freshly built specs against the list's current contents and
    /// notify it as narrowly as possible: an equal-count middle means rows
    /// changed in place (streaming growth, expansion) and only needs
    /// remeasuring, which preserves the scroll position exactly; a count
    /// change is a real splice.
    pub(super) fn sync_transcript_list(&mut self, new: Vec<RowSpec>) {
        if self.row_specs == new {
            return;
        }

        let prefix = self
            .row_specs
            .iter()
            .zip(&new)
            .take_while(|(a, b)| a == b)
            .count();
        let suffix = self.row_specs[prefix..]
            .iter()
            .rev()
            .zip(new[prefix..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        let old_mid = prefix..self.row_specs.len() - suffix;
        let new_mid = new.len() - suffix - prefix;

        if old_mid.len() == new_mid {
            self.transcript_list.remeasure_items(old_mid);
        } else {
            self.transcript_list.splice(old_mid, new_mid);
        }

        self.row_specs = new;
    }

    /// Build the element for one visible row. Row indices come from the list
    /// element during layout/paint, resolved through the spec snapshot taken
    /// in the current render pass.
    pub(super) fn render_row(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(spec) = self.row_specs.get(ix).cloned() else {
            return div().into_any_element();
        };

        let row = match spec {
            RowSpec::Entry { index, .. } => self.render_entry_row(index, window, cx),
            RowSpec::Work { index, .. } => self.render_work_row(index, window, cx),
            RowSpec::FoldHeader {
                turn,
                seconds,
                folded,
            } => self.render_fold_header(turn, seconds, folded, cx),
            RowSpec::RunToggle {
                run_start,
                tool_count,
                expanded,
            } => self.render_run_toggle(run_start, tool_count, expanded, cx),
            RowSpec::Working { compacting } => self.render_working_row(compacting, cx),
        };

        // The pre-virtualization container spaced rows with `gap_2` inside a
        // `p_3` body; each row now carries its own horizontal inset and
        // bottom gap.
        div().w_full().px_3().pb_2().child(row).into_any_element()
    }

    /// The live progress line. While the backend is compacting it names that
    /// explicitly and spins: compaction produces no streamed output, so a bare
    /// seconds counter would read as a hung turn for as long as a minute.
    pub(super) fn render_working_row(
        &self,
        compacting: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(started) = self.working_started else {
            return div().into_any_element();
        };

        if compacting {
            let accent = cx.theme().info;

            return h_flex()
                .w_full()
                .gap_2()
                .items_center()
                .px_1()
                .child(
                    Spinner::new()
                        .icon(IconName::LoaderCircle)
                        .with_size(px(12.))
                        .color(accent),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(accent)
                        .child("Compacting context…"),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground.opacity(0.55))
                        .child(working_label(started)),
                )
                .into_any_element();
        }

        h_flex()
            .w_full()
            .gap_2()
            .items_center()
            .px_1()
            .child(h_flex().gap_1().children((0..3).map(|i| {
                div()
                    .size(px(4.))
                    .rounded_full()
                    .bg(cx.theme().muted_foreground.opacity(0.85 - 0.28 * i as f32))
            })))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground.opacity(0.7))
                    .child(working_label(started)),
            )
            .into_any_element()
    }

    pub(super) fn render_entry_row(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let entry = &self.items[index];

        match &entry.item {
            SessionItem::UserMessage { text: Some(text) } => self.render_user_row(index, text, cx),
            SessionItem::AgentMessage {
                text: Some(text), ..
            } => self.render_agent_row(index, text.clone(), cx),
            SessionItem::Error { text } => self.render_error_row(index, text.clone(), cx),
            SessionItem::Compaction { detail, .. } => {
                let detail = detail.clone();

                self.render_compaction_row(index, detail, window, cx)
            }
            item if is_work_row(item) => self.render_work_row(index, window, cx),
            _ => div().into_any_element(),
        }
    }

    /// Hover-revealed timestamp; the row declares `.group("entry")`.
    pub(super) fn hover_stamp(&self, index: usize, cx: &mut Context<Self>) -> Div {
        div()
            .flex_none()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .invisible()
            .group_hover("entry", |this| this.visible())
            .child(self.items[index].at.clone())
    }

    pub(super) fn copy_menu(
        pane: gpui::WeakEntity<Self>,
        index: usize,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        move |menu, _, cx| {
            // Full transcript payloads can be very large. Resolve and clone the
            // text only after a right click opens the menu, keeping ordinary
            // list layout independent of the hidden message size.
            let copy_text = pane
                .read_with(cx, |pane, _| {
                    pane.items
                        .get(index)
                        .map(|entry| entry_copy_text(&entry.item))
                })
                .ok()
                .flatten();

            match copy_text {
                Some(copy_text) => {
                    menu.item(PopupMenuItem::new("Copy").on_click(move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copy_text.clone()));
                    }))
                }
                None => menu,
            }
        }
    }

    /// User prompt: right-aligned quiet bubble (muted surface, no border).
    ///
    /// Oversized prompts (huge pastes) collapse to their head by default:
    /// a visible row re-lays-out its full text every frame, so an unbounded
    /// prompt would make every frame O(paste size). Expansion is an explicit
    /// per-row choice, and the right-click Copy always carries the full text.
    pub(super) fn render_user_row(
        &self,
        index: usize,
        text: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let head_len = truncated_user_prompt(text).map(str::len);
        let expanded = head_len.is_some() && self.expanded_rows.contains(&index);
        let shown = match (head_len, expanded) {
            (Some(len), false) => text[..len].to_string(),
            _ => text.to_string(),
        };
        let toggle = head_len.is_some().then(|| {
            div()
                .mt_1()
                .text_xs()
                .text_color(cx.theme().primary)
                .cursor_pointer()
                .child(if expanded {
                    "Show less".to_string()
                } else {
                    "Show full message".to_string()
                })
                .id(("user-expand", index))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !this.expanded_rows.insert(index) {
                        this.expanded_rows.remove(&index);
                    }
                    cx.notify();
                }))
        });

        h_flex()
            .id(("entry", index))
            .group("entry")
            .w_full()
            .justify_end()
            .items_end()
            .gap_2()
            .context_menu(Self::copy_menu(cx.entity().downgrade(), index))
            .child(self.hover_stamp(index, cx))
            .child(
                div()
                    .max_w(relative(0.8))
                    .px_3()
                    .py_2()
                    .rounded(UI_RADIUS)
                    .bg(cx.theme().muted)
                    // Plain, not markdown: the prompt is user-authored text and
                    // must render verbatim, but stays drag-selectable.
                    .child(text::TextView::plain(("user-text", index), shown).selectable(true))
                    .children(toggle),
            )
            .into_any_element()
    }

    /// Assistant reply: full-width bare markdown — no bubble, no border;
    /// alignment and surface carry the distinction.
    fn markdown_view(
        id: impl Into<ElementId>,
        markdown: impl Into<SharedString>,
        cwd: Option<String>,
    ) -> text::TextView {
        text::TextView::markdown(id, markdown).on_link_click(move |target, _, cx| {
            links::open(target, cwd.as_deref().map(Path::new), cx);
        })
    }

    pub(super) fn render_agent_row(
        &self,
        index: usize,
        text: String,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        h_flex()
            .id(("entry", index))
            .group("entry")
            .w_full()
            .items_end()
            .gap_2()
            .context_menu(Self::copy_menu(cx.entity().downgrade(), index))
            .child(
                div().flex_1().min_w_0().px_1().child(
                    Self::markdown_view(("agent-md", index), text, self.cwd.clone())
                        // Monospaced glyphs average roughly 0.6em wide, so
                        // 48rem yields an approximately 80-character prose
                        // measure while technical Markdown remains full-width.
                        .style(
                            TextViewStyle::default().prose_max_width(rems(AGENT_TEXT_MEASURE_REMS)),
                        )
                        .selectable(true),
                ),
            )
            .child(self.hover_stamp(index, cx))
            .into_any_element()
    }

    pub(super) fn render_error_row(
        &self,
        index: usize,
        text: String,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        h_flex()
            .id(("entry", index))
            .w_full()
            .context_menu(Self::copy_menu(cx.entity().downgrade(), index))
            .child(
                div()
                    .max_w(relative(0.9))
                    .px_3()
                    .py_2()
                    .rounded(UI_RADIUS)
                    .bg(cx.theme().danger.opacity(0.15))
                    .text_color(cx.theme().danger)
                    .text_sm()
                    .child(text),
            )
            .into_any_element()
    }

    /// One work-log line: chevron · type icon · heading · status.
    /// Rows with detail (command output, reasoning text) expand on click into
    /// a bounded transcript surface with its own scroll position.
    pub(super) fn render_work_row(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let cwd = self.cwd.clone();
        let (icon, heading, status, detail) = match &self.items[index].item {
            SessionItem::CommandExecution {
                command,
                aggregated_output,
                status,
                exit_code,
                ..
            } => {
                // Belt and braces: a non-zero exit code is a failure even if
                // the provider reported the execution as completed.
                let state = status.as_deref().unwrap_or("inProgress");
                let failed = matches!(state, "failed" | "declined")
                    || exit_code.is_some_and(|code| code != 0);
                let state = if failed { "failed" } else { state };
                let detail = command_execution_detail(command, aggregated_output.as_deref());

                (
                    IconName::SquareTerminal,
                    COMMAND_EXECUTION_HEADING.to_string(),
                    Some(state.to_string()),
                    Some(Cow::Owned(detail)),
                )
            }
            SessionItem::FileChange {
                paths,
                diff,
                status,
                ..
            } => (
                IconName::File,
                format!("Edit {paths}"),
                Some(status.as_deref().unwrap_or("inProgress").to_string()),
                diff.as_deref()
                    .filter(|diff| !diff.trim().is_empty())
                    .map(Cow::Borrowed),
            ),
            SessionItem::Other {
                kind,
                title,
                output,
                status,
                ..
            } => (
                if kind == "webSearch" {
                    IconName::Globe
                } else {
                    IconName::Settings2
                },
                if title.trim().is_empty() {
                    kind.clone()
                } else {
                    format!("{kind} {title}")
                },
                Some(status.as_deref().unwrap_or("inProgress").to_string()),
                output
                    .as_deref()
                    .filter(|output| !output.trim().is_empty())
                    .map(Cow::Borrowed),
            ),
            SessionItem::Reasoning { summary, .. } => (
                IconName::Bot,
                "Thinking".to_string(),
                None,
                summary
                    .as_deref()
                    .filter(|text| !text.trim().is_empty())
                    .map(Cow::Borrowed),
            ),
            _ => return div().into_any_element(),
        };

        let expandable = detail.is_some();
        let expanded = expandable && self.expanded_rows.contains(&index);
        let status_label = status.as_deref().unwrap_or("No status");
        let status_glyph = status.as_ref().map(|state| {
            let (name, color) = match state.as_str() {
                "failed" | "declined" => (IconName::CircleX, cx.theme().danger),
                "completed" => (IconName::Check, cx.theme().muted_foreground),
                _ => (IconName::Minus, cx.theme().muted_foreground.opacity(0.6)),
            };

            Icon::new(name)
                .size_3()
                .text_color(color)
                .into_any_element()
        });

        let accessible_label = format!(
            "{}. {}{}",
            heading,
            status_label,
            if expandable {
                if expanded {
                    ". Expanded"
                } else {
                    ". Collapsed"
                }
            } else {
                ""
            }
        );
        let mut header = AgentDisclosureRow::new(("wl-head", index), heading)
            .type_icon(icon)
            .trailing(status_glyph)
            .accessible_label(accessible_label);
        if expandable {
            header = header.expanded(expanded);
        }
        let header = header.render(cx).when(expandable, |this| {
            this.on_click(cx.listener(move |this, _, _, cx| {
                if !this.expanded_rows.insert(index) {
                    this.expanded_rows.remove(&index);
                    this.virtual_transcripts.remove(&index);
                }
                cx.notify();
            }))
        });

        v_flex()
            .id(("entry", index))
            .w_full()
            .context_menu(Self::copy_menu(cx.entity().downgrade(), index))
            .child(header)
            .children(detail.as_deref().filter(|_| expanded).map(|detail| {
                let code_format = code_transcript_format(&self.items[index].item, detail);
                let virtualized = should_virtualize_transcript(code_format.is_some(), detail);

                let body = if virtualized {
                    let (_, strip_gutter) = code_format.as_ref().expect("code format");
                    let (source, segments, widest_segment, scroll) = {
                        let state = self
                            .virtual_transcripts
                            .entry(index)
                            .or_insert_with(|| VirtualTranscriptState::new(detail, *strip_gutter));
                        state.sync(detail, *strip_gutter);
                        (
                            state.source.clone(),
                            state.segments.clone(),
                            state.widest_segment,
                            state.scroll.clone(),
                        )
                    };
                    let segment_count = segments.len();
                    let line_height = px(f32::from(cx.theme().mono_font_size) * 1.5);
                    let text_color = cx.theme().muted_foreground;
                    let list_source = source.clone();
                    let list_segments = segments.clone();

                    div()
                        .id(("wl-out", index))
                        .w_full()
                        .h(px(256.))
                        .relative()
                        .overflow_hidden()
                        .rounded(UI_RADIUS)
                        .bg(cx.theme().tokens.muted)
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_size(cx.theme().mono_font_size)
                        // The outer transcript list is behind this viewport in
                        // hit-test order. Occlusion prevents wheel input at the
                        // virtual list's limits from moving the conversation.
                        .occlude()
                        .context_menu(Self::copy_menu(cx.entity().downgrade(), index))
                        .child(
                            uniform_list(
                                SharedString::from(format!("wl-virtual-{index}")),
                                segment_count,
                                move |range, _, _| {
                                    range
                                        .filter_map(|segment_index| {
                                            let range = list_segments.get(segment_index)?.clone();
                                            Some(
                                                div()
                                                    .h(line_height)
                                                    .flex_none()
                                                    .line_height(line_height)
                                                    .whitespace_nowrap()
                                                    .text_color(text_color)
                                                    .child(list_source[range].to_string()),
                                            )
                                        })
                                        .collect::<Vec<_>>()
                                },
                            )
                            .with_width_from_item(Some(widest_segment))
                            .with_horizontal_sizing_behavior(
                                ListHorizontalSizingBehavior::Unconstrained,
                            )
                            .track_scroll(&scroll)
                            .size_full()
                            .p_3(),
                        )
                        .child(
                            div()
                                .absolute()
                                .top_0()
                                .right_0()
                                .bottom_0()
                                .w(px(16.0))
                                .child(
                                    Scrollbar::vertical(&scroll)
                                        .id(("wl-virtual-v-scrollbar", index))
                                        .scrollbar_show(ScrollbarShow::Always),
                                ),
                        )
                        .child(
                            div()
                                .absolute()
                                .left_0()
                                .right_0()
                                .bottom_0()
                                .h(px(16.0))
                                .child(
                                    Scrollbar::horizontal(&scroll)
                                        .id(("wl-virtual-h-scrollbar", index))
                                        .scrollbar_show(ScrollbarShow::Always),
                                ),
                        )
                        .into_any_element()
                } else {
                    self.virtual_transcripts.remove(&index);

                    // Small technical transcripts retain syntax highlighting;
                    // Markdown-native tool output and reasoning retain their
                    // rich formatting. The expensive fence and gutter copy are
                    // therefore bounded by the virtualization thresholds.
                    let markdown = match code_format {
                        Some((language, strip_gutter)) => {
                            let normalized = if strip_gutter {
                                strip_read_gutter(detail)
                                    .map(Cow::Owned)
                                    .unwrap_or(Cow::Borrowed(detail))
                            } else {
                                Cow::Borrowed(detail)
                            };
                            fenced_code_block_as(&normalized, language.as_ref())
                        }
                        None => detail.to_owned(),
                    };
                    let detail_scroll = window
                        .use_keyed_state(("wl-scroll", index), cx, |_, _| ScrollHandle::default())
                        .read(cx)
                        .clone();

                    div()
                        .w_full()
                        .relative()
                        .child(
                            div()
                                .id(("wl-out", index))
                                .w_full()
                                .max_h(px(256.))
                                .overflow_y_scroll()
                                .track_scroll(&detail_scroll)
                                // The virtual conversation list handles wheel input
                                // before child bubble listeners run. Occluding its
                                // earlier hitbox makes this viewport the only scroll
                                // target under the pointer, even at either limit.
                                .occlude()
                                .context_menu(Self::copy_menu(cx.entity().downgrade(), index))
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(
                                    Self::markdown_view(("wl-md", index), markdown, cwd.clone())
                                        .selectable(true),
                                ),
                        )
                        .child(
                            div()
                                .absolute()
                                .top_0()
                                .right_0()
                                .bottom_0()
                                .w(px(16.0))
                                .child(
                                    Scrollbar::vertical(&detail_scroll)
                                        .id(("wl-scrollbar", index))
                                        .scrollbar_show(ScrollbarShow::Always),
                                ),
                        )
                        .into_any_element()
                };

                div()
                    // Expanded transcript content starts at the same horizontal
                    // position as its title; inner padding would add a redundant
                    // indentation level inside an already grouped tool call.
                    .ml(px(AGENT_DISCLOSURE_DETAIL_INSET))
                    .mt_1()
                    // Expanded technical content uses the same readable
                    // measure as assistant prose instead of stretching across
                    // the remaining window width.
                    .max_w(rems(AGENT_TEXT_MEASURE_REMS))
                    .relative()
                    .child(body)
            }))
            .into_any_element()
    }

    /// A context-compaction boundary. Rendered as a divider rather than a card
    /// because it is a structural break in the conversation: the rows above it
    /// are no longer what the model sees. Expanding reveals the accounting and
    /// the summary the thread continued from when the provider exposes it.
    pub(super) fn render_compaction_row(
        &self,
        index: usize,
        detail: Compaction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let label = compaction_label(&detail);
        let preview = compaction_accounting(&detail).join(" · ");
        let expandable = compaction_row_is_expandable(self.kind);
        let expanded = expandable && self.expanded_rows.contains(&index);
        let accent = cx.theme().info;

        let mut header_row = AgentDisclosureRow::new(("compaction-head", index), label)
            .type_icon(IconName::Minimize)
            .preview(preview.clone())
            .accent(accent);
        if expandable {
            header_row = header_row.expanded(expanded);
        }
        let accessible_label = if expandable {
            format!(
                "{label}. {preview}. {}",
                if expanded { "Expanded" } else { "Collapsed" }
            )
        } else {
            format!("{label}. {preview}")
        };
        let mut header = header_row.accessible_label(accessible_label).render(cx);
        if expandable {
            header = header.on_click(cx.listener(move |this, _, _, cx| {
                if !this.expanded_rows.insert(index) {
                    this.expanded_rows.remove(&index);
                }
                cx.notify();
            }));
        }

        v_flex()
            .id(("entry", index))
            .w_full()
            .gap_1()
            .context_menu(Self::copy_menu(cx.entity().downgrade(), index))
            // The rule sits above the heading: it closes off the conversation
            // that the summary below replaced.
            .child(div().w_full().h(px(1.)).bg(accent.opacity(0.35)))
            .child(header)
            .children(expanded.then(|| self.render_compaction_detail(index, &detail, window, cx)))
            .into_any_element()
    }

    /// Expanded compaction body: the accounting as labelled rows, the manual
    /// instructions when the user gave any, and the summary itself in a bounded
    /// scroll surface (summaries routinely run to several kilobytes).
    pub(super) fn render_compaction_detail(
        &self,
        index: usize,
        detail: &Compaction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let accounting_rows = [
            ("Before", detail.pre_tokens.map(compact_token_count)),
            ("After", detail.post_tokens.map(compact_token_count)),
            (
                "Messages summarized",
                detail.messages_summarized.map(|count| count.to_string()),
            ),
            (
                "Trigger",
                detail.trigger.map(|trigger| trigger.label().to_string()),
            ),
            ("Instructions", detail.user_context.clone()),
        ];
        let summary_scroll = window
            .use_keyed_state(("compaction-scroll", index), cx, |_, _| {
                ScrollHandle::default()
            })
            .read(cx)
            .clone();

        v_flex()
            .ml(px(AGENT_DISCLOSURE_DETAIL_INSET))
            .max_w(rems(AGENT_TEXT_MEASURE_REMS))
            .gap_1()
            .text_xs()
            .children(accounting_rows.into_iter().filter_map(|(name, value)| {
                let value = value?;

                Some(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .items_start()
                        .child(
                            div()
                                .flex_none()
                                .w(px(148.))
                                .text_color(cx.theme().muted_foreground.opacity(0.6))
                                .child(name),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_color(cx.theme().muted_foreground)
                                .child(value),
                        ),
                )
            }))
            .children(detail.summary.clone().map(|summary| {
                div()
                    .w_full()
                    .mt_1()
                    .relative()
                    .child(
                        div()
                            .id(("compaction-summary", index))
                            .w_full()
                            .max_h(px(320.))
                            .overflow_y_scroll()
                            .track_scroll(&summary_scroll)
                            // The virtual conversation list handles wheel input
                            // before child listeners run. Occluding its earlier
                            // hitbox makes this the only scroll target under the
                            // pointer, even at either limit.
                            .occlude()
                            .context_menu(Self::copy_menu(cx.entity().downgrade(), index))
                            .px_3()
                            .py_2()
                            .rounded(UI_RADIUS)
                            .bg(cx.theme().tokens.muted)
                            .text_color(cx.theme().muted_foreground)
                            .child(
                                Self::markdown_view(
                                    ("compaction-md", index),
                                    summary,
                                    self.cwd.clone(),
                                )
                                .selectable(true),
                            ),
                    )
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .right_0()
                            .bottom_0()
                            .w(px(16.0))
                            .child(
                                Scrollbar::vertical(&summary_scroll)
                                    .id(("compaction-scrollbar", index))
                                    .scrollbar_show(ScrollbarShow::Always),
                            ),
                    )
            }))
            .into_any_element()
    }

    /// The settled turn's "Worked for Ns" disclosure line, doubling as a
    /// section divider (bottom hairline).
    pub(super) fn render_fold_header(
        &self,
        turn: u64,
        seconds: u64,
        folded: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let label = format!("Worked for {seconds}s");

        v_flex()
            .w_full()
            .gap_1()
            .child(
                AgentDisclosureRow::new(("turn-fold", turn as usize), label.clone())
                    .expanded(!folded)
                    .accessible_label(format!(
                        "{label}. {}",
                        if folded { "Collapsed" } else { "Expanded" }
                    ))
                    .render(cx)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.expanded_turns.insert(turn) {
                            this.expanded_turns.remove(&turn);
                        }
                        cx.notify();
                    })),
            )
            .child(div().w_full().h(px(1.)).bg(cx.theme().border.opacity(0.6)))
            .into_any_element()
    }

    /// The "+N tool calls" / "Show fewer tool calls" toggle for a work run.
    pub(super) fn render_run_toggle(
        &self,
        run_start: usize,
        tool_count: usize,
        expanded: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let label = if expanded {
            "Show fewer tool calls".to_string()
        } else {
            format!("+{tool_count} tool calls")
        };

        AgentDisclosureRow::new(("wl-run", run_start), label.clone())
            .expanded(expanded)
            .without_type_icon_slot()
            .accessible_label(format!(
                "{label}. {}",
                if expanded { "Expanded" } else { "Collapsed" }
            ))
            .render(cx)
            .on_click(cx.listener(move |this, _, _, cx| {
                if !this.expanded_groups.insert(run_start) {
                    this.expanded_groups.remove(&run_start);
                }
                cx.notify();
            }))
            .into_any_element()
    }
}

#[cfg(test)]
mod prompt_truncation_tests {
    use gpui::px;
    use nmt_agent_utils::chat::{Compaction, CompactionTrigger, Item as SessionItem};

    use super::{
        AGENT_DISCLOSURE_DETAIL_INSET, AGENT_DISCLOSURE_GAP, AGENT_DISCLOSURE_PADDING,
        AGENT_DISCLOSURE_SLOT, AgentKind, COMMAND_EXECUTION_HEADING, Status,
        VIRTUAL_TRANSCRIPT_MAX_SEGMENT_BYTES, command_execution_detail, compaction_accounting,
        compaction_label, compaction_row_is_expandable, entry_copy_text, is_work_row,
        should_show_jump_to_latest, should_virtualize_transcript, transcript_segments,
        truncated_user_prompt,
    };
    use crate::agent_pane::composer::{ComposerAction, composer_action};

    #[test]
    fn disclosure_detail_matches_the_title_start() {
        assert_eq!(
            AGENT_DISCLOSURE_DETAIL_INSET,
            AGENT_DISCLOSURE_PADDING + AGENT_DISCLOSURE_SLOT * 2.0 + AGENT_DISCLOSURE_GAP * 2.0
        );
    }

    #[test]
    fn composer_replaces_send_with_stop_only_while_running() {
        assert_eq!(composer_action(Status::Running), ComposerAction::Stop);
        for status in [Status::Starting, Status::Idle, Status::Exited] {
            assert_eq!(composer_action(status), ComposerAction::Send);
        }
    }

    #[test]
    fn jump_to_latest_requires_hidden_content_below_the_viewport() {
        assert!(!should_show_jump_to_latest(false, None, px(0.)));
        assert!(!should_show_jump_to_latest(false, Some(true), px(200.)));
        assert!(should_show_jump_to_latest(false, Some(false), px(200.)));
        assert!(!should_show_jump_to_latest(true, Some(false), px(200.)));
        assert!(!should_show_jump_to_latest(true, None, px(200.)));
        assert!(should_show_jump_to_latest(false, None, px(200.)));
    }

    #[test]
    fn compaction_rows_name_the_trigger_and_report_only_known_numbers() {
        let full = Compaction {
            trigger: Some(CompactionTrigger::Automatic),
            pre_tokens: Some(154_000),
            post_tokens: Some(32_000),
            messages_summarized: Some(87),
            user_context: None,
            summary: None,
        };

        assert_eq!(compaction_label(&full), "Context auto-compacted");
        assert_eq!(
            compaction_accounting(&full),
            vec![
                "154k → 32k".to_string(),
                "122k freed".to_string(),
                "87 messages summarized".to_string(),
                "automatic".to_string(),
            ]
        );

        // A boundary the backend described only partially must not invent
        // zeroes for the fields it never reported.
        let sparse = Compaction {
            pre_tokens: Some(90_000),
            ..Compaction::default()
        };

        assert_eq!(compaction_label(&sparse), "Context compacted");
        assert_eq!(compaction_accounting(&sparse), vec!["from 90k".to_string()]);
        assert!(compaction_accounting(&Compaction::default()).is_empty());
    }

    #[test]
    fn a_compaction_row_is_a_divider_and_copies_its_summary() {
        let item = SessionItem::Compaction {
            id: "compaction-1".into(),
            detail: Compaction {
                trigger: Some(CompactionTrigger::Manual),
                pre_tokens: Some(120_000),
                post_tokens: Some(40_000),
                summary: Some("what happened so far".into()),
                ..Compaction::default()
            },
        };

        // Work rows collapse into "+N tool calls" runs; a structural
        // break must never be swallowed by one.
        assert!(!is_work_row(&item));
        assert_eq!(
            entry_copy_text(&item),
            "Context compacted\n120k → 40k · 80k freed · manual\n\nwhat happened so far"
        );
    }

    #[test]
    fn compaction_disclosure_matches_provider_capabilities() {
        assert!(!compaction_row_is_expandable(AgentKind::Codex));
        assert!(compaction_row_is_expandable(AgentKind::Claude));
    }

    #[test]
    fn command_tool_moves_the_full_command_and_output_into_detail() {
        assert_eq!(COMMAND_EXECUTION_HEADING, "Run Command");
        assert_eq!(
            command_execution_detail("cargo test --workspace", Some("running 42 tests\nok")),
            "$ cargo test --workspace\n\nrunning 42 tests\nok"
        );
        assert_eq!(
            command_execution_detail("cargo check", None),
            "$ cargo check"
        );
    }

    #[test]
    fn shared_tool_items_keep_transcript_details_intact() {
        let item = SessionItem::Other {
            id: "tool-1".into(),
            kind: "Read".into(),
            title: "src/lib.rs".into(),
            output: Some("contents".into()),
            status: Some("completed".into()),
        };

        let SessionItem::Other {
            id,
            kind,
            title,
            output,
            status,
        } = item
        else {
            panic!("expected a tool item");
        };
        assert_eq!(id, "tool-1");
        assert_eq!(kind, "Read");
        assert_eq!(title, "src/lib.rs");
        assert_eq!(output.as_deref(), Some("contents"));
        assert_eq!(status.as_deref(), Some("completed"));
    }

    #[test]
    fn short_prompts_pass_through_and_long_ones_cut_at_boundaries() {
        assert_eq!(truncated_user_prompt("hello\nworld"), None);

        let four_lines = "line\n".repeat(4);
        let head = truncated_user_prompt(&four_lines).expect("over the line cap");
        assert_eq!(head.lines().count(), 3);
        assert!(head.ends_with('\n'));

        let exact_char_cap = "x".repeat(512);
        assert_eq!(truncated_user_prompt(&exact_char_cap), None);

        let giant_line = "\u{4f60}".repeat(3000);
        let head = truncated_user_prompt(&giant_line).expect("over the character cap");
        assert_eq!(head.chars().count(), 512);
        assert!(giant_line.is_char_boundary(head.len()));
    }

    #[test]
    fn long_code_transcripts_switch_to_virtual_rows() {
        let many_rows = "output\n".repeat(129);
        let large_single_row = "x".repeat(16 * 1024);

        assert!(should_virtualize_transcript(true, &many_rows));
        assert!(should_virtualize_transcript(true, &large_single_row));
        assert!(!should_virtualize_transcript(true, "short output"));
        assert!(!should_virtualize_transcript(false, &many_rows));
    }

    #[test]
    fn virtual_transcript_segments_preserve_rows_and_utf8_boundaries() {
        let source = format!("alpha\r\n\n{}\nend", "你".repeat(2_000));
        let segments = transcript_segments(&source);

        assert_eq!(&source[segments[0].clone()], "alpha");
        assert_eq!(&source[segments[1].clone()], "");
        assert_eq!(&source[segments.last().expect("final row").clone()], "end");
        assert!(
            segments
                .iter()
                .all(|range| source.is_char_boundary(range.start)
                    && source.is_char_boundary(range.end)
                    && range.len() <= VIRTUAL_TRANSCRIPT_MAX_SEGMENT_BYTES)
        );
        assert!(segments.iter().filter(|range| range.len() > 0).count() > 3);
    }

    #[test]
    fn virtual_transcript_keeps_one_segment_per_short_logical_row() {
        let source = "row\n".repeat(10_000);
        let segments = transcript_segments(&source);

        assert_eq!(segments.len(), 10_000);
        assert!(segments.iter().all(|range| &source[range.clone()] == "row"));
    }
}

#[cfg(test)]
mod read_gutter_tests {
    use super::{file_extension_lang, strip_read_gutter};

    #[test]
    fn gutter_strips_only_when_every_line_matches() {
        assert_eq!(
            strip_read_gutter("     1\u{2192}fn main() {\n     2\u{2192}}").as_deref(),
            Some("fn main() {\n}\n")
        );
        assert_eq!(strip_read_gutter("plain output"), None);
        assert_eq!(strip_read_gutter("     1\u{2192}ok\nno gutter"), None);
    }

    #[test]
    fn extension_is_the_language_tag() {
        assert_eq!(file_extension_lang("C:\\src\\main.RS"), "rs");
        assert_eq!(file_extension_lang("noext"), "");
    }
}

#[cfg(test)]
mod fence_tests {
    use super::{detect_output_language, fenced_code_block_as};

    #[test]
    fn fence_outgrows_backtick_runs_and_sniffs_language() {
        assert_eq!(
            fenced_code_block_as("plain output", detect_output_language("plain output")),
            "```\nplain output\n```"
        );
        assert_eq!(
            fenced_code_block_as("{\"key\": 1}", detect_output_language("{\"key\": 1}")),
            "```json\n{\"key\": 1}\n```"
        );
        assert_eq!(detect_output_language("diff --git a/x b/x"), "diff");

        let tricky = "text with ```` four backticks";
        let fenced = fenced_code_block_as(tricky, "");
        assert!(
            fenced.starts_with("`````\n"),
            "fence must outgrow body runs"
        );
        assert!(fenced.ends_with("\n`````"));
    }
}
