//! Backend-neutral model for child agents ("background tasks") that a parent
//! agent session spawns. Codex reports descendants as threads, Claude Code
//! reports them as Task tool calls plus sidechain records; both reduce into the
//! same summary so the UI never parses a provider protocol.

use std::collections::HashMap;
use std::time::SystemTime;

mod transcript;

pub use crate::background_task::transcript::{
    BackgroundTaskTranscript, BackgroundTaskTranscriptState, BackgroundTaskTranscriptUpdate,
    MAX_TRANSCRIPT_ITEMS,
};

/// Which agent backend owns a task. Two providers can emit the same local id
/// string, so every identity in this module is qualified by the provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum BackgroundTaskProvider {
    Codex,
    ClaudeCode,
}

impl BackgroundTaskProvider {
    pub fn label(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::ClaudeCode => "Claude Code",
        }
    }
}

/// A provider plus a provider-local stable id. Used both for a child task and
/// for the parent session that owns it, because both need the same
/// qualification to stay distinct across simultaneously open providers.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BackgroundTaskKey {
    pub provider: BackgroundTaskProvider,
    pub id: String,
}

impl BackgroundTaskKey {
    pub fn new(provider: BackgroundTaskProvider, id: impl Into<String>) -> Self {
        Self {
            provider,
            id: id.into(),
        }
    }

    pub fn codex(id: impl Into<String>) -> Self {
        Self::new(BackgroundTaskProvider::Codex, id)
    }

    pub fn claude_code(id: impl Into<String>) -> Self {
        Self::new(BackgroundTaskProvider::ClaudeCode, id)
    }
}

/// Provider-specific identifiers kept beside the shared summary. These live in
/// an enum rather than as unrelated optional fields so a Codex-only or
/// Claude-only identifier can never be read for the wrong provider.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BackgroundTaskRefs {
    Codex {
        thread_id: String,
        /// Immediate parent thread, which can be another descendant rather than
        /// the selected root; retained so a later version can nest rows.
        parent_thread_id: Option<String>,
    },
    ClaudeCode {
        /// Task identifier from lifecycle records; absent until one arrives.
        task_id: Option<String>,
        /// Assistant Task/Agent tool-use id that launched the child.
        tool_use_id: Option<String>,
        /// Agent identifier some notification records carry instead of a task id.
        agent_id: Option<String>,
    },
}

impl BackgroundTaskRefs {
    /// Fill identifiers this reference does not know yet. Known values are kept
    /// because a later record can omit an id it already established.
    fn merge_from(&mut self, other: &Self) {
        match (self, other) {
            (
                Self::Codex {
                    parent_thread_id, ..
                },
                Self::Codex {
                    parent_thread_id: incoming,
                    ..
                },
            ) => {
                if parent_thread_id.is_none() {
                    parent_thread_id.clone_from(incoming);
                }
            }
            (
                Self::ClaudeCode {
                    task_id,
                    tool_use_id,
                    agent_id,
                },
                Self::ClaudeCode {
                    task_id: incoming_task,
                    tool_use_id: incoming_tool_use,
                    agent_id: incoming_agent,
                },
            ) => {
                if task_id.is_none() {
                    task_id.clone_from(incoming_task);
                }
                if tool_use_id.is_none() {
                    tool_use_id.clone_from(incoming_tool_use);
                }
                if agent_id.is_none() {
                    agent_id.clone_from(incoming_agent);
                }
            }
            // A provider mismatch means the key was reused across providers,
            // which the qualified key already prevents; keep the current value.
            _ => {}
        }
    }
}

/// Shared lifecycle state. Provider states map onto these; the view groups
/// non-terminal states under Running and terminal states under Finished.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BackgroundTaskState {
    Starting,
    Working,
    NeedsInput,
    Done,
    Interrupted,
    Stopped,
    Failed,
}

