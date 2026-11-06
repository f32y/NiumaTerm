use std::collections::VecDeque;

use gpui::{Bounds, ContentMask, ScaledPixels, Scene, TransformationMatrix, point, size};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum FrameDamage {
    Full,
    Region(Bounds<ScaledPixels>),
    None,
}

impl FrameDamage {
    fn include(&mut self, other: Self) {
        *self = match (*self, other) {
            (Self::Full, _) | (_, Self::Full) => Self::Full,
            (Self::None, damage) | (damage, Self::None) => damage,
            (Self::Region(a), Self::Region(b)) => Self::Region(a.union(&b)),
        };
    }
}

/// Compare rendered primitives and include changes since the current swap-chain
/// buffer was last used. Every buffer is fully initialized before partial draws.
pub(super) struct SceneDamage {
    previous: Scene,
    valid: bool,
    history: VecDeque<FrameDamage>,
    buffer_count: usize,
    initialized: usize,
}

impl SceneDamage {
    pub(super) fn new(buffer_count: usize) -> Self {
        assert!(buffer_count > 0);

        Self {
            previous: Scene::default(),
            valid: false,
            history: VecDeque::with_capacity(buffer_count),
            buffer_count,
            initialized: 0,
        }
    }

    pub(super) fn invalidate(&mut self) {
        self.valid = false;
        self.initialized = 0;

        self.history.clear();
    }

    pub(super) fn update(
        &mut self,
        scene: &Scene,
        extra: Option<&[Bounds<ScaledPixels>]>,
        viewport: Bounds<ScaledPixels>,
    ) -> FrameDamage {
        // Blur reads neighboring pixels; paths use a separate MSAA target. Keep
        // full draws until those passes support partial updates. Transformed
        // sprites can cover pixels outside their untransformed bounds.
        if !scene.paths.is_empty()
            || !scene.backdrop_blurs.is_empty()
            || !scene.surfaces.is_empty()
            || scene
                .monochrome_sprites
                .iter()
                .any(|sprite| sprite.transformation != TransformationMatrix::unit())
            || scene
                .subpixel_sprites
                .iter()
                .any(|sprite| sprite.transformation != TransformationMatrix::unit())
        {
            self.invalidate();

            return FrameDamage::Full;
        }

        let mut changed = if self.valid {
            FrameDamage::None
        } else {
            FrameDamage::Full
        };

        macro_rules! compare {
            ($($field:ident),+ $(,)?) => { $(
                if self.valid {
                    compare_primitives(
                        &self.previous.$field,
                        &scene.$field,
                        |primitive| (primitive.bounds, primitive.content_mask),
                        viewport,
                        &mut changed,
                    );
                }
                self.previous.$field.clone_from(&scene.$field);
            )+ };
        }

        compare!(
            shadows,
            quads,
            underlines,
            monochrome_sprites,
            subpixel_sprites,
            polychrome_sprites
        );

        if let Some(extra) = extra {
            for bounds in extra {
                include_bounds(&mut changed, *bounds, viewport);
            }
        }

        self.valid = true;

        let mut repaint = changed;

        if self.initialized < self.buffer_count {
            repaint = FrameDamage::Full;
            self.initialized += 1;
        } else {
            for previous in &self.history {
                repaint.include(*previous);
            }
        }

        // Store scene changes, not the larger repaint area, or a full repaint
        // would force subsequent buffers to repaint forever.
        self.history.push_back(changed);

        if self.history.len() >= self.buffer_count {
            self.history.pop_front();
        }

        // A broad update gains little from filtering and clipping, so keep the
        // direct full-scene path for scrolling and large layout changes.
        if let FrameDamage::Region(region) = repaint
            && region.size.width.0 * region.size.height.0
                >= viewport.size.width.0 * viewport.size.height.0 / 2.
        {
            return FrameDamage::Full;
        }

        repaint
    }
}

fn compare_primitives<T: PartialEq>(
    previous: &[T],
    current: &[T],
    bounds: impl Fn(&T) -> (Bounds<ScaledPixels>, ContentMask<ScaledPixels>),
    viewport: Bounds<ScaledPixels>,
    changed: &mut FrameDamage,
) {
    for index in 0..previous.len().max(current.len()) {
        let old = previous.get(index);
        let new = current.get(index);

        if old == new {
            continue;
        }

        for primitive in old.into_iter().chain(new) {
            let (bounds, mask) = bounds(primitive);

            include_bounds(changed, bounds.intersect(&mask.bounds), viewport);
        }
    }
}

