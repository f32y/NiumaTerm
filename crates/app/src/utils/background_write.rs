#[cfg(test)]
mod tests;

use std::future::Future;

use futures::StreamExt as _;
use futures::channel::{mpsc, oneshot};
use gpui::{App, Global};

type Write = Box<dyn FnOnce() + Send>;

struct BackgroundWrites(mpsc::UnboundedSender<Write>);

impl Global for BackgroundWrites {}

/// Serialize complete file updates in submission order. Dropping a reply does
/// not discard an accepted write, so closing a view cannot lose its edits.
pub fn background_write<R: Send + 'static, F: FnOnce() -> R + Send + 'static>(
    cx: &mut App,
    write: F,
) -> impl Future<Output = R> + Send + use<R, F> {
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

    let (sender, receiver) = oneshot::channel();

    cx.global::<BackgroundWrites>()
        .0
        .unbounded_send(Box::new(move || {
            let _ = sender.send(write());
        }))
        .expect("background writer stopped before application shutdown");

    async move { receiver.await.expect("background write panicked") }
}
