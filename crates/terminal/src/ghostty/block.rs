use std::{ptr, slice};

use libghostty_vt_sys::{
    BlockFormatOptions as VtBlockFormatOptions, BlockRef as VtBlockRef, ColorRgb as VtColorRgb,
    GridRef as VtGridRef, KittyGraphics as VtKittyGraphics, Result as VtResult,
    ghostty_block_ref_bytes, ghostty_block_ref_format_alloc, ghostty_block_ref_grid_ref,
    ghostty_block_ref_handle, ghostty_block_ref_kitty_graphics, ghostty_block_ref_release,
    ghostty_block_ref_row_count, ghostty_free, sized as vt_sized,
};

#[cfg(doc)]
use crate::ghostty::GhosttyTerminal;
use crate::ghostty::grid_read::visit_row_cells;
use crate::ghostty::{
    BlockHandle, CellText, CellWide, Error, Palette, PlacementScreenPos, Result, ScreenRowMeta,
    SnapshotStyle,
};

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
    ) -> Option<String> {
        let rows = self.row_count();

        if rows == 0 {
            return None;
        }

        let last_col: u32 = self.cols().saturating_sub(1).into();

        let clamp = |(row, col): (usize, u32)| {
            (
                row.min(rows - 1),
                col.min(last_col).min(u16::MAX as u32) as u16,
            )
        };

        let tl = clamp(start.unwrap_or((0, 0)));
        let br = clamp(end.unwrap_or((rows - 1, last_col)));

        self.format_range(tl, br, true, true).ok()
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
