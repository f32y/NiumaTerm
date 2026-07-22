//! Frozen block-split history backed by engine-owned blocks.
//!
//! The [`BlockStore`] consumes the PTY thread's [`BlockEvent`] stream and
//! keeps one item per finished command: metadata plus the engine block's
//! HANDLE. The block's content stays in the engine — rendering acquires a
//! refcounted `BlockRef` and reads rows/images directly, so the store
//! retains ~695 B/row of engine pages instead of materialized lines.
//!
//! Metadata is married to items by the OSC 133 mark sequence number; marks
//! (`;C` command/cwd, `;D` exit code) fire before the block finishes, so
//! metadata is stashed in `pending_meta` until the `EngineBlock` event
//! materializes the item. The metadata itself is written by the embedder's
//! event handling (`CommandStarted`/`CommandFinished` carry it), not by
//! this crate. Memory is bounded engine-side (the block byte budget evicts
//! oldest blocks); `EngineBlocksSync` mirrors those evictions into the
//! item list.

use std::collections::HashMap;
use std::time::SystemTime;

use crate::event::BlockEvent;
use crate::ghostty::BlockHandle;

/// Command metadata for one prompt segment, filled in as OSC 133 marks
/// arrive (`;A` registers the sequence, `;C` brings command/cwd, `;D` brings
/// the exit code).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SegmentMeta {
    pub command: Option<String>,
    pub cwd: Option<String>,
    pub exit_code: Option<i32>,
    pub started_at: Option<SystemTime>,
    pub ended_at: Option<SystemTime>,
}

/// One finished command of the frozen history list: metadata + the engine
/// block backing it. Born complete — no content ever streams into an item.
#[derive(Debug)]
pub struct BlockItem {
    pub seq: Option<u64>,
    pub meta: SegmentMeta,
    /// The finished engine block. Rendering acquires a `BlockRef` through
    /// it; `rows` caches the engine's current row count (refreshed by
    /// `EngineBlocksSync`) so layout never takes the engine lock.
    handle: BlockHandle,
    rows: usize,
}

impl BlockItem {
    /// The engine block backing this item, with the current generation for
    /// shaped-row cache keys.
    pub fn handle(&self) -> Option<BlockHandle> {
        Some(self.handle)
    }

    /// Cached engine row count (already wrapped at the current width — the
    /// engine reflows blocks eagerly on resize).
    pub fn engine_rows(&self) -> usize {
        self.rows
    }
}

#[derive(Debug, Default)]
pub struct BlockStore {
    items: Vec<BlockItem>,
    /// Metadata that arrived before its block finished (the normal order:
    /// marks fire at write time, the block at `;D`).
    pending_meta: HashMap<u64, SegmentMeta>,
    /// Items dropped from the front (engine budget eviction), so a list UI
    /// can splice instead of resetting scroll state.
    pub evicted_items: u64,
}

impl BlockStore {
    pub fn items(&self) -> &[BlockItem] {
        &self.items
    }

    /// Apply one PTY-thread block batch.
    pub fn apply(&mut self, events: impl IntoIterator<Item = BlockEvent>) {
        for event in events {
            match event {
                // A trusted `;D` froze the command into a finished engine
                // block; the item is born complete.
                BlockEvent::EngineBlock { seq, handle, rows } => {
                    let mut item = BlockItem {
                        seq: Some(seq),
                        meta: SegmentMeta::default(),
                        handle,
                        rows,
                    };

                    if let Some(meta) = self.pending_meta.remove(&seq) {
                        item.meta = meta;
                    }

                    self.items.push(item);
                }
                // Prune items whose engine block is gone (byte-budget
                // eviction is oldest-first, so removals are a prefix — the
                // eviction counter keeps list splicing aligned) and refresh
                // cached rows + generation after a reflow.
                BlockEvent::EngineBlocksSync(live) => {
                    let by_id: HashMap<u64, (BlockHandle, usize)> =
                        live.into_iter().map(|(h, r)| (h.id, (h, r))).collect();

                    let evicted = &mut self.evicted_items;

                    self.items
                        .retain_mut(|item| match by_id.get(&item.handle.id) {
                            Some(&(fresh, rows)) => {
                                item.handle = fresh;
                                item.rows = rows;
                                true
                            }
                            None => {
                                *evicted += 1;
                                false
                            }
                        });
                }
                // The user cleared the terminal (`;K` in-band mark): the
                // whole frozen history drops with the screen (the PTY side
                // already cleared the engine blocks).
                BlockEvent::HistoryCleared => {
                    self.items.clear();
                }
            }
        }
    }

    /// Attach or update command metadata for the segment registered under
    /// `seq`. Applied to the materialized item when it exists, stashed for
    /// its future `EngineBlock` otherwise.
    pub fn update_meta(&mut self, seq: u64, update: impl FnOnce(&mut SegmentMeta)) {
        if let Some(item) = self
            .items
            .iter_mut()
            .rev()
            .find(|item| item.seq == Some(seq))
        {
            update(&mut item.meta);
        } else {
            update(self.pending_meta.entry(seq).or_default());
        }
    }
}
