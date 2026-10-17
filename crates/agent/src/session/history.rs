#[cfg(test)]
#[path = "history_tests.rs"]
mod history_tests;

use std::cmp::Reverse;
use std::mem::{self, take};
use std::path::PathBuf;

use nmt_profile::AgentProfile;
use serde::{Deserialize, Serialize};
use tokio::task::spawn_blocking;

use crate::chat::{SessionOrigin, SessionScope, SessionSummary};
use crate::claude_code::sessions;
use crate::codex::rollouts::{self, ProviderFilter};
use crate::profile::agent_launch;
use crate::session::AgentKind;
use crate::{LaunchConfig, dsh};

/// The command a view in another process lists and resumes the host's
/// conversations with, one [`HistoryStep`] at a time.
pub const HISTORY_METHOD: &str = "history";

/// What a view in another process asks of the host's conversation history.
/// The host holds the transcripts, and the rows it lists reach every view
/// through the published view.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum HistoryStep {
    List(SessionScope),
    Resume(SessionSummary),
}

#[derive(Default)]
pub struct SessionHistory {
    next_request_id: u64,
    filesystem_request: Option<FilesystemHistoryRequest>,

    /// The rows the list shows: this tab's own agent's rows, merged newest
    /// first with other agents' rows while those are shown. Rebuilt from the
    /// two sources whenever either changes, so it is read here and written
    /// through the methods below.
    pub sessions: Vec<SessionSummary>,

    /// Rows the tab's own agent reported, in the order it reported them.
    own: Vec<SessionSummary>,

    /// The user asked for the list while it held no rows. A list on its way
    /// from the harness arrives as a later event, so an empty snapshot here is
    /// not yet an answer.
    awaiting_rows: bool,

    /// Rows listed from the records of every other configured agent, each
    /// holding the profile that continues it.
    other_agents: Vec<SessionSummary>,

    other_agents_request: Option<OtherAgentsRequest>,
    other_agents_remaining: usize,

    /// Show only the tab's own agent's rows, even where other agents' rows
    /// were listed.
    pub own_agent_only: bool,

    /// Expected rows while a disk query is still loading; retired with its request.
    pub pending: Option<usize>,

    /// Search results replace recent pages; the next recent page replaces matches.
    pub showing_search: bool,

    pub scope: SessionScope,
}

/// One listing of other agents' conversations, retired like a filesystem
/// request when the scope or conversation it was made for is replaced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OtherAgentsRequest {
    id: u64,
    scope: SessionScope,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilesystemHistoryRequest {
    id: u64,
    scope: SessionScope,
    cwd: Option<String>,
}

pub enum CountPublication {
    Stale,
    Empty,
    LoadRows,
}

impl SessionHistory {
    // Disk reads may finish after their view has been replaced. Retiring the
    // request also removes its placeholders without waiting for that work.
    pub fn invalidate_filesystem_history(&mut self) {
        self.filesystem_request = None;
        self.pending = None;
    }

    /// Whether a listing this pane started is still on its way back. A
    /// filesystem read reserves its rows while the count runs, and a harness
    /// that lists over the protocol answers with a later event.
    pub fn list_in_flight(&self) -> bool {
        self.filesystem_request.is_some()
            || self.pending.is_some()
            || self.other_agents_remaining > 0
    }

    /// Wait for rows without cancelling the requests already reading them.
    pub fn await_rows(&mut self) {
        self.awaiting_rows = true;
    }

    /// Whether the list is wanted and still has no rows to show.
    pub fn is_awaiting_rows(&self) -> bool {
        self.awaiting_rows
    }

    /// Clear the wait, reporting whether it was still outstanding. A listing
    /// that completes after the wait ended is not this request's answer.
    pub fn resolve_awaiting_rows(&mut self) -> bool {
        mem::take(&mut self.awaiting_rows)
    }

    pub fn begin_filesystem_history(&mut self, cwd: Option<String>) -> FilesystemHistoryRequest {
        self.invalidate_filesystem_history();

        self.next_request_id = self
            .next_request_id
            .checked_add(1)
            .expect("history request id exhausted");

        let request = FilesystemHistoryRequest {
            id: self.next_request_id,
            scope: self.scope,
            cwd,
        };

        self.filesystem_request = Some(request.clone());

        request
    }

    pub(crate) fn owns_filesystem_request(
        &self,
        request: &FilesystemHistoryRequest,
        cwd: Option<&str>,
    ) -> bool {
        self.filesystem_request.as_ref() == Some(request)
            && self.scope == request.scope
            && request.cwd.as_deref() == cwd
    }

