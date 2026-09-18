#[cfg(test)]
mod tests;

use std::future::Future;

use futures::StreamExt as _;
use futures::channel::{mpsc, oneshot};
use gpui::{App, Global};

type Write = Box<dyn FnOnce() + Send>;

struct BackgroundWrites(mpsc::UnboundedSender<Write>);

impl Global for BackgroundWrites {}

/// Queue a complete file update behind the ones already accepted; updates run
/// one at a time in submission order.
pub fn background_write(cx: &mut App, write: impl FnOnce() + Send + 'static) {
    submit(cx, Box::new(write));
}

/// Like [`background_write`], and resolves with the write's result. Dropping
/// the reply does not discard an accepted write, so closing a view cannot
/// lose its edits.
pub fn background_write_reply<R: Send + 'static, F: FnOnce() -> R + Send + 'static>(
    cx: &mut App,
    write: F,
) -> impl Future<Output = R> + Send + use<R, F> {
    let (sender, receiver) = oneshot::channel();

    submit(
        cx,
        Box::new(move || {
            let _ = sender.send(write());
        }),
    );

    async move { receiver.await.expect("background write panicked") }
}

fn submit(cx: &mut App, write: Write) {
    if !cx.has_global::<BackgroundWrites>() {
        let (sender, mut receiver) = mpsc::unbounded::<Write>();

        cx.background_executor()
            .spawn(async move {
                while let Some(write) = receiver.next().await {
                    write();
                }
            })
            .detach();

        cx.set_global(BackgroundWrites(sender));
    }

    cx.global::<BackgroundWrites>()
        .0
        .unbounded_send(write)
        .expect("background writer stopped before application shutdown");
}
