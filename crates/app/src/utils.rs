use std::env;
use std::future::Future;
#[cfg(not(test))]
use std::panic;
use std::path::PathBuf;

pub fn get_exe_dir() -> PathBuf {
    env::current_exe()
        .expect("locate current executable")
        .parent()
        .expect("current executable has no parent")
        .to_path_buf()
}

/// Run `future` on the shared runtime and wait for it from any executor.
/// Tokio timers, processes, and sockets need a runtime context that GPUI's
/// executors lack. A panic in the task resumes in the caller, as it would have
/// had the work run inline.
pub async fn on_runtime<T: Send + 'static>(future: impl Future<Output = T> + Send + 'static) -> T {
    // GPUI's test scheduler accepts wakes only from its own thread, and a
    // runtime task finishing after its test would wake a finished scheduler.
    // Unit tests therefore wait for the runtime work in place.
    #[cfg(test)]
    return nmt_runtime::handle().block_on(future);

    #[cfg(not(test))]
    match nmt_runtime::handle().spawn(future).await {
        Ok(value) => value,
        Err(error) => panic::resume_unwind(error.into_panic()),
    }
}
