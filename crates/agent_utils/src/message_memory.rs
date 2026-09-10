use std::mem::size_of;

use serde_json::Value;

/// Estimate retained JSON memory, including string capacity and container
/// overhead. This is a queue budget estimate, not an allocator measurement.
pub fn retained_bytes(value: &Value) -> usize {
    let heap = match value {
        Value::String(text) => text.capacity(),
        Value::Array(values) => values.iter().fold(
            values.capacity().saturating_mul(size_of::<Value>()),
            |total, value| total.saturating_add(retained_bytes(value)),
        ),
        Value::Object(values) => values.iter().fold(0usize, |total, (key, value)| {
            total
                .saturating_add(256)
                .saturating_add(key.capacity())
                .saturating_add(retained_bytes(value))
        }),
        _ => 0,
    };
    size_of::<Value>().saturating_add(heap)
}

/// Local notification used when a subprocess cannot safely continue reading.
pub const OUTPUT_FAILURE_METHOD: &str = "nmt/outputFailure";
