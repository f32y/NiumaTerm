use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use chrono::Local;
use nmt_agent::claude_code::usage_fetcher::{UsageFetchError, fetch_with_cancel};
use nmt_agent::codex::usage_fetcher::fetch;
use nmt_agent::usage::UsageSnapshot;
use nmt_platform::process::{decode_child_output, hidden_cmd_command};

use crate::daily_usage::{DailyTokenUsage, parse_usage};
use crate::usage_refresh::{FetchError, UsageSource};

pub(crate) fn account_sources() -> [Arc<dyn UsageSource<UsageSnapshot>>; 2] {
    [
        Arc::new(|_: &AtomicBool| fetch().map_err(FetchError::Failed)),
        Arc::new(|cancelled: &AtomicBool| {
            fetch_with_cancel(cancelled).map_err(|error| match error {
                UsageFetchError::Cancelled => FetchError::Cancelled,
                UsageFetchError::Failed(message) => FetchError::Failed(message),
            })
        }),
    ]
}

pub(crate) fn daily_source() -> Arc<dyn UsageSource<Option<DailyTokenUsage>>> {
    Arc::new(|_: &AtomicBool| {
        let now = Local::now();
        let since = now.format("%Y%m%d").to_string();
        let date = now.format("%Y-%m-%d").to_string();

        let output = hidden_cmd_command("npx")
            .args(["ccusage@latest", "-j", "--since", &since])
            .output()
            .map_err(|error| FetchError::Failed(format!("failed to run ccusage: {error}")))?;

        if !output.status.success() {
            return Err(FetchError::Failed(format!(
                "ccusage exited with {}: {}",
                output.status,
                decode_child_output(&output.stderr).trim()
            )));
        }

        parse_usage(&output.stdout, &date)
            .map(Some)
            .map_err(FetchError::Failed)
    })
}