impl BackgroundTaskState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Done | Self::Interrupted | Self::Stopped | Self::Failed
        )
    }

    /// True while the task is expected to make progress on its own. Needs Input
    /// is active but blocked, so it counts as active and is reported separately.
    pub fn is_active(self) -> bool {
        !self.is_terminal()
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Starting => "Starting",
            Self::Working => "Working",
            Self::NeedsInput => "Needs Input",
            Self::Done => "Done",
            Self::Interrupted => "Interrupted",
            Self::Stopped => "Stopped",
            Self::Failed => "Failed",
        }
    }
}

/// How far provider-specific restoration has progressed. Kept beside the rows
/// rather than encoded into them so a failed refresh can leave known rows
/// visible while still reporting the failure.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum BackgroundTaskDiscoveryState {
    #[default]
    NotLoaded,
    Loading,
    Ready,
    Unavailable {
        message: String,
    },
}

/// One child agent as the UI sees it. Optional fields stay absent when a
/// provider does not report them; a row is shown from its key and state alone.
#[derive(Clone, Debug, PartialEq)]
pub struct BackgroundTaskSummary {
    pub key: BackgroundTaskKey,
    pub parent_session: BackgroundTaskKey,
    pub refs: BackgroundTaskRefs,
    pub display_name: Option<String>,
    pub agent_type: Option<String>,
    /// What the child was asked to do, from the launch payload.
    pub objective: Option<String>,
    /// Latest short progress line, replaced as activity arrives.
    pub status: Option<String>,
    pub state: BackgroundTaskState,
    /// Local monotonic order of the last update that touched this row. Used to
    /// stop a delayed restoration result from replacing newer live state.
    pub sequence: u64,
    pub started_at: Option<SystemTime>,
    pub updated_at: Option<SystemTime>,
    pub completed_at: Option<SystemTime>,
    pub model: Option<String>,
    /// Distance from the selected root; direct children are depth 1.
    pub depth: Option<u32>,
    /// Most recent child output excerpt, for later hierarchical presentation.
    pub last_preview: Option<String>,
}

impl BackgroundTaskSummary {
    /// Name to show when the provider supplied none. Provider ids are long, so
    /// a trailing fragment keeps rows distinguishable without wrapping.
    pub fn display_label(&self) -> String {
        if let Some(name) = self.display_name.as_deref().map(str::trim)
            && !name.is_empty()
        {
            return name.to_owned();
        }
        let id = self.key.id.as_str();
        let tail = id.rsplit(['-', '_', ':']).next().unwrap_or(id);
        let tail = if tail.len() >= 4 { tail } else { id };
        let start = tail.len().saturating_sub(8);
        format!("Agent {}", &tail[start..])
    }
}

/// A patch applied to one task. Every field beyond the key is optional so a
/// record that only carries a status line cannot erase a known lifecycle state.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BackgroundTaskUpdate {
    pub refs: Option<BackgroundTaskRefs>,
    pub state: Option<BackgroundTaskState>,
    pub display_name: Option<String>,
    pub agent_type: Option<String>,
    pub objective: Option<String>,
    pub status: Option<String>,
    pub model: Option<String>,
    pub depth: Option<u32>,
    pub last_preview: Option<String>,
    pub started_at: Option<SystemTime>,
    pub updated_at: Option<SystemTime>,
    pub completed_at: Option<SystemTime>,
}

impl BackgroundTaskUpdate {
    pub fn state(state: BackgroundTaskState) -> Self {
        Self {
            state: Some(state),
            ..Self::default()
        }
    }
}

/// Replacement snapshot handed to the Agent pane and the view.
#[derive(Clone, Debug, PartialEq)]
pub struct BackgroundTaskSnapshot {
    pub parent_session: BackgroundTaskKey,
    pub tasks: Vec<BackgroundTaskSummary>,
    pub discovery: BackgroundTaskDiscoveryState,
    /// Advances when a task is created or changes lifecycle state. The title-bar
    /// button compares it against the last ordinal seen for this parent session,
    /// so metadata-only updates never re-raise the unseen indicator.
    pub activity: u64,
}

