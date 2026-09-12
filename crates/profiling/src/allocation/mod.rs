//! Allocation hooks selected by the compile-time profiling switch.

#[cfg(not(enable_profiling))]
pub use crate::allocation::disabled::*;
#[cfg(enable_profiling)]
pub use crate::allocation::enabled::*;

#[cfg(not(enable_profiling))]
mod disabled;
#[cfg(enable_profiling)]
mod enabled;
