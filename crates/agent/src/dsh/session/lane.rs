//! Ordered background execution of a session's commands.
//!
//! A command is a call the harness may take seconds to answer, and the thread
//! issuing it draws the window, so the call runs here and its answer comes back
//! as a frame. One command runs at a time, in submission order: a prompt sent
//! right after a model pick has to reach the harness after that pick.

#[cfg(test)]
#[path = "lane_tests.rs"]
mod lane_tests;

use std::future::Future;

use futures::future::BoxFuture;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio::task::AbortHandle;

pub(super) struct CommandLane {
    sender: UnboundedSender<BoxFuture<'static, ()>>,
    runner: AbortHandle,
}

impl CommandLane {
    pub(super) fn new() -> Self {
        let (sender, mut receiver) = unbounded_channel::<BoxFuture<'static, ()>>();

        let runner = nmt_platform::runtime().spawn(async move {
            while let Some(command) = receiver.recv().await {
                command.await;
            }
        });

        Self {
            sender,
            runner: runner.abort_handle(),
        }
    }

    pub(super) fn run(&self, command: impl Future<Output = ()> + Send + 'static) {
        // The runner only ends with this lane, so the send cannot fail while
        // there is still a lane to call this on.
        let _ = self.sender.send(Box::pin(command));
    }
}

impl Drop for CommandLane {
    fn drop(&mut self) {
        // A closed tab has nowhere to show an answer, and a prompt still
        // waiting here must not start a turn nobody can see.
        self.runner.abort();
    }
}
