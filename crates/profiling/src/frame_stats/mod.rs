//! Frame timing hooks selected by the compile-time profiling switch.

#[cfg(not(enable_profiling))]
pub use crate::frame_stats::disabled::*;
#[cfg(enable_profiling)]
pub use crate::frame_stats::enabled::*;

#[cfg(not(enable_profiling))]
mod disabled;
#[cfg(enable_profiling)]
mod enabled;
