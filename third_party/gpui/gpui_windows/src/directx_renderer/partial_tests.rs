use std::borrow::Cow;

use anyhow::{Context, Result};
use gpui::{
    AtlasKey, AtlasTile, Bounds, ContentMask, DevicePixels, FontId, GlyphId, ImageId,
    MonochromeSprite, PaddedBool32, PlatformAtlas, PolychromeSprite, Quad, RenderGlyphParams,
    RenderImageParams, ScaledPixels, Scene, Shadow, SubpixelSprite, TransformationMatrix,
    Underline, WindowBackgroundAppearance, black, point, px, red, size, white,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_QUERY_DATA_PIPELINE_STATISTICS, D3D11_QUERY_DESC, D3D11_QUERY_PIPELINE_STATISTICS,
    ID3D11DeviceContext, ID3D11Query,
};

use crate::directx_devices::DirectXDevices;
use crate::directx_renderer::DirectXRenderer;
use crate::directx_renderer::tests::TestWindow;

#[test]
fn partial_frames_match_full_draws_across_swap_chain_buffers() -> Result<()> {
    let window = TestWindow::new()?;
    let reference_window = TestWindow::new()?;
    let devices = DirectXDevices::new(true)?;

    let mut renderer = DirectXRenderer::new(window.0, &devices, false)?;
    let mut reference = DirectXRenderer::new(reference_window.0, &devices, false)?;

    renderer.resize(size(DevicePixels(64), DevicePixels(64)))?;
    reference.resize(size(DevicePixels(64), DevicePixels(64)))?;

    reference.atlas = renderer.atlas.clone();

    let mut tiles = Vec::new();

    for (is_emoji, subpixel_rendering) in [(false, false), (false, true), (true, false)] {
        let key = AtlasKey::Glyph(RenderGlyphParams {
            font_id: FontId(0),
            glyph_id: GlyphId(1),
            font_size: px(12.),
            subpixel_variant: point(0, 0),
            scale_factor: 1.,
            is_emoji,
            subpixel_rendering,
            dilation: 0,
        });

        let bytes = if is_emoji || subpixel_rendering {
            vec![180; 8 * 8 * 4]
        } else {
            vec![180; 8 * 8]
        };

        tiles.push(
            renderer
                .atlas
                .get_or_insert_with(&key, &mut || {
                    Ok(Some((
                        size(DevicePixels(8), DevicePixels(8)),
                        Cow::Borrowed(&bytes),
                    )))
                })?
                .expect("test glyph bitmap is nonempty"),
        );
    }

    let make_query = || -> Result<ID3D11Query> {
        let desc = D3D11_QUERY_DESC {
            Query: D3D11_QUERY_PIPELINE_STATISTICS,
            MiscFlags: 0,
        };

        let mut query = None;

        // SAFETY: The descriptor and output match the query API.
        unsafe {
            devices.device.CreateQuery(&desc, Some(&mut query))?;
        }

        query.context("pipeline query missing")
    };

    let partial_query = make_query()?;
    let full_query = make_query()?;

    let mut partial_pixels = 0;
    let mut full_pixels = 0;
    let mut partial_vertices = 0;
    let mut full_vertices = 0;

    for appearance in [
        WindowBackgroundAppearance::Opaque,
        WindowBackgroundAppearance::Transparent,
    ] {
        renderer.set_background_appearance(appearance)?;
        reference.set_background_appearance(appearance)?;

        for frame in 0..24 {
            let scene = scene_for_frame(frame, &tiles);

            // SAFETY: Queries use the same device and never overlap.
            unsafe {
                devices.device_context.Begin(&partial_query);
            }

            let region = renderer.render_frame(&scene, None)?;

            // SAFETY: The matching Begin completed before this call.
            unsafe {
                devices.device_context.End(&partial_query);
            }

            if frame >= 3 {
                let region = region.expect("a small update must use a partial draw");

                assert!((region.right - region.left) * (region.bottom - region.top) < 64 * 64 / 4);
                assert!(renderer.partial_scene.quads.len() < scene.quads.len());
            }

            let image = renderer.read_render_target()?;
            let partial_stats = pipeline_stats(&devices.device_context, &partial_query)?;

            // SAFETY: Queries use the same device and never overlap.
            unsafe {
                devices.device_context.Begin(&full_query);
            }

            reference.render(&scene, None)?;

            // SAFETY: The matching Begin completed before this call.
            unsafe {
                devices.device_context.End(&full_query);
            }

            let expected = reference.read_render_target()?;
            let full_stats = pipeline_stats(&devices.device_context, &full_query)?;

            if frame >= 3 {
                partial_pixels += partial_stats.PSInvocations;
                full_pixels += full_stats.PSInvocations;
                partial_vertices += partial_stats.VSInvocations;
                full_vertices += full_stats.VSInvocations;
            }

            assert_eq!(image, expected, "frame {frame}, appearance {appearance:?}");

            renderer.present(region)?;

            renderer.force_full_present = false;
        }
    }

    assert!(partial_pixels < full_pixels / 2);
    assert!(partial_vertices < full_vertices);

    println!(
        "partial rendering: pixel shader invocations {partial_pixels}/{full_pixels}, vertex shader invocations {partial_vertices}/{full_vertices}"
    );

    renderer.resize(size(DevicePixels(80), DevicePixels(72)))?;
    reference.resize(size(DevicePixels(80), DevicePixels(72)))?;

    assert!(renderer.render_frame(&Scene::default(), None)?.is_none());
    assert_eq!(
        renderer.read_render_target()?,
        reference.render_to_image(&Scene::default(), WindowBackgroundAppearance::Transparent)?,
    );

    Ok(())
}