    pub fn publish_filesystem_count(
        &mut self,
        request: &FilesystemHistoryRequest,
        cwd: Option<&str>,
        count: usize,
    ) -> CountPublication {
        if !self.owns_filesystem_request(request, cwd) {
            return CountPublication::Stale;
        }

        if count == 0 {
            self.own.clear();

            self.merge();

            self.invalidate_filesystem_history();

            CountPublication::Empty
        } else {
            self.pending = Some(count);

            CountPublication::LoadRows
        }
    }

    pub fn publish_filesystem_rows(
        &mut self,
        request: &FilesystemHistoryRequest,
        cwd: Option<&str>,
        sessions: Vec<SessionSummary>,
    ) -> bool {
        if !self.owns_filesystem_request(request, cwd) {
            return false;
        }

        self.own = sessions;

        self.merge();

        self.invalidate_filesystem_history();

        true
    }

    pub fn append_page(&mut self, sessions: Vec<SessionSummary>) {
        // Rows a caller placed on the list directly are the own rows a page
        // continues from.
        if self.own.is_empty() && self.other_agents.is_empty() {
            self.own = take(&mut self.sessions);
        }

        if take(&mut self.showing_search) {
            self.own.clear();
        }

        for session in sessions {
            if !self.own.iter().any(|existing| existing.id == session.id) {
                self.own.push(session);
            }
        }

        self.merge();
    }

    pub fn search_results(&mut self, sessions: Vec<SessionSummary>) -> bool {
        if sessions.is_empty() {
            return false;
        }

        self.invalidate_filesystem_history();

        // A search answers a query about this agent's conversations; other
        // agents' rows come back with the next recent list.
        self.sessions = sessions;

        self.own.clear();

        self.showing_search = true;

        true
    }

    /// Drop every row, for a list about to be refilled under another scope
    /// or for another computer. Pending listings of other agents are retired
    /// with them.
    pub fn clear_rows(&mut self) {
        self.sessions.clear();
        self.own.clear();
        self.other_agents.clear();

        self.other_agents_request = None;
        self.other_agents_remaining = 0;
        self.showing_search = false;
    }

    /// Start a listing of other agents' conversations under the current
    /// scope, retiring any listing still running and the rows it placed.
    pub fn begin_other_agents(&mut self, sources: usize) -> OtherAgentsRequest {
        self.other_agents_remaining = sources;

        self.other_agents.clear();

        self.merge();

        self.next_request_id = self
            .next_request_id
            .checked_add(1)
            .expect("history request id exhausted");

        let request = OtherAgentsRequest {
            id: self.next_request_id,
            scope: self.scope,
        };

        self.other_agents_request = Some(request.clone());

        request
    }

    /// Complete one source, including an empty or failed read. Requests for
    /// an earlier scope cannot complete a newer listing.
    pub fn finish_other_agent(&mut self, request: &OtherAgentsRequest) -> bool {
        if self.other_agents_request.as_ref() != Some(request) || self.scope != request.scope {
            return false;
        }

        self.other_agents_remaining = self.other_agents_remaining.saturating_sub(1);

        true
    }

    /// Add the rows one source of `request` listed, unless the list has moved
    /// on since. Sources answer independently, so the request stays open for
    /// the rest. Returns whether the rows were taken.
    pub fn publish_other_agents(
        &mut self,
        request: &OtherAgentsRequest,
        rows: Vec<SessionSummary>,
    ) -> bool {
        if self.other_agents_request.as_ref() != Some(request) || self.scope != request.scope {
            return false;
        }

        self.other_agents.extend(rows);

        self.merge();

        true
    }

    /// Show or hide other agents' rows. Returns whether anything on the list
    /// changed.
    pub fn set_own_agent_only(&mut self, own_agent_only: bool) -> bool {
        if self.own_agent_only == own_agent_only {
            return false;
        }

        self.own_agent_only = own_agent_only;

        self.merge();

        true
    }

    /// Whether other agents' rows were listed at all; without them, the
    /// choice to show or hide them means nothing.
    pub fn has_other_agents(&self) -> bool {
        !self.other_agents.is_empty()
    }

    /// Rebuild the shown rows. Own rows keep the order their agent gave them;
    /// other agents' rows fall in among them by recency, and one that repeats
    /// an own row's id is the same conversation listed twice.
    fn merge(&mut self) {
        if self.showing_search {
            return;
        }

        let mut rows = self.own.clone();

        if !self.own_agent_only {
            rows.extend(
                self.other_agents
                    .iter()
                    .filter(|row| !self.own.iter().any(|own| own.id == row.id))
                    .cloned(),
            );

            if !self.other_agents.is_empty() {
                rows.sort_by_key(|row| Reverse(row.last_active));
            }
        }

        self.sessions = rows;
    }
}

