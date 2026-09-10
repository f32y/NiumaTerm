//! No allocator instrumentation is installed without `enable_profiling`.

/// Accept the startup switch without enabling allocation tracking.
#[inline(always)]
pub fn set_enabled(_: bool) {}
