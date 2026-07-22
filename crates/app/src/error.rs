//! terminal engine for the self-owned renderer shell.
//!
//! Drives a `terminal` ConPTY session and exposes snapshots/events to the
//! renderer-owned shell. This module must not depend on UI code: the shell owns
//! layout, presentation, and window integration.

use std::{error, fmt};

/// Stable error categories for the shell's in-window error panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineErrorCode {
    /// ConPTY spawn failed (bad shell, working dir, …).
    PtySpawn,
    /// libghostty-vt engine init failed.
    EngineInit,
}

/// A structured engine failure.
#[derive(Debug)]
pub struct EngineError {
    pub code: EngineErrorCode,
    pub message: String,
}

impl EngineError {
    pub(crate) fn new(code: EngineErrorCode, message: impl Into<String>) -> Self {
        EngineError {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl error::Error for EngineError {}
