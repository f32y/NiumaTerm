//! Stream multiplexing inside a channel. Every sealed plaintext is one frame:
//!
//! ```text
//!  0         4      5       6
//!  +---------+------+-------+--------------------------+
//!  | stream  | kind | flags | payload (<= 65513 bytes) |
//!  | u32 LE  |  u8  |  u8   |                          |
//!  +---------+------+-------+--------------------------+
//! ```
//!
//! A message larger than one frame is split into fragments flagged `MORE`
//! except the last. Fragments of different streams may interleave, so a
//! large checkpoint on one stream does not hold back input on another.

pub mod kind {
    pub const CONTROL_JSON: u8 = 0x01;

    /// Liveness probe on stream 0, answered with `PONG` by the channel
    /// itself; the payload is echoed and otherwise meaningless.
    pub const PING: u8 = 0x02;

    pub const PONG: u8 = 0x03;

    /// Bounded fragments of a paged agent control response or update.
    pub const CONTROL_CHUNK: u8 = 0x04;

    pub const CHECKPOINT: u8 = 0x10;
    pub const OUTPUT: u8 = 0x11;
    pub const INPUT: u8 = 0x12;
    pub const EXIT: u8 = 0x13;
    pub const SIZE: u8 = 0x14;
}

use std::collections::HashMap;

use crate::Error;
use crate::channel::MAX_PLAINTEXT;

pub(crate) const HEADER_LEN: usize = 6;
pub(crate) const MAX_PAYLOAD: usize = MAX_PLAINTEXT - HEADER_LEN;

/// Largest reassembled message. A peer announcing more is broken or hostile,
/// and buffering it would let it exhaust memory.
pub(crate) const MAX_MESSAGE: usize = 32 * 1024 * 1024;

pub(crate) const FLAG_MORE: u8 = 0x01;

/// Stream 0 is reserved for control messages.
pub const CONTROL_STREAM: u32 = 0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Frame<'a> {
    pub stream: u32,
    pub kind: u8,
    pub flags: u8,
    pub payload: &'a [u8],
}

impl<'a> Frame<'a> {
    pub fn decode(bytes: &'a [u8]) -> Result<Self, Error> {
        if bytes.len() < HEADER_LEN {
            return Err(Error::Malformed("frame header"));
        }

        let payload = &bytes[HEADER_LEN..];

        if payload.len() > MAX_PAYLOAD {
            return Err(Error::TooLarge { limit: MAX_PAYLOAD });
        }

        Ok(Self {
            stream: u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            kind: bytes[4],
            flags: bytes[5],
            payload,
        })
    }
}

/// Split one message into encoded frames, each ready to seal.
pub fn encode_message(stream: u32, kind: u8, message: &[u8]) -> Vec<Vec<u8>> {
    let mut chunks: Vec<&[u8]> = message.chunks(MAX_PAYLOAD).collect();

    if chunks.is_empty() {
        chunks.push(&[]);
    }

    let last = chunks.len() - 1;

    chunks
        .into_iter()
        .enumerate()
        .map(|(index, chunk)| {
            let flags = if index == last { 0 } else { FLAG_MORE };

            let mut frame = Vec::with_capacity(HEADER_LEN + chunk.len());

            frame.extend_from_slice(&stream.to_le_bytes());
            frame.push(kind);
            frame.push(flags);
            frame.extend_from_slice(chunk);

            frame
        })
        .collect()
}

/// A complete message reassembled from one stream's fragments.
#[derive(Debug, PartialEq, Eq)]
pub struct Message {
    pub stream: u32,
    pub kind: u8,
    pub payload: Vec<u8>,
}

/// Collects fragments per stream until a frame without `MORE` completes the
/// message.
#[derive(Default)]
pub struct Reassembler {
    partial: HashMap<u32, (u8, Vec<u8>)>,
}

impl Reassembler {
    /// Add one frame. Returns the message it completes, if any. An error
    /// means the peer violated framing and the channel should close.
    pub fn push(&mut self, frame: Frame<'_>) -> Result<Option<Message>, Error> {
        let (kind, mut payload) = match self.partial.remove(&frame.stream) {
            Some((kind, _)) if kind != frame.kind => {
                return Err(Error::Malformed("fragment kind"));
            }
            Some(partial) => partial,
            None => (frame.kind, Vec::new()),
        };

        if payload.len() + frame.payload.len() > MAX_MESSAGE {
            return Err(Error::TooLarge { limit: MAX_MESSAGE });
        }

        payload.extend_from_slice(frame.payload);

        if frame.flags & FLAG_MORE != 0 {
            self.partial.insert(frame.stream, (kind, payload));

            return Ok(None);
        }

        Ok(Some(Message {
            stream: frame.stream,
            kind,
            payload,
        }))
    }
}