/// The filesystem history a scope covers. Only a backend that reads its own
/// transcripts takes this route; one that lists over the protocol asks its
/// server for the scope instead.
pub fn count_scoped_sessions(scope: SessionScope, cwd: Option<&str>) -> usize {
    match scope {
        SessionScope::CurrentDirectory => sessions::count_sessions(cwd),
        SessionScope::AllDirectories => sessions::count_all_sessions(),
    }
}

pub fn list_scoped_sessions(scope: SessionScope, cwd: Option<&str>) -> Vec<SessionSummary> {
    match scope {
        SessionScope::CurrentDirectory => sessions::list_sessions(cwd),
        SessionScope::AllDirectories => sessions::list_all_sessions(),
    }
}

/// Where one other agent's conversations are read from. Profiles that share a
/// record store share a source, so a store is read once however many
/// profiles point at it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum HistoryStore {
    /// Claude Code's transcript directory.
    ClaudeTranscripts,
    /// Codex rollouts under one Codex home, limited to the model providers
    /// one profile can continue.
    CodexRollouts {
        home: PathBuf,
        providers: ProviderFilter,
    },
    /// The DeepSeek harness's session store, reached through its host.
    DeepSeekHost,
}

/// One other agent's conversations to list, with the profile a row read from
/// it is continued by.
#[derive(Clone, Debug)]
pub struct HistorySource {
    origin: SessionOrigin,
    store: HistoryStore,
    launch: LaunchConfig,
}

impl HistorySource {
    pub fn origin(&self) -> &SessionOrigin {
        &self.origin
    }
}

fn history_store(profile: &AgentProfile, launch: &LaunchConfig) -> Option<HistoryStore> {
    Some(match profile.kind {
        AgentKind::Claude => HistoryStore::ClaudeTranscripts,
        AgentKind::Codex => HistoryStore::CodexRollouts {
            home: rollouts::codex_home(&launch.env)?,
            providers: launch
                .provider
                .as_ref()
                .map_or(ProviderFilter::ExceptGenerated, |provider| {
                    ProviderFilter::Only(provider.id.clone())
                }),
        },
        AgentKind::DeepSeek => HistoryStore::DeepSeekHost,
    })
}

/// The stores a tab running `own` lists other agents' conversations from:
/// one per distinct store among `profiles`, skipping the store the tab's own
/// agent already lists. Where several profiles share a store, `preferred`
/// (the configured default) continues its rows if it is one of them, and
/// otherwise the first in configuration order does.
pub fn other_agent_sources(
    profiles: &[AgentProfile],
    own: &AgentProfile,
    preferred: &str,
) -> Vec<HistorySource> {
    let own_store = history_store(own, &agent_launch(own));

    let ordered = profiles
        .iter()
        .filter(|profile| profile.name == preferred)
        .chain(profiles.iter().filter(|profile| profile.name != preferred));

    let mut sources: Vec<HistorySource> = Vec::new();

    for profile in ordered {
        let launch = agent_launch(profile);

        let Some(store) = history_store(profile, &launch) else {
            continue;
        };

        if Some(&store) == own_store.as_ref() || sources.iter().any(|source| source.store == store)
        {
            continue;
        }

        sources.push(HistorySource {
            origin: SessionOrigin {
                kind: profile.kind,
                profile: profile.name.clone(),
            },
            store,
            launch,
        });
    }

    sources
}

/// The conversations `source` holds that `scope` covers from `cwd`, each
/// tagged with the profile that continues it. Runs on the application's async
/// runtime; the disk stores are read on its blocking pool.
pub async fn list_source(
    source: HistorySource,
    scope: SessionScope,
    cwd: Option<String>,
) -> Vec<SessionSummary> {
    let HistorySource {
        origin,
        store,
        launch,
    } = source;

    let rows = match store {
        HistoryStore::ClaudeTranscripts => {
            spawn_blocking(move || list_scoped_sessions(scope, cwd.as_deref()))
                .await
                .unwrap_or_default()
        }
        HistoryStore::CodexRollouts { home, providers } => spawn_blocking(move || {
            rollouts::list_sessions(&home, scope, cwd.as_deref(), &providers)
        })
        .await
        .unwrap_or_default(),
        // The harness's rows carry no directory to open a conversation in,
        // so only the ones of this directory can be offered under any scope,
        // as a DeepSeek tab's own list does.
        HistoryStore::DeepSeekHost => dsh::list_sessions(&launch, cwd.as_deref()).await,
    };

    rows.into_iter()
        .map(|row| SessionSummary {
            origin: Some(origin.clone()),
            ..row
        })
        .collect()
}
