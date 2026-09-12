//! Transcript timing hooks and operation names shared by both build modes.

#[cfg(not(enable_profiling))]
pub use crate::transcript::disabled::*;
#[cfg(enable_profiling)]
pub use crate::transcript::enabled::*;
pub use crate::transcript::operation::Operation;

#[cfg(not(enable_profiling))]
mod disabled;
#[cfg(enable_profiling)]
mod enabled;
mod operation;