impl BackgroundTaskSnapshot {
    pub fn active_count(&self) -> usize {
        self.tasks
            .iter()
            .filter(|task| task.state.is_active())
            .count()
    }

    pub fn needs_input_count(&self) -> usize {
        self.tasks
            .iter()
            .filter(|task| task.state == BackgroundTaskState::NeedsInput)
            .count()
    }

    pub fn terminal_count(&self) -> usize {
        self.tasks.len() - self.active_count()
    }
}

/// Reduces provider input into the shared model. Both adapters own one of these
/// per parent session; the shell keeps no second mutable copy.
#[derive(Debug)]
pub struct BackgroundTaskRegistry {
    parent_session: BackgroundTaskKey,
    tasks: HashMap<BackgroundTaskKey, BackgroundTaskSummary>,
    discovery: BackgroundTaskDiscoveryState,
    sequence: u64,
    activity: u64,
}

impl BackgroundTaskRegistry {
    pub fn new(parent_session: BackgroundTaskKey) -> Self {
        Self {
            parent_session,
            tasks: HashMap::new(),
            discovery: BackgroundTaskDiscoveryState::default(),
            sequence: 0,
            activity: 0,
        }
    }

    pub fn parent_session(&self) -> &BackgroundTaskKey {
        &self.parent_session
    }

    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    pub fn contains(&self, key: &BackgroundTaskKey) -> bool {
        self.tasks.contains_key(key)
    }

    pub fn get(&self, key: &BackgroundTaskKey) -> Option<&BackgroundTaskSummary> {
        self.tasks.get(key)
    }

    /// Order counter a restoration request captures before it starts, so its
    /// later response can tell which rows changed while it was in flight.
    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    pub fn discovery(&self) -> &BackgroundTaskDiscoveryState {
        &self.discovery
    }

    /// Returns true when the state changed, so callers only publish a snapshot
    /// for a real transition.
    pub fn set_discovery(&mut self, discovery: BackgroundTaskDiscoveryState) -> bool {
        if self.discovery == discovery {
            return false;
        }
        self.discovery = discovery;
        true
    }

    /// Apply a live update. Returns true when anything changed.
    pub fn apply(&mut self, key: BackgroundTaskKey, update: BackgroundTaskUpdate) -> bool {
        self.sequence += 1;
        let sequence = self.sequence;
        let parent_session = self.parent_session.clone();

        match self.tasks.get_mut(&key) {
            Some(existing) => {
                let previous_state = existing.state;
                let changed = merge_update(existing, &update, sequence);
                if existing.state != previous_state {
                    self.activity += 1;
                }
                if !changed {
                    self.sequence -= 1;
                }
                changed
            }
            None => {
                let refs = update.refs.clone().unwrap_or_else(|| default_refs(&key));
                let mut summary = BackgroundTaskSummary {
                    key: key.clone(),
                    parent_session,
                    refs,
                    display_name: None,
                    agent_type: None,
                    objective: None,
                    status: None,
                    state: BackgroundTaskState::Starting,
                    sequence,
                    started_at: None,
                    updated_at: None,
                    completed_at: None,
                    model: None,
                    depth: None,
                    last_preview: None,
                };
                merge_update(&mut summary, &update, sequence);
                self.tasks.insert(key, summary);
                self.activity += 1;
                true
            }
        }
    }

