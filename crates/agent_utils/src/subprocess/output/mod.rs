use std::io::{self, BufRead};

pub(super) const MAX_STDOUT_LINE: usize = 8 * 1024 * 1024;
pub(super) const MAX_STDERR_CHUNK: usize = 64 * 1024;

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
