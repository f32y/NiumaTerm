use std::{ptr, slice, time};

use libghostty_vt_sys::{
    KittyGraphics as VtKittyGraphics, KittyGraphicsData as VtKittyGraphicsData,
    KittyGraphicsImage as VtKittyGraphicsImage, KittyGraphicsImageData as VtKittyGraphicsImageData,
    KittyGraphicsPlacementData as VtKittyGraphicsPlacementData,
    KittyGraphicsPlacementIterator as VtKittyGraphicsPlacementIterator,
    KittyImageFormat as VtKittyImageFormat, Result as VtResult, Terminal as VtTerminal,
    TerminalData as VtTerminalData, ghostty_block_ref_placement_pos, ghostty_kitty_graphics_get,
    ghostty_kitty_graphics_image, ghostty_kitty_graphics_image_get,
    ghostty_kitty_graphics_placement_get, ghostty_kitty_graphics_placement_grid_size,
    ghostty_kitty_graphics_placement_next, ghostty_kitty_graphics_placement_pixel_size,
    ghostty_kitty_graphics_placement_source_rect, ghostty_kitty_graphics_placement_viewport_pos,
    ghostty_terminal_get,
};
use rustc_hash::FxHashSet;

use crate::ghostty::{BlockRef, GhosttyTerminal, PlacementScreenPos, SnapshotPlacement};
use crate::graphics;

impl GhosttyTerminal {
    /// Walk the engine's kitty-graphics placements into owned `SnapshotPlacement`s
    /// Re-points the persistent iterator at the live storage (no alloc),
    /// then for each placement reads the scalar fields and — for non-virtual visible
    /// placements — the viewport-relative geometry. Returns empty when graphics are
    /// disabled or there are no placements (the common case: ~3 FFI calls).
    pub(super) fn placements(&mut self) -> Vec<SnapshotPlacement> {
        let mut out = Vec::new();

        // Borrowed handle to the active screen's image storage; valid until the next
        // mutating terminal call (we only read here).
        let mut graphics: VtKittyGraphics = ptr::null_mut();

        if unsafe {
            ghostty_terminal_get(
                self.terminal,
                VtTerminalData::KITTY_GRAPHICS,
                (&mut graphics as *mut VtKittyGraphics).cast(),
            )
        } != VtResult::SUCCESS
            || graphics.is_null()
        {
            return out;
        }

        // Re-point the persistent iterator at the live placement set (no alloc).
        if unsafe {
            ghostty_kitty_graphics_get(
                graphics,
                VtKittyGraphicsData::PLACEMENT_ITERATOR,
                (&mut self.placement_iter as *mut VtKittyGraphicsPlacementIterator).cast(),
            )
        } != VtResult::SUCCESS
        {
            return out;
        }

        while unsafe { ghostty_kitty_graphics_placement_next(self.placement_iter) } {
            let iter = self.placement_iter;
            let image_id = placement_scalar::<u32>(iter, VtKittyGraphicsPlacementData::IMAGE_ID);
            let placement_id =
                placement_scalar::<u32>(iter, VtKittyGraphicsPlacementData::PLACEMENT_ID);

            let mut is_virtual = false;

            unsafe {
                ghostty_kitty_graphics_placement_get(
                    iter,
                    VtKittyGraphicsPlacementData::IS_VIRTUAL,
                    (&mut is_virtual as *mut bool).cast(),
                );
            }

            // Virtual placements have no engine viewport position (terminal reads the
            // placeholder cells instead), but carry the identity + grid size + z the
            // frame path needs to match placeholder runs to a placement.
            if is_virtual {
                out.push(SnapshotPlacement {
                    image_id,
                    placement_id,
                    is_virtual: true,
                    viewport_col: 0,
                    viewport_row: 0,
                    pixel_width: 0,
                    pixel_height: 0,
                    grid_cols: placement_scalar::<u32>(iter, VtKittyGraphicsPlacementData::COLUMNS),
                    grid_rows: placement_scalar::<u32>(iter, VtKittyGraphicsPlacementData::ROWS),
                    cell_x_offset: 0,
                    cell_y_offset: 0,
                    source_x: 0,
                    source_y: 0,
                    source_width: 0,
                    source_height: 0,
                    z: placement_scalar::<i32>(iter, VtKittyGraphicsPlacementData::Z),
                });

                continue;
            }

            // Geometry needs the image handle.
            let image = unsafe { ghostty_kitty_graphics_image(graphics, image_id) };

            if image.is_null() {
                continue;
            }

            let (mut vp_col, mut vp_row) = (0i32, 0i32);
            if unsafe {
                ghostty_kitty_graphics_placement_viewport_pos(
                    iter,
                    image,
                    self.terminal,
                    &mut vp_col,
                    &mut vp_row,
                )
            } != VtResult::SUCCESS
            {
                // Off-screen (NO_VALUE) — invisible this frame, nothing to paint.
                continue;
            }

            let (mut px_w, mut px_h) = (0u32, 0u32);

            unsafe {
                ghostty_kitty_graphics_placement_pixel_size(
                    iter,
                    image,
                    self.terminal,
                    &mut px_w,
                    &mut px_h,
                );
            }

            let (g_cols, g_rows, [sx, sy, sw, sh]) = placement_geometry(iter, image, self.terminal);

            out.push(SnapshotPlacement {
                image_id,
                placement_id,
                is_virtual: false,
                viewport_col: vp_col,
                viewport_row: vp_row,
                pixel_width: px_w,
                pixel_height: px_h,
                grid_cols: g_cols,
                grid_rows: g_rows,
                cell_x_offset: placement_scalar::<u32>(
                    iter,
                    VtKittyGraphicsPlacementData::X_OFFSET,
                ),
                cell_y_offset: placement_scalar::<u32>(
                    iter,
                    VtKittyGraphicsPlacementData::Y_OFFSET,
                ),
                source_x: sx,
                source_y: sy,
                source_width: sw,
                source_height: sh,
                z: placement_scalar::<i32>(iter, VtKittyGraphicsPlacementData::Z),
            });
        }

        out
    }

