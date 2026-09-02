use std::{ptr, slice};

use libghostty_vt_sys::{
    BlockFormatOptions as VtBlockFormatOptions, BlockRef as VtBlockRef, ColorRgb as VtColorRgb,
    GridRef as VtGridRef, KittyGraphics as VtKittyGraphics, Result as VtResult,
    TerminalOption as VtTerminalOption, ghostty_block_ref_bytes, ghostty_block_ref_cols,
    ghostty_block_ref_format_alloc, ghostty_block_ref_grid_ref, ghostty_block_ref_handle,
    ghostty_block_ref_kitty_graphics, ghostty_block_ref_release, ghostty_block_ref_row_count,
    ghostty_free, ghostty_terminal_block_acquire, ghostty_terminal_block_at,
    ghostty_terminal_block_cols, ghostty_terminal_block_count, ghostty_terminal_block_grid_ref,
    ghostty_terminal_block_row_count, ghostty_terminal_blocks_bytes, ghostty_terminal_clear_blocks,
    ghostty_terminal_finish_block, ghostty_terminal_remove_block, ghostty_terminal_set,
    sized as vt_sized,
};

use crate::ghostty::grid_read::visit_row_cells;
use crate::ghostty::{
    BlockHandle, CellText, CellWide, Error, GhosttyTerminal, Palette, PlacementScreenPos, Result,
    ScreenRowMeta, SnapshotStyle,
};
#[cfg(test)]
use crate::ghostty::{RowCell, ScreenRowRead};

/// An acquired read reference to a finished block (engine-refcounted).
///
/// Pins an immutable snapshot of the block: while held, the block cannot
/// be freed (removal/eviction defer destruction) or mutated (reflow
/// drains readers first), and every read here takes no engine lock — the
/// render thread can read frozen blocks while the PTY thread is inside a
/// `write_vt` burst. Released on drop.
///
/// Keep references short-lived (one read pass, e.g. a frame): a held
/// reference blocks the engine's resize reflow of this block.
pub struct BlockRef {
    pub(super) raw: VtBlockRef,
    pub(super) cols: u16,
}

// SAFETY: the engine's block_ref API is explicitly any-thread —
// acquire/release and all block_ref_* readers synchronize internally
// (refcount under the block-set mutex; the pinned data is immutable).
unsafe impl Send for BlockRef {}
unsafe impl Sync for BlockRef {}

impl BlockRef {
    /// The `(id, generation)` of the pinned snapshot — the stable cache
    /// key for shaped/rendered rows.
    pub fn handle(&self) -> BlockHandle {
        let mut handle = BlockHandle::default();

        unsafe {
            let _ = ghostty_block_ref_handle(self.raw, &mut handle);
        }

        handle
    }

    /// Logical row count of the snapshot.
    pub fn row_count(&self) -> usize {
        let mut rows: usize = 0;

        unsafe {
            let _ = ghostty_block_ref_row_count(self.raw, &mut rows);
        }

        rows
    }

    /// Column count of the snapshot (the width it is currently laid out
    /// at).
    pub fn cols(&self) -> u16 {
        self.cols
    }

    /// Page-storage bytes of the snapshot.
    pub fn bytes(&self) -> usize {
        let mut bytes: usize = 0;

        unsafe {
            let _ = ghostty_block_ref_bytes(self.raw, &mut bytes);
        }

        bytes
    }

    /// Walk one row of the snapshot with styles — same visitor shape as
    /// [`GhosttyTerminal::read_screen_row_visit`], but without the engine
    /// lock. `None` for a row at/beyond the logical row count.
    pub fn read_row_visit(
        &self,
        row: usize,
        palette: &[VtColorRgb; 256],
        on_cell: impl FnMut(u16, CellText, CellWide, SnapshotStyle),
    ) -> Result<Option<ScreenRowMeta>> {
        let mut grid_ref = VtGridRef::default();

        match unsafe { ghostty_block_ref_grid_ref(self.raw, row, &mut grid_ref) } {
            VtResult::SUCCESS => {}
            VtResult::INVALID_VALUE => return Ok(None),
            other => {
                Error::from_code(other)?;
                return Ok(None);
            }
        }

        Ok(Some(visit_row_cells(
            grid_ref, self.cols, palette, on_cell,
        )?))
    }

    /// The snapshot's Kitty graphics storage handle, for placement
    /// iteration and lazy pixel upload of frozen images. Valid while this
    /// reference is held. `None` if kitty graphics are disabled at build
    /// time.
    pub fn kitty_graphics_raw(&self) -> Option<VtKittyGraphics> {
        let mut graphics: VtKittyGraphics = ptr::null_mut();

        (unsafe { ghostty_block_ref_kitty_graphics(self.raw, &mut graphics) } == VtResult::SUCCESS
            && !graphics.is_null())
        .then_some(graphics)
    }

