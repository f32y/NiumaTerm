use std::io::BufRead;
use std::str::from_utf8;

use serde_json::Value;
use tracing::warn;

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
    let mut startup_notice_seen = false;
    let mut line = Vec::new();

    loop {
        line.clear();
        reader
            .read_until(b'\n', &mut line)
            .map_err(|error| format!("Agent output read failed: {error}"))?;

        if line.is_empty() {
            return Ok(());
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

                if !startup_notice_seen {
                    startup_notice_seen = true;
                    warn!(
                        provider,
                        "agent launcher produced non-protocol startup output; content omitted"
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
