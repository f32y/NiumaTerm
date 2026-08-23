use std::path::{Path, PathBuf};
use std::{fs, io};

use tracing_appender::non_blocking;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, fmt, registry};

use crate::utils::get_data_dir;

/// Testing instances keep their own log directory so a test run cannot rotate
/// away or interleave with the logs of a normally launched terminal.
fn log_dir(testing: bool) -> PathBuf {
    let base = get_data_dir();
    if testing {
        base.join("Test").join("logs")
    } else {
        base.join("logs")
    }
}

fn rotate_logs(log_path: &Path) -> io::Result<()> {
    const LOG_PREFIX: &str = "app";

    fs::create_dir_all(log_path)?;

    let current = log_path.join(format!("{LOG_PREFIX}.log"));

    let oldest = log_path.join(format!("{LOG_PREFIX}-prev4.log"));
    let _ = fs::remove_file(oldest);

    // app-prev3.log -> app-prev4.log
    // app-prev2.log -> app-prev3.log
    // app-prev1.log -> app-prev2.log
    for i in (1..=3).rev() {
        let from = log_path.join(format!("{LOG_PREFIX}-prev{i}.log"));
        let to = log_path.join(format!("{LOG_PREFIX}-prev{}.log", i + 1));

        if from.exists() {
            fs::rename(from, to)?;
        }
    }

    // app.log -> app-prev1.log
    if current.exists() {
        let prev1 = log_path.join(format!("{LOG_PREFIX}-prev1.log"));
        fs::rename(current, prev1)?;
    }

    Ok(())
}

pub fn init_logging(testing: bool) -> io::Result<WorkerGuard> {
    let log_path = log_dir(testing);
    rotate_logs(&log_path)?;

    let log_file = log_path.join("app.log");

    let file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_file)?;

    let (non_blocking, guard) = non_blocking(file);

    let file_layer = fmt::layer()
        .with_writer(non_blocking)
        .with_ansi(false)
        .with_target(true)
        .with_thread_ids(true)
        .with_thread_names(true);

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    registry().with(filter).with(file_layer).init();

    Ok(guard)
}
