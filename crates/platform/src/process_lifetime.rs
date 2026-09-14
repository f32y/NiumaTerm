use std::process::Child;

use tracing::warn;

pub(crate) fn cleanup_failed_attachment(child: &mut Child) {
    match child.kill() {
        Ok(()) => {
            if let Err(error) = child.wait() {
                warn!(
                    "failed to reap child {} after containment failure: {error}",
                    child.id()
                );
            }
        }
        Err(error) => match child.try_wait() {
            Ok(Some(_)) => {}
            result => warn!(
                "failed to terminate child {} after containment failure: {error}; status: {result:?}",
                child.id()
            ),
        },
    }
}
