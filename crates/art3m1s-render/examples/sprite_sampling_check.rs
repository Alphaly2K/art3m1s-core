//! Run on macOS with `cargo run --features gl,metal --example sprite_sampling_check`.
//! Checks the composition target before presentation or upscaling.

#[cfg(not(all(target_os = "macos", any(feature = "metal", feature = "gl"))))]
fn main() {
    eprintln!("This probe requires macOS with the metal or gl feature");
}

#[cfg(all(target_os = "macos", any(feature = "metal", feature = "gl")))]
fn main() -> Result<(), String> {
    #[cfg(feature = "metal")]
    check(art3m1s_render::backend::metal::MetalBackend::new(16, 16)?)?;
    #[cfg(feature = "gl")]
    check(art3m1s_render::backend::gl::GlBackend::new(
        art3m1s_render::backend::gl::platform::GfxBackend::Cgl,
        16,
        16,
    )?)?;
    Ok(())
}

#[cfg(all(target_os = "macos", any(feature = "metal", feature = "gl")))]
fn check(mut gpu: impl art3m1s_render::GpuBackend) -> Result<(), String> {
    use art3m1s_render::*;
    use glam::{Affine2, Vec2};

    let atlas = (0..8)
        .flat_map(|y| {
            (0..9).flat_map(move |x| match x {
                8 => [255, 0, 0, 255],
                _ if (x + y) % 2 == 0 => [255; 4],
                _ => [0, 0, 0, 255],
            })
        })
        .collect::<Vec<_>>();
    gpu.begin_access();
    let texture = gpu.create_texture(
        "sprite-sampling-atlas",
        TextureDesc::sampled_rgba8(9, 8),
        TextureData::Rgba8(&atlas),
    )?;
    let mut sprite = DrawCommand {
        texture,
        size: TextureInfo {
            width: 9,
            height: 8,
        },
        transform: Affine2::IDENTITY,
        opacity: 1.,
        blend: BlendMode::Alpha,
        color: ColorFilter::default(),
        clip: ClipRect {
            quad_size: [2., 2.],
            uv_offset: [0., 0.],
            uv_scale: [8. / 9., 1.],
        },
        clip_bounds: None,
        shader: Some(ShaderEffect {
            name: SPRITE_NEAREST_SHADER.into(),
            uniforms: Default::default(),
            mask_texture: None,
            user_texture: None,
        }),
        mesh: None,
        stencil: None,
        native_emote: None,
    };
    let mut draw = |sprite: DrawCommand| -> Result<Vec<[u8; 4]>, String> {
        let mut frame = DrawList::new();
        frame.push(sprite);
        gpu.begin_frame(FrameTarget::Main)?;
        gpu.render_with_clear_color(&frame, [0.; 4]);
        gpu.end_frame();
        let mut bytes = [0; 16 * 16 * 4];
        gpu.readback(FrameTarget::Main, Extent2D::new(16, 16), &mut bytes)?;
        Ok(bytes
            .chunks_exact(4)
            .map(|p| p.try_into().unwrap())
            .collect())
    };
    let nearest = draw(sprite.clone())?;
    sprite.shader.as_mut().unwrap().name = SPRITE_RESAMPLE_SHADER.into();
    let filtered = draw(sprite.clone())?;
    for y in 0..2 {
        for x in 0..2 {
            let i = y * 16 + x;
            assert!(
                matches!(nearest[i], [0, 0, 0, 255] | [255, 255, 255, 255]),
                "nearest sample: {:?}",
                nearest[i]
            );
            assert_eq!(filtered[i][3], 255);
            assert!(
                filtered[i][..3].iter().all(|&v| v.abs_diff(128) <= 1),
                "{:?}",
                filtered[i]
            );
        }
    }

    sprite.transform = Affine2::from_translation(Vec2::splat(8.))
        * Affine2::from_angle(0.35)
        * Affine2::from_translation(Vec2::splat(-4.));
    sprite.clip.quad_size = [8., 8.];
    sprite.clip.uv_offset = [8. / 9., 0.];
    sprite.clip.uv_scale = [1. / 9., 1.];
    let rotated = draw(sprite)?;
    let covered: Vec<_> = rotated.iter().filter(|p| p[3] != 0).collect();
    assert!((60..=68).contains(&covered.len()));
    assert!(covered.iter().all(|p| **p == [255, 0, 0, 255]));
    println!(
        "{:?}: 4x reduction, nearest sampling, and rotated atlas isolation",
        gpu.backend_info().kind
    );
    gpu.end_access();
    Ok(())
}
