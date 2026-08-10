use std::mem;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use nmt_terminal::block_store::{BlockStore, SegmentMeta};
use nmt_terminal::clipboard::Clipboard;
use nmt_terminal::event::{BlockEvent, EventListener, TerminalEvent, WindowId};
use parking_lot::Mutex;
use tracing::debug;

use super::{HostEvent, HostEventQueue, InFlightBlock, SessionGraphics};
use crate::terminal;
use crate::terminal::wake::{Wake, WakeSender};

#[derive(Clone)]
pub struct TerminalEventProxy {
    events: HostEventQueue,
    /// Frozen block-split history (engine-block handles + command
    /// metadata); shared with `TerminalSession`. Fed on the PTY thread.
    block_store: Arc<Mutex<BlockStore>>,
    /// Live Kitty image generations keyed by image ID; shared with the session
    /// and pane. `UpdateGraphics` installs/removes generations here.
    generation_store: SessionGraphics,
    /// Lock-free mirror of the live generation count (shared with the session), kept
    /// in sync whenever `UpdateGraphics` installs or removes generations.
    live_image_count: Arc<AtomicUsize>,
    /// The current PTY read's block events, staged until that read's damage
    /// wake so image generations install before frozen slices bind.
    /// Bounded to one read cycle; flushed or discarded on damage/shutdown.
    staged_blocks: Arc<Mutex<Vec<BlockEvent>>>,
    /// The in-flight command; set at CommandStarted, cleared by
    /// CommandFinished / trust loss / exit.
    in_flight: Arc<Mutex<Option<InFlightBlock>>>,
    open_prompt: Arc<Mutex<bool>>,
    /// Frozen Kitty generation cache, pruned on the same batches that feed
    /// the block store (evicted blocks drop their cached images).
    frozen_images: terminal::graphics::FrozenImageCache,
    /// Source surface id, stamped onto every wake so the shell can route by tab.
    id: u64,
    /// Render-wakeup sender; `None` for sessions/tests without a live shell.
    wake: Option<WakeSender>,
}

impl TerminalEventProxy {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        events: HostEventQueue,
        block_store: Arc<Mutex<BlockStore>>,
        generation_store: SessionGraphics,
        live_image_count: Arc<AtomicUsize>,
        staged_blocks: Arc<Mutex<Vec<BlockEvent>>>,
        in_flight: Arc<Mutex<Option<InFlightBlock>>>,
        open_prompt: Arc<Mutex<bool>>,
        frozen_images: terminal::graphics::FrozenImageCache,
        id: u64,
        wake: Option<WakeSender>,
    ) -> Self {
        Self {
            events,
            block_store,
            generation_store,
            live_image_count,
            staged_blocks,
            in_flight,
            open_prompt,
            frozen_images,
            id,
            wake,
        }
    }

    fn signal(&self, kind: Wake) {
        if let Some(tx) = &self.wake {
            tx.send(kind);
        }
    }

    /// Flush the current read's staged block events into the block store.
    /// Called on the read's damage wake so items land together with the
    /// render they belong to. Empty in steady state.
    fn flush_staged_blocks(&self) {
        let batch = mem::take(&mut *self.staged_blocks.lock());
        if batch.is_empty() {
            return;
        }
        terminal::graphics::prune_frozen_images(&self.frozen_images, &batch);
        self.block_store.lock().apply(batch);
    }
}

impl EventListener for TerminalEventProxy {
    fn event(&self) -> (Option<TerminalEvent>, bool) {
        (None, false)
    }

