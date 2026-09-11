use std::mem;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use tracing::debug;

use crate::block_store::SegmentMeta;
use crate::event::{EventListener, TerminalEvent, WindowId};
use crate::session::{
    HostEvent, InFlightBlock, SessionChange, SessionObserver, SessionSharedState,
};

#[derive(Clone)]
pub(super) struct TerminalEventProxy {
    shared: Arc<SessionSharedState>,

    /// Source surface id, stamped onto every wake so the shell can route by tab.
    id: u64,

    /// Render-wakeup sender; `None` for sessions/tests without a live shell.
    observer: Option<Arc<dyn SessionObserver>>,
}

impl TerminalEventProxy {
    pub(super) fn new(
        shared: Arc<SessionSharedState>,
        id: u64,
        observer: Option<Arc<dyn SessionObserver>>,
    ) -> Self {
        Self {
            shared,
            id,
            observer,
        }
    }

    fn signal(&self, kind: SessionChange) {
        if let Some(observer) = &self.observer {
            observer.changed(kind);
        }
    }

    /// Flush the current read's staged block events into the block store.
    /// Called on the read's damage wake so items land together with the
    /// render they belong to. Empty in steady state.
    fn flush_staged_blocks(&self) {
        let batch = mem::take(&mut *self.shared.staged_blocks.lock());

        if batch.is_empty() {
            return;
        }

        if let Some(observer) = &self.observer {
            observer.blocks(&batch);
        }

        self.shared.block_store.lock().apply(batch);
    }
}

impl EventListener for TerminalEventProxy {
    fn event(&self) -> (Option<TerminalEvent>, bool) {
        (None, false)
    }

    fn send_event(&self, event: TerminalEvent, _id: WindowId) {
        if matches!(event, TerminalEvent::ReadReady) {
            self.signal(SessionChange::Content);

            return;
        }

        // Content damage drives a render with no chrome rebuild. This is the read's
        // final wake: image generations have already installed (UpdateGraphics runs
        // before this), so flush the staged block events before the UI
        // wakes.
        if matches!(
            event,
            TerminalEvent::TerminalDamaged(_) | TerminalEvent::Render
        ) {
            self.flush_staged_blocks();
            self.signal(SessionChange::Content);

            return;
        }

        // Decoded Kitty pixels: install/replace live generations and drop removed
        // ones, then wake for a lazy visible upload. Route-scoped so a cross-session
        // event is ignored.
        if let TerminalEvent::UpdateGraphics { route_id, queues } = event {
            if route_id != self.id as usize {
                return;
            }

            if let Some(observer) = &self.observer {
                observer.graphics(queues);
            }

            self.signal(SessionChange::Content);

            return;
        }

        let host = match event {
            TerminalEvent::Title(t) | TerminalEvent::TitleWithSubtitle(t, _) => HostEvent::Title(t),
            TerminalEvent::ResetTitle => HostEvent::Title(String::new()),
            TerminalEvent::Bell => HostEvent::Bell,
            TerminalEvent::Cwd(cwd) => HostEvent::Cwd(cwd),
            TerminalEvent::ProgressReport(report) => HostEvent::Progress(report),

            TerminalEvent::ClipboardStore(ty, text) => {
                if let Some(observer) = &self.observer {
                    observer.clipboard(ty, text);
                }

                return;
            }

            TerminalEvent::CloseTerminal(_) => {
                self.shared.exited.store(true, Ordering::Release);
                self.shared.selection.clear();
                // The shell died: no ;D is coming for a running command, and any
                // half-staged block batch for the interrupted read is discarded.
                *self.shared.in_flight.lock() = None;

                *self.shared.open_prompt.lock() = false;

                self.shared.staged_blocks.lock().clear();

                HostEvent::Exit
            }

            TerminalEvent::DesktopNotification { title, body } => {
                HostEvent::Notification { title, body }
            }

            TerminalEvent::InteractiveState(on) => HostEvent::InteractiveState(on),

            TerminalEvent::AltScreen(on) => {
                self.shared.alt_screen.store(on, Ordering::Release);

                HostEvent::AltScreen(on)
            }

            TerminalEvent::PromptBoundaryTrusted(on) => {
                if !on {
                    // Trust lost mid-command (nested shell, malformed stream): the
                    // running block's lifecycle can no longer complete.
                    *self.shared.in_flight.lock() = None;
                    *self.shared.open_prompt.lock() = false;
                }

                HostEvent::PromptBoundaryTrusted(on)
            }

            TerminalEvent::PromptStarted => {
                *self.shared.open_prompt.lock() = true;

                HostEvent::PromptStarted
            }

            TerminalEvent::BlockBatch(batch) => {
                // Stage this read's block events; they flush to the store on the
                // read's damage wake, after `UpdateGraphics` installs the generations
                // its slices bind to. No chrome/content wake here.
                self.shared.staged_blocks.lock().extend(batch);

                return;
            }

            TerminalEvent::CommandStarted(cmd) => {
                *self.shared.open_prompt.lock() = false;

                // Marry the command metadata to its block item; the
                // segment materializes later, when its rows scroll out.
                self.shared
                    .block_store
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

                *self.shared.in_flight.lock() = Some(block);

                HostEvent::CommandStarted
            }

            TerminalEvent::CommandFinished(cmd) => {
                self.shared.selection.clear();
                *self.shared.in_flight.lock() = None;

                self.shared
                    .block_store
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

                HostEvent::CommandFinished {
                    exit_code: cmd.exit_code,
                }
            }

            _ => return,
        };

        self.shared.events.lock().push_back(host);

        // A user-visible event changes chrome (tab title, status, attention).
        self.signal(SessionChange::HostEvents);
    }
}
