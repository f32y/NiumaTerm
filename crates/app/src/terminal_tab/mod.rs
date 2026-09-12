//! Terminal presentation and its GPUI host.
//!
//! `nmt_terminal` owns input encoding, cell selection, copy requests, and link
//! interpretation. `pane_model` maps pixel gestures onto those operations and
//! owns frame and list state.
//! `frame_source` reads the session and resolves images. `view`, `terminal_view`
//! and `paint` own entities, elements, timers, and drawing. Presentation values
//! receive settings and theme snapshots instead of consulting host globals.
//!
//! The application shell owns tabs, workspaces, and settings; this module
//! reads only the [`settings::TerminalSettings`] snapshot the shell installs
//! and exposes the pane, its metrics, and its host events. Modules that only
//! serve the pane internally stay private to this module.

pub(in crate::terminal_tab) mod block_list;
pub(in crate::terminal_tab) mod dirty;
pub mod frame;
pub(in crate::terminal_tab) mod graphics;
pub(in crate::terminal_tab) mod layout;
pub mod metrics;
pub(in crate::terminal_tab) mod paint;
pub(in crate::terminal_tab) mod scrollbar;

pub use nmt_terminal::session;

pub(in crate::terminal_tab) mod frame_source;
pub(in crate::terminal_tab) mod pane_model;
#[cfg(test)]
mod remote_tests;
pub mod settings;
pub(in crate::terminal_tab) mod terminal_view;
pub(in crate::terminal_tab) mod theme;
pub mod view;
pub(in crate::terminal_tab) mod wake;