    /// [`Self::format_range`] with caller-friendly endpoints: `None` means
    /// the block edge, and rows/columns clamp into the snapshot's bounds —
    /// the shape a selection copy produces. `None` for an
    /// empty block.
    pub fn format_range_clamped(
        &self,
        start: Option<(usize, u32)>,
        end: Option<(usize, u32)>,
        unwrap: bool,
        trim: bool,
    ) -> Option<String> {
        let rows = self.row_count();

        if rows == 0 {
            return None;
        }

        let last_col = u32::from(self.cols().saturating_sub(1));

        let clamp = |(row, col): (usize, u32)| {
            (
                row.min(rows - 1),
                col.min(last_col).min(u16::MAX as u32) as u16,
            )
        };

        let tl = clamp(start.unwrap_or((0, 0)));
        let br = clamp(end.unwrap_or((rows - 1, last_col)));

        self.format_range(tl, br, unwrap, trim).ok()
    }

    /// Export an inclusive cell range of the snapshot as plain text — the
    /// copy/deep-search floor. Cross-block copy concatenates per-block
    /// exports so no cross-block engine lock is needed.
    pub fn format_range(
        &self,
        tl: (usize, u16),
        br: (usize, u16),
        unwrap: bool,
        trim: bool,
    ) -> Result<String> {
        let mut opts = vt_sized!(VtBlockFormatOptions);

        opts.tl_row = tl.0;
        opts.tl_col = tl.1;
        opts.br_row = br.0;
        opts.br_col = br.1;
        opts.unwrap = unwrap;
        opts.trim = trim;

        let mut out_ptr: *mut u8 = ptr::null_mut();
        let mut out_len: usize = 0;

        Error::from_code(unsafe {
            ghostty_block_ref_format_alloc(self.raw, ptr::null(), opts, &mut out_ptr, &mut out_len)
        })?;

        let text = if out_ptr.is_null() || out_len == 0 {
            String::new()
        } else {
            let bytes = unsafe { slice::from_raw_parts(out_ptr, out_len) };

            String::from_utf8_lossy(bytes).into_owned()
        };

        if !out_ptr.is_null() {
            unsafe { ghostty_free(ptr::null(), out_ptr, out_len) };
        }

        Ok(text)
    }
}

impl Drop for BlockRef {
    fn drop(&mut self) {
        unsafe { ghostty_block_ref_release(self.raw) }
    }
}

/// One acquired frozen block, bundled with everything a read pass needs
/// from under the engine lock: the pinned reference, the palette its styles
/// resolve against, and its Kitty placements in block-relative coordinates
/// so callers can finish reading after releasing the engine lock. Produced by
/// [`GhosttyTerminal::acquire_block_snapshot`].
pub struct AcquiredBlock {
    pub block: BlockRef,
    pub palette: Palette,
    pub placements: Vec<PlacementScreenPos>,
}

impl GhosttyTerminal {
    /// Finish the current command block: freeze the primary screen into the
    /// engine's block set (O(1) ownership move) and continue on a fresh
    /// primary screen with writer state carried over. Returns `None` when
    /// the active screen has no content (no block created). Errors with
    /// `InvalidValue` if the alternate screen is active — callers gate on
    /// the primary screen because alternate-screen content should not enter history.
    pub fn finish_block(&mut self) -> Result<Option<BlockHandle>> {
        let mut handle = BlockHandle::default();

        match unsafe { ghostty_terminal_finish_block(self.terminal, &mut handle) } {
            VtResult::SUCCESS => Ok(Some(handle)),
            VtResult::NO_VALUE => Ok(None),
            other => {
                Error::from_code(other)?;
                Ok(None)
            }
        }
    }

    /// Remove and destroy all finished blocks (user clear; `;K` path).
    pub fn clear_blocks(&mut self) {
        unsafe { ghostty_terminal_clear_blocks(self.terminal) }
    }

    /// Remove and destroy one finished block. Returns `false` for a stale
    /// handle (already removed/evicted).
    pub fn remove_block(&mut self, handle: BlockHandle) -> bool {
        (unsafe { ghostty_terminal_remove_block(self.terminal, handle) }) == VtResult::SUCCESS
    }

    pub fn block_count(&self) -> usize {
        unsafe { ghostty_terminal_block_count(self.terminal) }
    }

    /// The handle of the finished block at `index`, oldest first.
    pub fn block_at(&self, index: usize) -> Option<BlockHandle> {
        let mut handle = BlockHandle::default();

        (unsafe { ghostty_terminal_block_at(self.terminal, index, &mut handle) }
            == VtResult::SUCCESS)
            .then_some(handle)
    }

