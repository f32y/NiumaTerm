use std::time::{Duration, Instant};

use crate::pty_pipe::powershell_compatibility::{PowerShellCompatibility, RESIZE_INPUT_DELAY};

#[test]
fn resize_pauses_expire_and_interleaved_input_has_a_fixed_wait_limit() {
    let now = Instant::now();

    let mut compatibility = PowerShellCompatibility::default();

    compatibility.set_enabled(true);

    let limit = compatibility.input_limit(now);

    assert_eq!(compatibility.timeout(limit, now), None);

    compatibility.resized(now);

    assert_eq!(compatibility.timeout(limit, now), Some(RESIZE_INPUT_DELAY));
    assert_eq!(
        compatibility.timeout(limit, now + RESIZE_INPUT_DELAY),
        Some(Duration::ZERO)
    );

    let later = now + Duration::from_millis(240);

    compatibility.resized(later);

    assert_eq!(
        compatibility.timeout(limit, later),
        Some(Duration::from_millis(10))
    );
    assert_eq!(
        compatibility.timeout(limit, later + RESIZE_INPUT_DELAY),
        Some(Duration::ZERO)
    );

    compatibility.set_enabled(false);

    assert_eq!(compatibility.timeout(limit, now), None);
    assert_eq!(compatibility.input_limit(now), None);

    compatibility.set_enabled(true);

    assert_eq!(compatibility.timeout(limit, now), None);
}