    fn send_event(&self, event: TerminalEvent, _id: WindowId) {
        // Content damage drives a render with no chrome rebuild. This is the read's
        // final wake: image generations have already installed (UpdateGraphics runs
        // before this), so flush the staged block events before the UI
        // wakes.
        if matches!(
            event,
            TerminalEvent::TerminalDamaged(_) | TerminalEvent::Render
        ) {
            self.flush_staged_blocks();
            self.signal(Wake::Content(self.id));

            return;
        }

        // Decoded Kitty pixels: install/replace live generations and drop removed
        // ones, then wake for a lazy visible upload. Route-scoped so a cross-session
        // event is ignored.
        if let TerminalEvent::UpdateGraphics { route_id, queues } = event {
            if route_id != self.id as usize {
                return;
            }

            let mut store = self.generation_store.lock();

            for (image_id, data) in queues.pending_images {
                store.install(image_id, data);
            }

            for gid in queues.remove_queue {
                store.remove(gid.0 as u32);
            }

            // Publish the live count lock-free so the render path can skip the store.
            self.live_image_count.store(store.len(), Ordering::Relaxed);

            drop(store);

            self.signal(Wake::Content(self.id));

            return;
        }
        let host = match event {
            TerminalEvent::Title(t) | TerminalEvent::TitleWithSubtitle(t, _) => HostEvent::Title(t),
            TerminalEvent::ResetTitle => HostEvent::Title(String::new()),
            TerminalEvent::Bell => HostEvent::Bell,
            TerminalEvent::ProgressReport(report) => HostEvent::Progress(report),
            TerminalEvent::ClipboardStore(ty, text) => {
                let mut clipboard = Clipboard::default();

                clipboard.set(ty, text);

                return;
            }
            TerminalEvent::CloseTerminal(_) => {
                // The shell died: no ;D is coming for a running command, and any
                // half-staged block batch for the interrupted read is discarded.
                *self.in_flight.lock() = None;

                *self.open_prompt.lock() = false;

                self.staged_blocks.lock().clear();

                HostEvent::Exit
            }
            TerminalEvent::DesktopNotification { title, body } => {
                HostEvent::Notification { title, body }
            }
            TerminalEvent::InteractiveState(on) => HostEvent::InteractiveState(on),
            TerminalEvent::AltScreen(on) => HostEvent::AltScreen(on),
            TerminalEvent::PromptBoundaryTrusted(on) => {
                if !on {
                    // Trust lost mid-command (nested shell, malformed stream): the
                    // running block's lifecycle can no longer complete.
                    *self.in_flight.lock() = None;
                    *self.open_prompt.lock() = false;
                }

                HostEvent::PromptBoundaryTrusted(on)
            }
            TerminalEvent::PromptStarted => {
                *self.open_prompt.lock() = true;

                HostEvent::PromptStarted
            }
            TerminalEvent::BlockBatch(batch) => {
                // Stage this read's block events; they flush to the store on the
                // read's damage wake, after `UpdateGraphics` installs the generations
                // its slices bind to. No chrome/content wake here.
                self.staged_blocks.lock().extend(batch);

                return;
            }
            TerminalEvent::CommandStarted(cmd) => {
                *self.open_prompt.lock() = false;

                // Marry the command metadata to its block item; the
                // segment materializes later, when its rows scroll out.
                self.block_store
                    .lock()
                    .update_meta(cmd.seq, |m: &mut SegmentMeta| {
                        m.command = Some(cmd.command.clone());
                        m.cwd = cmd.cwd.as_ref().map(|p| p.to_string_lossy().into_owned());
                        m.started_at = Some(cmd.started_at);
                    });

                let block = InFlightBlock {
                    command: cmd.command,
                    started_at: cmd.started_at,
                };

                *self.in_flight.lock() = Some(block);

                HostEvent::CommandStarted
            }
            TerminalEvent::CommandFinished(cmd) => {
                *self.in_flight.lock() = None;

                self.block_store
                    .lock()
                    .update_meta(cmd.seq, |m: &mut SegmentMeta| {
                        m.exit_code = cmd.exit_code;
                        m.ended_at = Some(cmd.ended_at);
                    });

                debug!(
                    command = %cmd.command,
                    exit_code = ?cmd.exit_code,
                    "command block metadata recorded"
                );

                HostEvent::CommandFinished
            }
            _ => return,
        };

        self.events.lock().push_back(host);

        // A user-visible event changes chrome (tab title, status, attention).
        self.signal(Wake::Chrome(self.id));
    }
}
