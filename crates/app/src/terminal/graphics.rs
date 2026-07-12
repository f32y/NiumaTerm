//! Kitty graphics image cache for the GPUI terminal.
//!
//! Decoded Kitty pixels arrive on the PTY thread via `TerminalEvent::UpdateGraphics`.
//! This module turns them into CPU-only GPUI [`RenderImage`] generations off the UI
//! thread, keeps the live set in a per-session [`GenerationStore`] keyed by numeric
//! image ID, and releases GPUI atlas tiles through a shared [`ReleaseQueue`] when a
//! generation's final reference drops. Active frames and frozen command-block history
//! share generations by `Arc`, so a generation lives exactly as long as something can
//! still paint it.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui::RenderImage;
use image_rs::{Frame, RgbaImage};
use nmt_terminal::graphics::{ColorType, GraphicData};
use parking_lot::Mutex;

// Generations are built and dropped on the PTY thread, so both the GPUI image
// and the wrapper must cross threads. The compile-time assertion enforces the invariant.
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<RenderImage>();
    assert_send_sync::<ImageGeneration>();
};

/// UI-thread atlas-release queue for one window/pane. An [`ImageGeneration`] that
/// reached the GPUI atlas pushes its `Arc<RenderImage>` here exactly once when its
/// final reference drops; the UI drains it through `Window::drop_image` on content
/// wakes. Cheap to clone (an `Arc`).
pub type ReleaseQueue = Arc<Mutex<Vec<Arc<RenderImage>>>>;

/// One decoded Kitty image generation: an immutable CPU-only [`RenderImage`], its
/// byte size (for the frozen-history budget), an atomic "reached the atlas" flag,
/// and the shared release queue. Shared by `Arc` between the live [`GenerationStore`],
/// active frames, and frozen blocks. When the last `Arc` drops, RAII enqueues one
/// atlas release iff the image was ever uploaded.
#[derive(Debug)]
pub struct ImageGeneration {
    image: Arc<RenderImage>,
    uploaded: AtomicBool,
    release: ReleaseQueue,
}

impl ImageGeneration {
    fn new(image: Arc<RenderImage>, release: ReleaseQueue) -> Arc<Self> {
        Arc::new(Self {
            image,
            uploaded: AtomicBool::new(false),
            release,
        })
    }

    /// The CPU-only GPUI image. Cloning the `Arc` is what keeps a frame or frozen
    /// block referencing this generation without copying pixels.
    pub fn image(&self) -> &Arc<RenderImage> {
        &self.image
    }

    /// Record that a `Window::paint_image` uploaded this generation into the atlas.
    /// Idempotent; enables the one-shot release on final drop.
    pub fn mark_uploaded(&self) {
        self.uploaded.store(true, Ordering::Relaxed);
    }
}

impl Drop for ImageGeneration {
    fn drop(&mut self) {
        // Only images that reached the atlas need releasing; an unpainted superseded
        // generation just frees its CPU bytes here without touching the queue.
        if self.uploaded.load(Ordering::Relaxed) {
            self.release.lock().push(self.image.clone());
        }
    }
}

/// Expanded full-image bounds for source-rectangle painting. GPUI's
/// `paint_image` has no source argument, so a source crop is rendered by painting the
/// **full** image into an enlarged rectangle and clipping it to the destination via a
/// content mask. For a normalized source `[u0, v0, u1, v1]` and destination
/// `[dx, dy, dw, dh]` (pixels), returns the `[x, y, w, h]` full-image rectangle to
/// pass to `paint_image`, or `None` for a degenerate or non-finite source/destination
/// (which paint skips). Pure — no GPUI types.
pub fn expanded_full_bounds(dest: [f32; 4], source: [f32; 4]) -> Option<[f32; 4]> {
    let [dx, dy, dw, dh] = dest;
    let [u0, v0, u1, v1] = source;
    if !dest.iter().chain(source.iter()).all(|f| f.is_finite()) {
        return None;
    }
    if dw <= 0.0 || dh <= 0.0 {
        return None;
    }
    let (du, dv) = (u1 - u0, v1 - v0);
    if du <= 0.0 || dv <= 0.0 {
        return None;
    }
    let full_w = dw / du;
    let full_h = dh / dv;
    if !full_w.is_finite() || !full_h.is_finite() {
        return None;
    }
    // Offset the full image so the source origin (u0, v0) lands at the destination's
    // top-left; the caller clips the overflow to `dest`.
    Some([dx - u0 * full_w, dy - v0 * full_h, full_w, full_h])
}

