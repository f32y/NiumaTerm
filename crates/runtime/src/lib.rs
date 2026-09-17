//! The process-wide tokio runtime.
//!
//! Network clients in this workspace run as tasks on one shared runtime
//! instead of each owning threads or a runtime of its own: tasks are what make
//! an in-flight request cancellable when its owner goes away, and one reactor
//! keeps the thread count independent of how many tabs are open.
//!
//! The runtime is a static rather than a value handed down from the
//! application entry point, because library code and its unit tests reach it
//! without an application object. It is never dropped, so no shutdown can run
//! from inside one of its own tasks.

use std::sync::OnceLock;

use tokio::runtime::{Builder, Handle, Runtime};

/// The work is dominated by waiting on sockets, so a small pool suffices.
const WORKER_THREADS: usize = 4;

static RUNTIME: OnceLock<Runtime> = OnceLock::new();

/// The shared runtime, started on first use.
///
/// Blocking on this handle from one of the runtime's own tasks panics; only
/// threads outside the runtime may wait on a task synchronously.
pub fn handle() -> &'static Handle {
    RUNTIME
        .get_or_init(|| {
            Builder::new_multi_thread()
                .worker_threads(WORKER_THREADS)
                .thread_name("nmt-io")
                .enable_all()
                .build()
                .expect("the operating system refused the I/O worker threads")
        })
        .handle()
}
