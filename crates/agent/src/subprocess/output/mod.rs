use std::io::{self, BufRead};
use std::str::from_utf8;

use serde_json::Value;
use tracing::warn;

pub(super) const MAX_STDOUT_LINE: usize = 8 * 1024 * 1024;
pub(super) const MAX_STDERR_CHUNK: usize = 64 * 1024;
const MAX_STARTUP_LINES: usize = 8;
const MAX_STARTUP_BYTES: usize = 64 * 1024;

/// Startup wrappers may print plain-text notices before the first protocol
/// object. Once the protocol starts, skipping a malformed line could lose a
/// response or transcript item, so decoding failure ends the stream.
pub(super) fn read_messages(
    reader: &mut impl BufRead,
    provider: &str,
    mut deliver: impl FnMut(Value),
) -> Result<(), String> {
    let mut started = false;
    let mut first_line = true;
    let mut startup_lines = 0;
    let mut startup_bytes = 0usize;

    loop {
        let line = read_piece(reader, MAX_STDOUT_LINE)
            .map_err(|error| format!("Agent output read failed: {error}"))?;

        if line.is_empty() {
            return Ok(());
        }

        if line.len() == MAX_STDOUT_LINE && line.last() != Some(&b'\n') {
            return Err(
                "Agent output line exceeded the 8 MiB limit; the process was stopped.".into(),
            );
        }

        let raw = if first_line {
            line.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&line)
        } else {
            &line
        };

        first_line = false;

        let raw = raw.trim_ascii();

        if raw.is_empty() {
            continue;
        }

        match serde_json::from_slice::<Value>(raw) {
            Ok(message) if message.is_object() => {
                started = true;
                deliver(message);
            }
            Ok(_) => {
                return Err(
                    "Agent protocol output must be a JSON object; the process was stopped.".into(),
                );
            }
            Err(error) => {
                let bracketed_notice =
                    [b"[warn]".as_slice(), b"[warning]", b"[info]"]
                        .iter()
                        .any(|prefix| {
                            raw.get(..prefix.len())
                                .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
                        });

                let plain_startup = !started
                    && from_utf8(raw).is_ok()
                    && raw[0] != b'{'
                    && raw[0] != b'"'
                    && (raw[0] != b'[' || bracketed_notice);

                if !plain_startup {
                    // Parser error text can include input fragments. Only the
                    // category and numeric location are safe to report here.
                    return Err(format!(
                        "Agent protocol JSON is invalid ({:?}, line {}, column {}); the process was stopped.",
                        error.classify(),
                        error.line(),
                        error.column()
                    ));
                }

                startup_lines += 1;
                startup_bytes = startup_bytes.saturating_add(line.len());

                if startup_lines > MAX_STARTUP_LINES || startup_bytes > MAX_STARTUP_BYTES {
                    return Err("Agent startup output exceeded the non-protocol allowance; the process was stopped.".into());
                }

                if startup_lines == 1 {
                    warn!(
                        provider,
                        "agent launcher produced non-protocol startup output; content omitted"
                    );
                }
            }
        }
    }
}

/// Return one bounded piece, including a newline when present. A full piece
/// without a newline may continue in the next read; EOF returns an empty piece.
pub(super) fn read_piece(reader: &mut impl BufRead, limit: usize) -> io::Result<Vec<u8>> {
    let mut line = Vec::new();

    while line.len() < limit {
        let available = reader.fill_buf()?;

        if available.is_empty() {
            break;
        }

        let count = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index + 1)
            .min(limit - line.len());

        line.extend_from_slice(&available[..count]);
        reader.consume(count);

        if line.last() == Some(&b'\n') {
            break;
        }
    }

    Ok(line)
}

#[cfg(test)]
mod tests;
