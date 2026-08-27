use std::time;

use nmt_terminal::graphics::GraphicId;

use crate::terminal::graphics::*;

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
        transmit_time: time::Instant::now(),
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
        let _g = graphic_to_generation(data(1, 1, 1, ColorType::Rgba, vec![0; 4]), &queue).unwrap();
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
        let g = graphic_to_generation(data(1, 1, 1, ColorType::Rgba, vec![0; 4]), &queue).unwrap();
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
