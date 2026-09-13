use std::mem::take;

use crate::chat::{SessionScope, SessionSummary};
use crate::claude_code::sessions;

#[derive(Default)]
pub struct SessionHistory {
    next_request_id: u64,
    filesystem_request: Option<FilesystemHistoryRequest>,
    pub sessions: Vec<SessionSummary>,

    /// Expected rows while a disk query is still loading; retired with its request.
    pub pending: Option<usize>,

    /// Search results replace recent pages; the next recent page replaces matches.
    pub showing_search: bool,

    pub scope: SessionScope,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilesystemHistoryRequest {
    id: u64,
    scope: SessionScope,
    cwd: Option<String>,
    epoch: u64,
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

    pub fn begin_filesystem_history(
        &mut self,
        cwd: Option<String>,
        epoch: u64,
    ) -> FilesystemHistoryRequest {
        self.invalidate_filesystem_history();

        self.next_request_id = self
            .next_request_id
            .checked_add(1)
            .expect("history request id exhausted");

        let request = FilesystemHistoryRequest {
            id: self.next_request_id,
            scope: self.scope,
            cwd,
            epoch,
        };

        self.filesystem_request = Some(request.clone());

        request
    }

    pub(crate) fn owns_filesystem_request(
        &self,
        request: &FilesystemHistoryRequest,
        cwd: Option<&str>,
        epoch: u64,
    ) -> bool {
        self.filesystem_request.as_ref() == Some(request)
            && self.scope == request.scope
            && request.cwd.as_deref() == cwd
            && request.epoch == epoch
    }

    pub fn publish_filesystem_count(
        &mut self,
        request: &FilesystemHistoryRequest,
        cwd: Option<&str>,
        epoch: u64,
        count: usize,
    ) -> CountPublication {
        if !self.owns_filesystem_request(request, cwd, epoch) {
            return CountPublication::Stale;
        }

        if count == 0 {
            self.sessions.clear();
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
        epoch: u64,
        sessions: Vec<SessionSummary>,
    ) -> bool {
        if !self.owns_filesystem_request(request, cwd, epoch) {
            return false;
        }

        self.sessions = sessions;
        self.invalidate_filesystem_history();

        true
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

impl SessionHistory {
    pub fn append_page(&mut self, sessions: Vec<SessionSummary>) {
        if take(&mut self.showing_search) {
            self.sessions.clear();
        }

        for session in sessions {
            if !self
                .sessions
                .iter()
                .any(|existing| existing.id == session.id)
            {
                self.sessions.push(session);
            }
        }
    }

    pub fn search_results(&mut self, sessions: Vec<SessionSummary>) -> bool {
        if sessions.is_empty() {
            return false;
        }

        self.invalidate_filesystem_history();
        self.sessions = sessions;
        self.showing_search = true;

        true
    }
}