    /// Enumerate a finished block's Kitty placements with **block-relative**
    /// positions: `screen_col`/`screen_row`
    /// of the returned entries are in the block's own row space (the same
    /// rows `BlockRef::read_row_visit` reads). Requires the engine lock —
    /// the grid-size helpers read the live terminal's cell metrics — but the
    /// placements themselves come from the frozen storage pinned by `block`.
    /// Virtual placements and evicted pins are skipped; empty when graphics
    /// are disabled or the block has none (~2 FFI calls).
    pub fn block_placements(&mut self, block: &BlockRef) -> Vec<PlacementScreenPos> {
        let mut out = Vec::new();

        let Some(graphics) = block.kitty_graphics_raw() else {
            return out;
        };

        if unsafe {
            ghostty_kitty_graphics_get(
                graphics,
                VtKittyGraphicsData::PLACEMENT_ITERATOR,
                (&mut self.placement_iter as *mut VtKittyGraphicsPlacementIterator).cast(),
            )
        } != VtResult::SUCCESS
        {
            return out;
        }

        while unsafe { ghostty_kitty_graphics_placement_next(self.placement_iter) } {
            let iter = self.placement_iter;
            let (mut col, mut row) = (0u32, 0u32);

            if unsafe { ghostty_block_ref_placement_pos(block.raw, iter, &mut col, &mut row) }
                != VtResult::SUCCESS
            {
                // Virtual placement (unicode placeholder) — no pin to resolve.
                continue;
            }

            let image_id = placement_scalar::<u32>(iter, VtKittyGraphicsPlacementData::IMAGE_ID);
            let image = unsafe { ghostty_kitty_graphics_image(graphics, image_id) };

            if image.is_null() {
                continue;
            }
            let (g_cols, g_rows, [sx, sy, sw, sh]) = placement_geometry(iter, image, self.terminal);

            out.push(PlacementScreenPos {
                image_id,
                placement_id: placement_scalar::<u32>(
                    iter,
                    VtKittyGraphicsPlacementData::PLACEMENT_ID,
                ),
                screen_col: col,
                screen_row: row,
                grid_cols: g_cols,
                grid_rows: g_rows,
                source_x: sx,
                source_y: sy,
                source_width: sw,
                source_height: sh,
                z: placement_scalar::<i32>(iter, VtKittyGraphicsPlacementData::Z),
            });
        }

        out
    }

