use std::sync::Arc;
use std::time::Instant;
use std::{io, thread};

use parking_lot::{Condvar, Mutex};

#[derive(Default)]
struct State {
    next: Option<Instant>,
    stopped: bool,
}

#[derive(Clone)]
pub(crate) struct TimerHandle(Arc<(Mutex<State>, Condvar)>);

impl TimerHandle {
    pub(crate) fn set(&self, next: Option<Instant>) {
        let (state, wake) = &*self.0;
        let mut state = state.lock();

        if !state.stopped && state.next != next {
            state.next = next;
            wake.notify_one();
        }
    }

    pub(crate) fn stop(&self) {
        let (state, wake) = &*self.0;

        state.lock().stopped = true;
        wake.notify_one();
    }
}

pub(crate) struct DeadlineTimer {
    handle: TimerHandle,
}

impl DeadlineTimer {
    pub(crate) fn new(callback: impl Fn() + Send + 'static) -> io::Result<Self> {
        let handle = TimerHandle(Arc::new((Mutex::new(State::default()), Condvar::new())));
        let worker = handle.clone();

        thread::Builder::new()
            .name("agent-deadlines".into())
            .spawn(move || {
                let (state, wake) = &*worker.0;
                let mut state = state.lock();

                loop {
                    if state.stopped {
                        break;
                    }

                    match state.next {
                        None => wake.wait(&mut state),

                        Some(next) if next > Instant::now() => {
                            wake.wait_until(&mut state, next);
                        }

                        Some(_) => {
                            state.next = None;

                            // The callback can re-arm the timer while resolving requests.
                            drop(state);
                            callback();
                            state = worker.0.0.lock();
                        }
                    }
                }
            })?;

        Ok(Self { handle })
    }

    pub(crate) fn handle(&self) -> TimerHandle {
        self.handle.clone()
    }

    pub(crate) fn set(&self, next: Option<Instant>) {
        self.handle.set(next);
    }
}

impl Drop for DeadlineTimer {
    fn drop(&mut self) {
        self.handle.stop();
    }
}

#[cfg(test)]
mod tests;
