//! The recent-conversation list: which conversations a tab offers to reopen,
//! where that list comes from, and what reopening one does.
//!
//! A harness that lists over the protocol is asked; one that keeps its
//! transcripts on disk is read here instead, which is why the list has a
//! loading shape of its own rather than simply arriving.

pub(crate) use nmt_agent::session::history::FilesystemHistoryRequest;

#[cfg(test)]
mod restore_tests;
#[cfg(test)]
mod tests;

#[cfg(test)]
use nmt_agent::session::history::CountPublication;