    /// Copy one frozen image's decoded pixels out of a finished block's
    /// Kitty storage. The caller keys the lazily uploaded result by
    /// `(block_id, image_id)`. `None` if the block holds no such image. Engine lock
    /// held by the caller; the pixels are copied out before returning.
    pub fn block_image_pixels(
        &self,
        block: &BlockRef,
        image_id: u32,
    ) -> Option<graphics::GraphicData> {
        let graphics = block.kitty_graphics_raw()?;
        let image = unsafe { ghostty_kitty_graphics_image(graphics, image_id) };

        if image.is_null() {
            return None;
        }

        let read_u32 = |data: VtKittyGraphicsImageData::Type| -> u32 {
            let mut v: u32 = 0;
            unsafe {
                ghostty_kitty_graphics_image_get(image, data, (&mut v as *mut u32).cast());
            }
            v
        };

        let width = read_u32(VtKittyGraphicsImageData::WIDTH);
        let height = read_u32(VtKittyGraphicsImageData::HEIGHT);

        let mut data_len: usize = 0;

        unsafe {
            ghostty_kitty_graphics_image_get(
                image,
                VtKittyGraphicsImageData::DATA_LEN,
                (&mut data_len as *mut usize).cast(),
            );
        }

        unsafe { kitty_image_graphic_data(image, image_id, width, height, data_len) }
    }

    /// Diff the live kitty images against the shipped set and return the pixel
    /// deltas. Called **only on the PTY reader thread** after `snapshot`
    /// (engine lock held by the caller); `placements` is that snapshot's placement
    /// list, so no second iterator walk. New or changed images (`(id,w,h,len)` key)
    /// have their pixels copied (`gray`/`gray_alpha` → rgba); vanished ids are
    /// reported for removal. Empty in steady state.
    pub fn take_image_deltas(
        &mut self,
        placements: &[SnapshotPlacement],
    ) -> (Vec<(u32, graphics::GraphicData)>, Vec<u32>) {
        let mut pending = Vec::new();
        let mut live: FxHashSet<u32> = FxHashSet::default();

        let mut graphics: VtKittyGraphics = ptr::null_mut();

        let have_graphics = unsafe {
            ghostty_terminal_get(
                self.terminal,
                VtTerminalData::KITTY_GRAPHICS,
                (&mut graphics as *mut VtKittyGraphics).cast(),
            )
        } == VtResult::SUCCESS
            && !graphics.is_null();

        if have_graphics {
            for p in placements {
                if !live.insert(p.image_id) {
                    continue;
                }

                let image = unsafe { ghostty_kitty_graphics_image(graphics, p.image_id) };

                if image.is_null() {
                    continue;
                }

                let read_u32 = |data: VtKittyGraphicsImageData::Type| -> u32 {
                    let mut v: u32 = 0;
                    unsafe {
                        ghostty_kitty_graphics_image_get(image, data, (&mut v as *mut u32).cast());
                    }
                    v
                };

                let width = read_u32(VtKittyGraphicsImageData::WIDTH);
                let height = read_u32(VtKittyGraphicsImageData::HEIGHT);

                let mut data_len: usize = 0;

                unsafe {
                    ghostty_kitty_graphics_image_get(
                        image,
                        VtKittyGraphicsImageData::DATA_LEN,
                        (&mut data_len as *mut usize).cast(),
                    );
                }

                let key = (width, height, data_len);

                if self.shipped_images.get(&p.image_id) == Some(&key) {
                    continue; // unchanged — already shipped
                }

                let Some(data) = (unsafe {
                    kitty_image_graphic_data(image, p.image_id, width, height, data_len)
                }) else {
                    continue;
                };

                pending.push((p.image_id, data));

                self.shipped_images.insert(p.image_id, key);
            }
        }

        // "Live" means the engine still holds the image, not that it has a visible
        // placement". An image scrolled off-screen is still live, so it must not
        // be evicted/re-shipped on scroll-back ("a scroll must not emit graphics
        // churn"). Removal fires only on kitty delete-with-free or storage
        // eviction. `live` (above) is only a per-batch placement dedup.
        let removed: Vec<u32> = self
            .shipped_images
            .keys()
            .copied()
            .filter(|id| {
                !have_graphics || unsafe { ghostty_kitty_graphics_image(graphics, *id) }.is_null()
            })
            .collect();

        for id in &removed {
            self.shipped_images.remove(id);
        }

        (pending, removed)
    }
}

