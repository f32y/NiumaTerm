use std::fmt;

use futures::channel::oneshot;

use crate::ghostty::BlockHandle;
use crate::graphics::GraphicData;
use crate::selection::SelectionType;
use crate::session::page::{PageSource, RowPage};

pub type Request<T> = oneshot::Receiver<Result<T, RequestError>>;

pub type Reply<T> = oneshot::Sender<Result<T, RequestError>>;

pub type BlockRange = ((usize, u32), (usize, u32));

#[derive(Debug, Clone)]
pub enum RequestError {
    Stale,
    Unavailable,
    Engine(String),
}

#[derive(Debug)]
pub struct TextPiece {
    pub handle: BlockHandle,
    pub start: Option<(usize, u32)>,
    pub end: Option<(usize, u32)>,
}

#[derive(Debug)]
pub enum TextSource {
    Screen {
        revision: u64,
        start: (u16, u32),
        end: (u16, u32),
        rectangle: bool,
    },

    Blocks(Vec<TextPiece>),

    BlockSelection {
        handle: BlockHandle,
        line: usize,
        col: u32,
        kind: SelectionType,
    },
}

#[derive(Debug)]
pub enum Query {
    Image {
        handle: BlockHandle,
        image_id: u32,
        reply: Reply<GraphicData>,
    },

    Rows {
        source: PageSource,
        start: usize,
        reply: Reply<RowPage>,
    },

    Text {
        source: TextSource,
        reply: Reply<String>,
    },

    ExpandSelection {
        handle: BlockHandle,
        line: usize,
        col: u32,
        kind: SelectionType,
        reply: Reply<BlockRange>,
    },
}

#[derive(Debug)]
pub struct Checkpoint {
    pub vt: Vec<u8>,
    pub cols: u16,
    pub rows: u16,
}

/// Completion runs on the owner thread before any later output is parsed.
/// It may register a stream subscriber but must not wait for another thread.
pub struct CheckpointRequest(pub Box<dyn FnOnce(Result<Checkpoint, RequestError>) + Send>);

impl fmt::Debug for CheckpointRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CheckpointRequest")
    }
}