/// Convert decoded Kitty pixels into the BGRA byte layout GPUI's atlas expects,
/// consuming `pixels` so a valid RGBA buffer is reused in place (only R/B swapped).
/// RGB is expanded to BGRA with opaque alpha. Returns `None` for zero dimensions, a
/// pixel count that overflows `usize`, or a byte length that does not match
/// `width * height * channels`. Pure — no GPUI or window access.
pub fn graphic_to_bgra(
    width: usize,
    height: usize,
    color_type: ColorType,
    mut pixels: Vec<u8>,
) -> Option<Vec<u8>> {
    if width == 0 || height == 0 {
        return None;
    }
    let pixel_count = width.checked_mul(height)?;
    match color_type {
        ColorType::Rgba => {
            if pixels.len() != pixel_count.checked_mul(4)? {
                return None;
            }
            for px in pixels.chunks_exact_mut(4) {
                px.swap(0, 2); // RGBA -> BGRA
            }
            Some(pixels)
        }
        ColorType::Rgb => {
            if pixels.len() != pixel_count.checked_mul(3)? {
                return None;
            }
            let mut out = Vec::with_capacity(pixel_count * 4);
            for px in pixels.chunks_exact(3) {
                out.extend_from_slice(&[px[2], px[1], px[0], 255]); // BGRA
            }
            Some(out)
        }
    }
}

/// Build a CPU-only image generation from decoded pixels, or `None` if the pixels are
/// invalid (dimensions/length). Constructs the `RenderImage` but performs no GPU upload.
pub fn graphic_to_generation(
    data: GraphicData,
    release: &ReleaseQueue,
) -> Option<Arc<ImageGeneration>> {
    let (width, height) = (data.width, data.height);
    let bgra = graphic_to_bgra(width, height, data.color_type, data.pixels)?;
    // `from_raw` re-validates length against the dimensions and never allocates.
    let buffer = RgbaImage::from_raw(width as u32, height as u32, bgra)?;
    let image = Arc::new(RenderImage::new(vec![Frame::new(buffer)]));
    Some(ImageGeneration::new(image, release.clone()))
}

/// Per-session cache of lazily-read frozen Kitty generations keyed
/// `(block_id, image_id)`. Pixels are copied out of the engine
/// block once on first paint, then shared by `Arc`; entries die with their
/// block (see [`prune_frozen_images`]), so the memory mirrors the engine's
/// own per-block image ownership, which the engine block budget bounds.
/// Lives beside the (gpui-free) `BlockStore` rather than inside it because
/// the values are gpui images.
pub(crate) type FrozenImageCache = Arc<Mutex<HashMap<(u64, u32), Arc<ImageGeneration>>>>;

/// Mirror block lifecycle events into the frozen-image cache: entries of
/// evicted blocks die on `EngineBlocksSync`, and a user clear (`;K`) drops
/// everything. Called on the PTY-event path, right where the same batch
/// feeds the block store.
pub(crate) fn prune_frozen_images(
    cache: &FrozenImageCache,
    events: &[nmt_terminal::event::BlockEvent],
) {
    use nmt_terminal::event::BlockEvent;
    for event in events {
        match event {
            BlockEvent::EngineBlocksSync(live) => {
                let alive: std::collections::HashSet<u64> =
                    live.iter().map(|(handle, _)| handle.id).collect();
                cache
                    .lock()
                    .retain(|(block_id, _), _| alive.contains(block_id));
            }
            BlockEvent::HistoryCleared => cache.lock().clear(),
            BlockEvent::EngineBlock { .. } => {}
        }
    }
}

