use crate::pty_pipe::scrollback_bytes;

/// The engine scrollback budget is derived from the config line limit
/// (not the old hardcoded 10 MB) — proportional to lines × cols, 0 → 0.
#[test]
fn scrollback_bytes_from_config() {
    // Default 10k lines @ 80 cols → ~12.8 MB (config-driven, ≈ the old 10 MB).
    assert_eq!(scrollback_bytes(10_000, 80), 10_000 * 80 * 16);
    // Scales with the configured line count.
    assert!(scrollback_bytes(100_000, 80) > scrollback_bytes(10_000, 80));
    // Scales with width (byte budget, not lines).
    assert!(scrollback_bytes(10_000, 200) > scrollback_bytes(10_000, 80));
    // Disabled scrollback → 0 budget.
    assert_eq!(scrollback_bytes(0, 80), 0);
    // No overflow on absurd input.
    assert_eq!(scrollback_bytes(usize::MAX, 80), usize::MAX);
}
