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

pub use nmt_terminal::session;

pub mod frame;

pub mod metrics;

pub mod settings;

pub mod view;

mod block_list;
mod dirty;

mod graphics;
mod layout;

mod paint;
mod scrollbar;

mod frame_source;
mod pane_model;

mod terminal_view;
mod theme;

mod wake;

#[cfg(test)]
mod remote_tests;