#[test]
fn reused_atlas_identifiers_refresh_pixels_without_scene_changes() -> Result<()> {
    let window = TestWindow::new()?;
    let devices = DirectXDevices::new(true)?;

    let mut renderer = DirectXRenderer::new(window.0, &devices, false)?;

    renderer.resize(size(DevicePixels(64), DevicePixels(64)))?;

    let key = AtlasKey::Image(RenderImageParams {
        image_id: ImageId(123),
        frame_index: 0,
    });

    let mut previous_tile = None;

    for color in [0, 255] {
        let bytes: Vec<_> = (0..64).flat_map(|_| [color, color, color, 255]).collect();

        if previous_tile.is_some() {
            renderer.atlas.remove(&key);
        }

        let tile = renderer
            .atlas
            .get_or_insert_with(&key, &mut || {
                Ok(Some((
                    size(DevicePixels(8), DevicePixels(8)),
                    Cow::Borrowed(&bytes),
                )))
            })?
            .expect("test image is nonempty");

        if let Some(previous) = previous_tile {
            assert_eq!(
                tile, previous,
                "the replacement must reuse identical scene data"
            );
        }

        let mut scene = Scene::default();

        scene.insert_primitive(PolychromeSprite {
            order: 0,
            pad: 0,
            grayscale: PaddedBool32::from(false),
            opacity: 1.,
            bounds: Bounds {
                origin: point(ScaledPixels(8.), ScaledPixels(8.)),
                size: size(ScaledPixels(8.), ScaledPixels(8.)),
            },
            content_mask: ContentMask {
                bounds: Bounds {
                    origin: point(ScaledPixels(0.), ScaledPixels(0.)),
                    size: size(ScaledPixels(64.), ScaledPixels(64.)),
                },
            },
            corner_radii: Default::default(),
            tile,
        });

        scene.finish();

        for _ in 0..3 {
            assert!(renderer.render_frame(&scene, None)?.is_none());
            assert_eq!(
                renderer.read_render_target()?.get_pixel(12, 12).0,
                [color, color, color, 255]
            );

            renderer.present(None)?;

            renderer.force_full_present = false;
        }

        previous_tile = Some(tile);
    }

    Ok(())
}

fn scene_for_frame(frame: usize, tiles: &[AtlasTile]) -> Scene {
    let bounds = |x, y, width, height| Bounds {
        origin: point(ScaledPixels(x), ScaledPixels(y)),
        size: size(ScaledPixels(width), ScaledPixels(height)),
    };

    let content_mask = ContentMask {
        bounds: bounds(0., 0., 64., 64.),
    };

    let mut scene = Scene::default();

    scene.insert_primitive(Quad {
        bounds: content_mask.bounds,
        content_mask,
        background: white().opacity(0.25).into(),
        ..Quad::default()
    });

    for y in 0..4 {
        for x in 0..4 {
            scene.insert_primitive(Quad {
                bounds: bounds(x as f32 * 16., y as f32 * 16., 7., 7.),
                content_mask,
                background: red().opacity(0.5).into(),
                ..Quad::default()
            });
        }
    }

    scene.insert_primitive(Shadow {
        order: 0,
        blur_radius: ScaledPixels(2.),
        bounds: bounds(8., 8., 20., 20.),
        content_mask,
        corner_radii: Default::default(),
        color: black().opacity(0.5),
        element_bounds: bounds(12., 12., 10., 10.),
        element_corner_radii: Default::default(),
        inset: 0,
        pad: 0,
    });

    scene.insert_primitive(MonochromeSprite {
        order: 0,
        pad: 0,
        bounds: bounds(10., 10., 8., 8.),
        content_mask,
        color: red(),
        tile: tiles[0],
        transformation: TransformationMatrix::unit(),
    });

    scene.insert_primitive(SubpixelSprite {
        order: 0,
        pad: 0,
        bounds: bounds(13., 10., 8., 8.),
        content_mask,
        color: black(),
        tile: tiles[1],
        transformation: TransformationMatrix::unit(),
    });

    scene.insert_primitive(PolychromeSprite {
        order: 0,
        pad: 0,
        bounds: bounds(15., 14., 8., 8.),
        content_mask,
        grayscale: PaddedBool32::from(false),
        opacity: 0.5,
        corner_radii: Default::default(),
        tile: tiles[2],
    });

    scene.insert_primitive(Underline {
        order: 0,
        pad: 0,
        bounds: bounds(8., 18., 20., 3.),
        content_mask,
        color: black(),
        thickness: ScaledPixels(1.),
        wavy: PaddedBool32::from(true),
    });

    // Moving and removing a translucent primitive must clear its
    // previous position without accumulating alpha on the backdrop.
    if frame % 7 != 6 {
        scene.insert_primitive(Quad {
            bounds: bounds(10.25 + (frame % 3) as f32, 12.5, 4.5, 8.),
            content_mask,
            background: black().opacity(0.5).into(),
            ..Quad::default()
        });
    }

    scene.finish();

    scene
}

fn pipeline_stats(
    context: &ID3D11DeviceContext,
    query: &ID3D11Query,
) -> Result<D3D11_QUERY_DATA_PIPELINE_STATISTICS> {
    let mut stats = D3D11_QUERY_DATA_PIPELINE_STATISTICS::default();

    // SAFETY: A staging readback completed after End, so the query is ready.
    // The output structure and size match the pipeline statistics query.
    unsafe {
        context.GetData(
            query,
            Some(&mut stats as *mut _ as _),
            std::mem::size_of_val(&stats) as u32,
            0,
        )?;
    }

    Ok(stats)
}