    /// Logical row count of a finished block (trailing blanks after the
    /// finish-time cursor truncated). `None` for a stale handle.
    pub fn block_row_count(&self, handle: BlockHandle) -> Option<usize> {
        let mut rows: usize = 0;

        (unsafe { ghostty_terminal_block_row_count(self.terminal, handle, &mut rows) }
            == VtResult::SUCCESS)
            .then_some(rows)
    }

    /// The column count the block was frozen at (can differ from the live
    /// terminal width after a resize). `None` for a stale handle.
    pub fn block_cols(&self, handle: BlockHandle) -> Option<u16> {
        let mut cols: u16 = 0;

        (unsafe { ghostty_terminal_block_cols(self.terminal, handle, &mut cols) }
            == VtResult::SUCCESS)
            .then_some(cols)
    }

    /// Total page-storage bytes of all finished blocks — the value the
    /// block byte budget is enforced against.
    pub fn blocks_bytes(&self) -> usize {
        unsafe { ghostty_terminal_blocks_bytes(self.terminal) }
    }

    /// Set the finished-block byte budget. Oldest blocks are evicted
    /// immediately (and on every finish) while the total exceeds it; the
    /// newest block is never evicted. Zero means unlimited.
    pub fn set_block_budget_bytes(&mut self, bytes: usize) -> Result<()> {
        Error::from_code(unsafe {
            ghostty_terminal_set(
                self.terminal,
                VtTerminalOption::BLOCK_BUDGET_BYTES,
                (&bytes as *const usize).cast(),
            )
        })
    }

    /// Take a read reference on a finished block (engine-refcounted; any
    /// thread). `None` for a stale handle or while the engine is
    /// reflowing the block — retry next frame. The reference pins an
    /// immutable snapshot: the block cannot be freed or mutated while it
    /// is held, and reads through it take no engine lock. Keep it
    /// short-lived (one read pass) — a held reference blocks the writer's
    /// resize reflow.
    pub fn block_acquire(&self, handle: BlockHandle) -> Option<BlockRef> {
        let mut raw: VtBlockRef = ptr::null_mut();

        if unsafe { ghostty_terminal_block_acquire(self.terminal, handle, &mut raw) }
            != VtResult::SUCCESS
            || raw.is_null()
        {
            return None;
        }

        let mut cols: u16 = 0;

        unsafe {
            let _ = ghostty_block_ref_cols(raw, &mut cols);
        }

        Some(BlockRef { raw, cols })
    }

    /// [`Self::block_acquire`] plus everything a frame's read pass needs
    /// from under the engine lock in one call: the palette styles resolve
    /// against and the block's Kitty placements in block-relative
    /// coordinates. Every subsequent text read through the
    /// returned reference is lock-free.
    pub fn acquire_block_snapshot(&mut self, handle: BlockHandle) -> Option<AcquiredBlock> {
        let block = self.block_acquire(handle)?;
        let palette = self.color_palette();
        let placements = self.block_placements(&block);

        Some(AcquiredBlock {
            block,
            palette,
            placements,
        })
    }

    /// Walk one row of a finished block with styles — the frozen-block
    /// counterpart of [`Self::read_screen_row_visit`]. Returns `None` for a
    /// stale handle or a row at/beyond the block's logical row count.
    /// Unlike active-screen refs, block refs stay valid until the block is
    /// removed, but this still reads within one call (same visitor shape).
    pub fn read_block_row_visit(
        &self,
        handle: BlockHandle,
        row: usize,
        palette: &[VtColorRgb; 256],
        on_cell: impl FnMut(u16, CellText, CellWide, SnapshotStyle),
    ) -> Result<Option<ScreenRowMeta>> {
        let mut grid_ref = VtGridRef::default();

        match unsafe { ghostty_terminal_block_grid_ref(self.terminal, handle, row, &mut grid_ref) }
        {
            VtResult::SUCCESS => {}
            VtResult::NO_VALUE | VtResult::INVALID_VALUE => return Ok(None),
            other => {
                Error::from_code(other)?;
                return Ok(None);
            }
        }

        let cols = self.block_cols(handle).unwrap_or(self.cols);

        Ok(Some(visit_row_cells(grid_ref, cols, palette, on_cell)?))
    }

    /// Materializing convenience over [`Self::read_block_row_visit`] — test-only.
    #[cfg(test)]
    pub fn read_block_row(&self, handle: BlockHandle, row: usize) -> Result<Option<ScreenRowRead>> {
        let palette = self.color_palette();
        let cols = self.block_cols(handle).unwrap_or(self.cols) as usize;
        let mut cells = Vec::with_capacity(cols);

        let meta = self.read_block_row_visit(handle, row, &palette, |x, text, wide, style| {
            cells.push(RowCell {
                x,
                text,
                wide,
                style,
            })
        })?;

        Ok(meta.map(|meta| ScreenRowRead {
            cells,
            wrapped: meta.wrapped,
            prompt_start: meta.prompt_start,
            hyperlinks: meta.hyperlinks,
        }))
    }
}
