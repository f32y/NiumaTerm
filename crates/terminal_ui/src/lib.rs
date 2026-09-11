//! Terminal presentation and its GPUI host.
//!
//! `nmt_terminal` owns input encoding, cell selection, copy requests, and link
//! interpretation. `pane_model` maps pixel gestures onto those operations and
//! owns frame and list state.
//! `frame_source` reads the session and resolves images. `view`, `terminal_view`
//! and `paint` own entities, elements, timers, and drawing. Presentation values
//! receive settings and theme snapshots instead of consulting host globals.
//!
//! The application shell owns tabs, workspaces, and settings; this crate
//! reads only the [`settings::TerminalSettings`] snapshot the shell installs
//! and exposes the pane, its metrics, and its host events. Modules that only
//! serve the pane internally stay private to the crate.

pub(crate) mod block_list;
pub(crate) mod dirty;
pub mod frame;
pub(crate) mod graphics;
pub(crate) mod layout;
pub mod metrics;
pub(crate) mod paint;
pub(crate) mod scrollbar;
pub use nmt_terminal::session;
pub(crate) mod frame_source;
pub(crate) mod pane_model;
#[cfg(test)]
mod remote_tests;
pub mod settings;
pub(crate) mod terminal_view;
pub(crate) mod theme;
pub mod view;
pub(crate) mod wake;