/// Per-session store of live Kitty image generations keyed by numeric image ID
/// A newer transmission replaces the active generation under the same ID;
/// frozen blocks keep any prior generation alive by holding their own `Arc`. Owns the
/// session/window [`ReleaseQueue`] and hands clones to the block store.
#[derive(Default)]
pub struct GenerationStore {
    generations: HashMap<u32, Arc<ImageGeneration>>,
    release: ReleaseQueue,
}

impl GenerationStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// The shared atlas-release queue for this session; clone into the block store so
    /// frozen generations release through the same window atlas.
    pub fn release_queue(&self) -> ReleaseQueue {
        self.release.clone()
    }

    /// Install (or replace) the live generation for `id`. Returns the new generation,
    /// or `None` on invalid pixels — in which case the previously cached generation is
    /// left untouched. Replacing drops the store's `Arc` to the old generation; if no
    /// frame or frozen block still holds it and it was uploaded, its `Drop` enqueues an
    /// atlas release.
    pub fn install(&mut self, id: u32, data: GraphicData) -> Option<Arc<ImageGeneration>> {
        let generation = graphic_to_generation(data, &self.release)?;
        self.generations.insert(id, generation.clone());
        Some(generation)
    }

    /// Remove the live mapping for `id` (image deleted or evicted). Immutable
    /// generations retained by frozen blocks stay valid.
    pub fn remove(&mut self, id: u32) {
        self.generations.remove(&id);
    }

    /// Look up the live generation for `id` (an `Arc` clone; no lock held afterwards).
    #[cfg(test)]
    pub fn get(&self, id: u32) -> Option<Arc<ImageGeneration>> {
        self.generations.get(&id).cloned()
    }

    /// Clone the live id→generation map so frame extraction can resolve placements
    /// without holding the store lock while it walks the render buffer. This avoids
    /// nesting the generation-store and render/block locks. Cheap: `Arc` clones.
    pub fn live_generations(&self) -> HashMap<u32, Arc<ImageGeneration>> {
        self.generations.clone()
    }

    /// Take the accumulated atlas releases so the UI can drain them through
    /// `Window::drop_image`. Includes releases from frozen blocks sharing this queue.
    pub fn drain_released(&self) -> Vec<Arc<RenderImage>> {
        std::mem::take(&mut *self.release.lock())
    }

    pub fn len(&self) -> usize {
        self.generations.len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.generations.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use nmt_terminal::graphics::GraphicId;

    use super::*;

    fn data(id: u32, w: usize, h: usize, color_type: ColorType, pixels: Vec<u8>) -> GraphicData {
        GraphicData {
            id: GraphicId(id as u64),
            width: w,
            height: h,
            color_type,
            pixels,
            is_opaque: true,
            resize: None,
            display_width: None,
            display_height: None,
            transmit_time: std::time::Instant::now(),
        }
    }

    fn approx4(a: [f32; 4], b: [f32; 4]) {
        for (x, y) in a.iter().zip(b.iter()) {
            assert!((x - y).abs() < 1e-3, "expected {b:?}, got {a:?}");
        }
    }

    /// The frozen-image cache mirrors block lifecycle: sync eviction drops an
    /// evicted block's entries, `;K` drops everything.
    #[test]
    fn frozen_image_cache_prunes_with_block_lifecycle() {
        use nmt_terminal::event::BlockEvent;
        use nmt_terminal::ghostty::BlockHandle;

        let q: ReleaseQueue = Default::default();
        let g = graphic_to_generation(data(1, 1, 1, ColorType::Rgba, vec![0; 4]), &q).unwrap();
        let cache: FrozenImageCache = Default::default();
        cache.lock().insert((10, 1), g.clone());
        cache.lock().insert((11, 1), g.clone());

        // Block 10 evicted engine-side; block 11 survives.
        prune_frozen_images(
            &cache,
            &[BlockEvent::EngineBlocksSync(vec![(
                BlockHandle {
                    id: 11,
                    generation: 2,
                },
                9,
            )])],
        );
        assert!(!cache.lock().contains_key(&(10, 1)), "evicted block pruned");
        assert!(cache.lock().contains_key(&(11, 1)));

        prune_frozen_images(&cache, &[BlockEvent::HistoryCleared]);
        assert!(cache.lock().is_empty(), "user clear drops everything");
    }

    #[test]
    fn expanded_bounds_full_source_is_identity() {
        // Full [0,0,1,1] source → paint the whole image at the destination as-is.
        approx4(
            expanded_full_bounds([10.0, 20.0, 100.0, 50.0], [0.0, 0.0, 1.0, 1.0]).unwrap(),
            [10.0, 20.0, 100.0, 50.0],
        );
    }

    #[test]
    fn expanded_bounds_crop_enlarges_and_offsets() {
        // Top-left quarter [0,0,0.5,0.5] → full image is 2x the dest, top-left aligned.
        approx4(
            expanded_full_bounds([0.0, 0.0, 100.0, 50.0], [0.0, 0.0, 0.5, 0.5]).unwrap(),
            [0.0, 0.0, 200.0, 100.0],
        );
        // Bottom-right quarter [0.5,0.5,1,1] → full 2x, offset up/left by one dest.
        approx4(
            expanded_full_bounds([0.0, 0.0, 100.0, 50.0], [0.5, 0.5, 1.0, 1.0]).unwrap(),
            [-100.0, -50.0, 200.0, 100.0],
        );
    }

    #[test]
    fn expanded_bounds_rejects_degenerate_and_non_finite() {
        assert!(expanded_full_bounds([0.0, 0.0, 0.0, 50.0], [0.0, 0.0, 1.0, 1.0]).is_none());
        assert!(expanded_full_bounds([0.0, 0.0, 100.0, 50.0], [0.5, 0.0, 0.5, 1.0]).is_none());
        assert!(expanded_full_bounds([0.0, 0.0, 100.0, 50.0], [0.0, 0.0, 1.0, f32::NAN]).is_none());
        assert!(
            expanded_full_bounds([f32::INFINITY, 0.0, 100.0, 50.0], [0.0, 0.0, 1.0, 1.0]).is_none()
        );
    }

    #[test]
    fn rgba_reuses_buffer_and_swaps_channels() {
        // One RGBA pixel R=1 G=2 B=3 A=4 -> BGRA 3 2 1 4.
        let out = graphic_to_bgra(1, 1, ColorType::Rgba, vec![1, 2, 3, 4]).unwrap();
        assert_eq!(out, vec![3, 2, 1, 4]);
    }

    #[test]
    fn rgb_expands_with_opaque_alpha() {
        // One RGB pixel R=1 G=2 B=3 -> BGRA 3 2 1 255.
        let out = graphic_to_bgra(1, 1, ColorType::Rgb, vec![1, 2, 3]).unwrap();
        assert_eq!(out, vec![3, 2, 1, 255]);
    }

    #[test]
    fn rejects_invalid_dimensions_and_lengths() {
        assert!(graphic_to_bgra(0, 1, ColorType::Rgba, vec![]).is_none());
        assert!(graphic_to_bgra(1, 0, ColorType::Rgb, vec![]).is_none());
        // Mismatched byte length for the stated dimensions.
        assert!(graphic_to_bgra(2, 2, ColorType::Rgba, vec![0; 4]).is_none());
        assert!(graphic_to_bgra(2, 2, ColorType::Rgb, vec![0; 8]).is_none());
        // Pixel-count overflow.
        assert!(graphic_to_bgra(usize::MAX, 2, ColorType::Rgb, vec![]).is_none());
    }

    #[test]
    fn install_replaces_under_same_id() {
        let mut store = GenerationStore::new();
        let a = store
            .install(7, data(7, 1, 1, ColorType::Rgba, vec![255, 0, 0, 255]))
            .unwrap();
        let b = store
            .install(7, data(7, 1, 1, ColorType::Rgba, vec![0, 0, 255, 255]))
            .unwrap();
        // The store now maps to the newer generation; the two are distinct.
        assert!(!Arc::ptr_eq(&a, &b));
        assert!(Arc::ptr_eq(&store.get(7).unwrap(), &b));
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn invalid_pixels_leave_previous_cached() {
        let mut store = GenerationStore::new();
        let good = store
            .install(1, data(1, 1, 1, ColorType::Rgba, vec![1, 2, 3, 4]))
            .unwrap();
        // Wrong length: install fails, cache untouched.
        assert!(
            store
                .install(1, data(1, 1, 1, ColorType::Rgba, vec![0]))
                .is_none()
        );
        assert!(Arc::ptr_eq(&store.get(1).unwrap(), &good));
    }

    #[test]
    fn remove_and_id_reuse() {
        let mut store = GenerationStore::new();
        store.install(3, data(3, 1, 1, ColorType::Rgb, vec![1, 1, 1]));
        store.remove(3);
        assert!(store.get(3).is_none());
        // A later transmission may reuse the same numeric id independently.
        let reused = store
            .install(3, data(3, 1, 1, ColorType::Rgb, vec![2, 2, 2]))
            .unwrap();
        assert!(Arc::ptr_eq(&store.get(3).unwrap(), &reused));
    }

    #[test]
    fn unpainted_generation_releases_nothing() {
        let store = GenerationStore::new();
        let queue = store.release_queue();
        {
            let _g =
                graphic_to_generation(data(1, 1, 1, ColorType::Rgba, vec![0; 4]), &queue).unwrap();
            // Never marked uploaded.
        }
        assert!(
            queue.lock().is_empty(),
            "unpainted drop must not enqueue release"
        );
    }

    #[test]
    fn uploaded_generation_releases_exactly_once() {
        let store = GenerationStore::new();
        let queue = store.release_queue();
        {
            let g =
                graphic_to_generation(data(1, 1, 1, ColorType::Rgba, vec![0; 4]), &queue).unwrap();
            g.mark_uploaded();
            // Extra clones must not each enqueue a release — only the final drop does.
            let _c1 = g.clone();
            let _c2 = g.clone();
        }
        assert_eq!(
            store.drain_released().len(),
            1,
            "an uploaded generation releases exactly once when its last Arc drops"
        );
    }

    #[test]
    fn replacement_releases_old_when_uploaded_and_unreferenced() {
        let mut store = GenerationStore::new();
        let old = store
            .install(9, data(9, 1, 1, ColorType::Rgba, vec![0; 4]))
            .unwrap();
        old.mark_uploaded();
        drop(old); // store still holds a ref
        assert!(
            store.drain_released().is_empty(),
            "still referenced by store"
        );
        // Replacing drops the store's last ref to the old generation.
        store.install(9, data(9, 1, 1, ColorType::Rgba, vec![255; 4]));
        assert_eq!(
            store.drain_released().len(),
            1,
            "old uploaded generation released"
        );
    }

    #[test]
    fn independent_sessions_do_not_share() {
        let mut s1 = GenerationStore::new();
        let mut s2 = GenerationStore::new();
        s1.install(1, data(1, 1, 1, ColorType::Rgba, vec![0; 4]));
        assert_eq!(s1.len(), 1);
        assert!(s2.is_empty(), "second session's store is independent");
        // A release in s1 does not surface in s2's queue.
        let g = s2
            .install(1, data(1, 1, 1, ColorType::Rgba, vec![0; 4]))
            .unwrap();
        g.mark_uploaded();
        s2.remove(1);
        drop(g);
        assert!(
            s1.drain_released().is_empty(),
            "s1 queue unaffected by s2 release"
        );
        assert_eq!(s2.drain_released().len(), 1);
    }
}