fn include_bounds(
    damage: &mut FrameDamage,
    bounds: Bounds<ScaledPixels>,
    viewport: Bounds<ScaledPixels>,
) {
    let coordinates = [
        bounds.left().0,
        bounds.top().0,
        bounds.right().0,
        bounds.bottom().0,
    ];

    if coordinates.iter().any(|coordinate| !coordinate.is_finite()) {
        *damage = FrameDamage::Full;
    } else if !bounds.is_empty() {
        // Include antialiasing at fractional edges and round outward to physical
        // pixels before using the same area for clearing, clipping and present.
        let left = bounds.left().0.floor() - 1.;
        let top = bounds.top().0.floor() - 1.;
        let right = bounds.right().0.ceil() + 1.;
        let bottom = bounds.bottom().0.ceil() + 1.;

        let bounds = Bounds {
            origin: point(ScaledPixels(left), ScaledPixels(top)),
            size: size(ScaledPixels(right - left), ScaledPixels(bottom - top)),
        }
        .intersect(&viewport);

        if !bounds.is_empty() {
            damage.include(FrameDamage::Region(bounds));
        }
    }
}

pub(super) fn select_primitives(scene: &Scene, region: Bounds<ScaledPixels>, output: &mut Scene) {
    output.clear();

    macro_rules! select {
        ($($field:ident),+ $(,)?) => { $(
            output.$field.extend(scene.$field.iter().filter(|primitive| {
                !primitive.bounds.intersect(&primitive.content_mask.bounds).intersect(&region).is_empty()
            }).copied());
        )+ };
    }

    select!(
        shadows,
        quads,
        underlines,
        monochrome_sprites,
        subpixel_sprites,
        polychrome_sprites
    );
}

#[cfg(test)]
mod tests {
    use gpui::{BackdropBlur, Bounds, ContentMask, Quad, ScaledPixels, Scene, point, red, size};

    use crate::directx_renderer::damage::{FrameDamage, SceneDamage, select_primitives};

    fn bounds(x: f32, y: f32, width: f32, height: f32) -> Bounds<ScaledPixels> {
        Bounds {
            origin: point(ScaledPixels(x), ScaledPixels(y)),
            size: size(ScaledPixels(width), ScaledPixels(height)),
        }
    }

    fn scene_at(x: f32) -> Scene {
        let mut scene = Scene::default();

        scene.insert_primitive(Quad {
            bounds: bounds(x, 10., 5., 5.),
            content_mask: ContentMask {
                bounds: bounds(0., 0., 100., 100.),
            },
            background: red().into(),
            ..Quad::default()
        });

        scene.finish();

        scene
    }

    #[test]
    fn rotating_buffers_include_changes_since_their_last_use() {
        let viewport = bounds(0., 0., 100., 100.);

        let mut damage = SceneDamage::new(3);

        for _ in 0..3 {
            assert_eq!(
                damage.update(&scene_at(10.), None, viewport),
                FrameDamage::Full
            );
        }

        assert_eq!(
            damage.update(&scene_at(10.), None, viewport),
            FrameDamage::None
        );

        let moved = FrameDamage::Region(bounds(9., 9., 27., 7.));

        assert_eq!(damage.update(&scene_at(30.), None, viewport), moved);
        assert_eq!(damage.update(&scene_at(30.), None, viewport), moved);
        assert_eq!(damage.update(&scene_at(30.), None, viewport), moved);
        assert_eq!(
            damage.update(&scene_at(30.), None, viewport),
            FrameDamage::None
        );

        damage.invalidate();

        assert_eq!(
            damage.update(&scene_at(30.), None, viewport),
            FrameDamage::Full
        );
    }

    #[test]
    fn backdrop_blur_invalidates_partial_history_until_all_buffers_are_initialized() {
        let viewport = bounds(0., 0., 100., 100.);

        let mut damage = SceneDamage::new(3);

        for _ in 0..3 {
            damage.update(&scene_at(10.), None, viewport);
        }

        let mut blurred = scene_at(10.);

        blurred.backdrop_blurs.push(BackdropBlur::default());

        assert_eq!(damage.update(&blurred, None, viewport), FrameDamage::Full);

        for _ in 0..3 {
            assert_eq!(
                damage.update(&scene_at(10.), None, viewport),
                FrameDamage::Full
            );
        }

        assert_eq!(
            damage.update(&scene_at(10.), None, viewport),
            FrameDamage::None
        );
    }

    #[test]
    fn removed_primitives_and_explicit_damage_remain_in_repaint_area() {
        let viewport = bounds(0., 0., 100., 100.);

        let mut damage = SceneDamage::new(1);

        damage.update(&scene_at(10.), None, viewport);

        assert_eq!(
            damage.update(
                &Scene::default(),
                Some(&[bounds(50., 50., 2., 2.)]),
                viewport
            ),
            FrameDamage::Region(bounds(9., 9., 44., 44.)),
        );
    }

    #[test]
    fn selection_preserves_overlapping_background_and_draw_order() {
        let mut scene = scene_at(10.);

        scene.quads.insert(
            0,
            Quad {
                bounds: bounds(0., 0., 100., 100.),
                content_mask: ContentMask {
                    bounds: bounds(0., 0., 100., 100.),
                },
                ..Quad::default()
            },
        );

        scene.quads.extend(scene_at(80.).quads);

        let mut output = Scene::default();

        select_primitives(&scene, bounds(9., 9., 7., 7.), &mut output);

        assert_eq!(output.quads, scene.quads[..2]);
    }
}
