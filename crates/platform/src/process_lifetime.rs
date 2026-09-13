use std::io;
use std::process::Child;

use crate::process::{KillOnCloseJob, ProcessTree};

impl KillOnCloseJob {
    pub fn attach_or_kill(child: &mut Child) -> io::Result<Self> {
        Self::attach(child).inspect_err(|_| {
            let _ = child.kill();
            let _ = child.wait();
        })
    }
}

impl ProcessTree {
    pub fn other_process_count(&self) -> usize {
        self.process_count().saturating_sub(1)
    }
}