    /// Fold a restored row in. `starting_sequence` is the value [`sequence`]
    /// returned when the restoration request began: a row updated live since
    /// then keeps its state and only accepts still-missing metadata.
    ///
    /// [`sequence`]: Self::sequence
    pub fn merge_restored(
        &mut self,
        key: BackgroundTaskKey,
        update: BackgroundTaskUpdate,
        starting_sequence: u64,
    ) -> bool {
        let is_stale = self
            .tasks
            .get(&key)
            .is_some_and(|existing| existing.sequence > starting_sequence);
        if is_stale {
            let metadata_only = BackgroundTaskUpdate {
                state: None,
                completed_at: None,
                updated_at: None,
                ..update
            };
            return self.apply(key, metadata_only);
        }
        self.apply(key, update)
    }

    /// Drop every row, for example when the pane switches to another session.
    pub fn clear(&mut self) -> bool {
        if self.tasks.is_empty() && self.discovery == BackgroundTaskDiscoveryState::NotLoaded {
            return false;
        }
        self.tasks.clear();
        self.discovery = BackgroundTaskDiscoveryState::NotLoaded;
        self.activity += 1;
        true
    }

    pub fn snapshot(&self) -> BackgroundTaskSnapshot {
        let mut tasks: Vec<_> = self.tasks.values().cloned().collect();
        // Stable order keeps snapshot comparisons meaningful; the view applies
        // its own running/finished ordering on top.
        tasks.sort_by(|left, right| left.key.cmp(&right.key));
        BackgroundTaskSnapshot {
            parent_session: self.parent_session.clone(),
            tasks,
            discovery: self.discovery.clone(),
            activity: self.activity,
        }
    }
}

fn default_refs(key: &BackgroundTaskKey) -> BackgroundTaskRefs {
    match key.provider {
        BackgroundTaskProvider::Codex => BackgroundTaskRefs::Codex {
            thread_id: key.id.clone(),
            parent_thread_id: None,
        },
        BackgroundTaskProvider::ClaudeCode => BackgroundTaskRefs::ClaudeCode {
            task_id: None,
            tool_use_id: None,
            agent_id: None,
        },
    }
}

fn merge_update(
    summary: &mut BackgroundTaskSummary,
    update: &BackgroundTaskUpdate,
    sequence: u64,
) -> bool {
    let mut changed = false;

    if let Some(refs) = update.refs.as_ref() {
        let mut merged = summary.refs.clone();
        merged.merge_from(refs);
        if merged != summary.refs {
            summary.refs = merged;
            changed = true;
        }
    }
    if let Some(state) = update.state
        && summary.state != state
    {
        summary.state = state;
        changed = true;
    }
    changed |= replace_text(&mut summary.display_name, &update.display_name);
    changed |= replace_text(&mut summary.agent_type, &update.agent_type);
    changed |= replace_text(&mut summary.objective, &update.objective);
    changed |= replace_text(&mut summary.status, &update.status);
    changed |= replace_text(&mut summary.model, &update.model);
    changed |= replace_text(&mut summary.last_preview, &update.last_preview);
    if let Some(depth) = update.depth
        && summary.depth != Some(depth)
    {
        summary.depth = Some(depth);
        changed = true;
    }
    // The earliest known start wins: a restored row can report a start time
    // that a live update observed only after the task was already running.
    if let Some(started_at) = update.started_at
        && summary.started_at.is_none_or(|known| started_at < known)
    {
        summary.started_at = Some(started_at);
        changed = true;
    }
    if let Some(completed_at) = update.completed_at
        && summary.completed_at != Some(completed_at)
    {
        summary.completed_at = Some(completed_at);
        changed = true;
    }
    if let Some(updated_at) = update.updated_at
        && summary.updated_at.is_none_or(|known| updated_at > known)
    {
        summary.updated_at = Some(updated_at);
        changed = true;
    }

    if changed {
        summary.sequence = sequence;
    }
    changed
}

fn replace_text(current: &mut Option<String>, incoming: &Option<String>) -> bool {
    let Some(incoming) = incoming else {
        return false;
    };
    if current.as_deref() == Some(incoming.as_str()) {
        return false;
    }
    *current = Some(incoming.clone());
    true
}

#[cfg(test)]
mod tests;
