//! Terminal graphics-protocol data types (kitty / sixel / iTerm2 inline
//! images). Relocated from the old `nmt_renderer` crate: these are pure data
//! the terminal engine produces from the PTY, carrying no renderer/GPU state.

use std::time;

/// Unique identifier for every graphic added to a grid.
/// An id of 0 represents a temporary, non-referenceable image
/// (matching kitty's behavior).
#[derive(Eq, PartialEq, Clone, Debug, Copy, Hash, PartialOrd, Ord)]
pub struct GraphicId(pub u64);

impl GraphicId {
    /// Create a new GraphicId from a u64 value.
    #[inline]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Get the inner u64 value.
    #[inline]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Specifies the format of the pixel data.
#[derive(Eq, PartialEq, Clone, Debug, Copy)]
pub enum ColorType {
    /// 3 bytes per pixel (red, green, blue).
    Rgb,

    /// 4 bytes per pixel (red, green, blue, alpha).
    Rgba,
}

/// Defines a single graphic read from the PTY.
#[derive(Eq, PartialEq, Clone, Debug)]
pub struct GraphicData {
    /// Graphics identifier.
    pub id: GraphicId,

    /// Width, in pixels, of the graphic.
    pub width: usize,

    /// Height, in pixels, of the graphic.
    pub height: usize,

    /// Color type of the pixels.
    pub color_type: ColorType,

    /// Pixels data.
    pub pixels: Vec<u8>,

    /// Indicate if there are no transparent pixels.
    pub is_opaque: bool,

    /// Generation counter for cache invalidation.
    /// Incremented when image data changes (re-transmission with same ID).
    pub transmit_time: time::Instant,
}

/// One batch of image changes handed to the renderer's image store. The engine
/// reports pixels and removals together so a re-transmission under an id the
/// grid still references cannot be applied in the wrong order.
#[derive(Debug, Clone)]
pub struct UpdateQueues {
    /// Atlas graphics (sixel/iTerm2) read from the PTY.
    pub pending: Vec<GraphicData>,

    /// Image textures (kitty) keyed by image_id.
    pub pending_images: Vec<(u32, GraphicData)>,

    /// Graphics removed from the grid.
    pub remove_queue: Vec<GraphicId>,
}
