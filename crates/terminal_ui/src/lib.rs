//! The GPUI terminal pane: session plumbing over the VT engine, frame
//! extraction, block-list presentation, and the pane view itself.
//!
//! The application shell owns tabs, workspaces, and settings; this crate
//! reads only the [`settings::TerminalSettings`] snapshot the shell installs
//! and exposes the pane, its metrics, and its host events. Modules that only
//! serve the pane internally stay private to the crate.

pub(crate) mod block_list;
pub(crate) mod dirty;
pub mod frame;
pub(crate) mod graphics;
pub(crate) mod input;
pub(crate) mod layout;
pub(crate) mod links;
pub mod metrics;
pub(crate) mod paint_text;
pub(crate) mod scrollbar;
pub use nmt_terminal::session;
#[cfg(test)]
mod remote_tests;
pub mod settings;
pub(crate) mod surface;
pub(crate) mod terminal_view;
pub(crate) mod theme;
pub mod view;
pub(crate) mod wake;