/// Read one datum of the placement the iterator is positioned on. `T` must be
/// the 32-bit integer type the FFI writes for `data` (u32 or i32).
fn placement_scalar<T: Default>(
    iter: VtKittyGraphicsPlacementIterator,
    data: VtKittyGraphicsPlacementData::Type,
) -> T {
    let mut v = T::default();

    unsafe {
        ghostty_kitty_graphics_placement_get(iter, data, (&mut v as *mut T).cast());
    }
    v
}

/// Grid size + resolved source rectangle of the current placement — the shared
/// tail of every placement walk. Returns `(grid_cols, grid_rows, [sx, sy, sw, sh])`.
fn placement_geometry(
    iter: VtKittyGraphicsPlacementIterator,
    image: VtKittyGraphicsImage,
    terminal: VtTerminal,
) -> (u32, u32, [u32; 4]) {
    let (mut g_cols, mut g_rows) = (0u32, 0u32);
    let (mut sx, mut sy, mut sw, mut sh) = (0u32, 0u32, 0u32, 0u32);

    unsafe {
        ghostty_kitty_graphics_placement_grid_size(iter, image, terminal, &mut g_cols, &mut g_rows);

        ghostty_kitty_graphics_placement_source_rect(
            iter, image, &mut sx, &mut sy, &mut sw, &mut sh,
        );
    }

    (g_cols, g_rows, [sx, sy, sw, sh])
}

/// Copy a decoded kitty image's pixels into a [`crate::graphics::GraphicData`],
/// converting gray forms to RGBA (the engine already decoded PNG/zlib, so
/// only raw pixel formats reach here). Shared by the live delta shipper and
/// the frozen-block lazy read.
///
/// # Safety
/// `image` must be a live image handle from the storage the caller currently
/// pins (engine lock or an acquired block ref).
unsafe fn kitty_image_graphic_data(
    image: VtKittyGraphicsImage,
    image_id: u32,
    width: u32,
    height: u32,
    data_len: usize,
) -> Option<graphics::GraphicData> {
    use crate::graphics::{ColorType, GraphicData, GraphicId};

    let mut format: VtKittyImageFormat::Type = VtKittyImageFormat::RGBA;

    unsafe {
        ghostty_kitty_graphics_image_get(
            image,
            VtKittyGraphicsImageData::FORMAT,
            (&mut format as *mut VtKittyImageFormat::Type).cast(),
        );
    }

    let mut data_ptr: *const u8 = ptr::null();

    unsafe {
        ghostty_kitty_graphics_image_get(
            image,
            VtKittyGraphicsImageData::DATA_PTR,
            (&mut data_ptr as *mut *const u8).cast(),
        );
    }

    if data_ptr.is_null() || data_len == 0 {
        return None;
    }

    let raw = unsafe { slice::from_raw_parts(data_ptr, data_len) };

    let (pixels, color_type) = match format {
        VtKittyImageFormat::RGB => (raw.to_vec(), ColorType::Rgb),
        VtKittyImageFormat::RGBA => (raw.to_vec(), ColorType::Rgba),
        VtKittyImageFormat::GRAY => {
            let mut px = Vec::with_capacity(raw.len() * 4);

            for &g in raw {
                px.extend_from_slice(&[g, g, g, 255]);
            }

            (px, ColorType::Rgba)
        }
        VtKittyImageFormat::GRAY_ALPHA => {
            let mut px = Vec::with_capacity(raw.len() * 2);

            for ga in raw.chunks_exact(2) {
                px.extend_from_slice(&[ga[0], ga[0], ga[0], ga[1]]);
            }

            (px, ColorType::Rgba)
        }
        _ => return None, // PNG/unknown shouldn't reach here post-decode
    };

    let is_opaque = color_type == ColorType::Rgb;

    Some(GraphicData {
        id: GraphicId(image_id as u64),
        width: width as usize,
        height: height as usize,
        color_type,
        pixels,
        is_opaque,
        resize: None,
        display_width: None,
        display_height: None,
        transmit_time: time::Instant::now(),
    })
}
