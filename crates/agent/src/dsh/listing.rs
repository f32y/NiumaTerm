//! DeepSeek conversations listed for a tab that runs another agent, so a
//! history list can offer every agent's work in one directory.
//!
//! The harness keeps its sessions behind its own host, so listing them needs a
//! running `dsh web`. The shared host of an open DeepSeek tab is reused when
//! there is one; otherwise one is started and kept for a while, because blank
//! tabs list history each time they open and Node's start cost would otherwise
//! be paid on every one of them.

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tokio::time::sleep;
use tracing::debug;

use crate::LaunchConfig;
use crate::chat::SessionSummary;
use crate::dsh::history::sessions;
use crate::dsh::host::{Host, shared};

/// How long a host started only to list sessions stays up after the listing.
/// Long enough to serve the next few blank tabs, short enough that a user who
/// never opens a DeepSeek tab does not keep a Node process around.
const LISTING_HOST_LINGER: Duration = Duration::from_secs(5 * 60);

/// The resumable conversations the harness `launch` selects holds for `cwd`,
/// or for every directory when `cwd` is `None`. A harness that is missing or
/// fails to start lists nothing: its absence is not an error for a list of
/// other agents' work.
pub async fn list_sessions(launch: &LaunchConfig, cwd: Option<&str>) -> Vec<SessionSummary> {
    let host = match shared(launch).await {
        Ok(host) => host,
        Err(error) => {
            debug!(error = error.message(), "deepseek history unavailable");

            return Vec::new();
        }
    };

    let listed = host
        .client()
        .call("session/list", json!({ "_request": {} }))
        .await;

    linger(host);

    match listed {
        Ok(value) => sessions(&value, cwd),
        Err(error) => {
            debug!(error = error.message(), "deepseek session list failed");

            Vec::new()
        }
    }
}

fn linger(host: Arc<Host>) {
    nmt_platform::runtime().spawn(async move {
        sleep(LISTING_HOST_LINGER).await;

        drop(host);
    });
}
