#[cfg(target_os = "macos")]
mod shared_surface;

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(target_os = "macos")]
    use art3m1s_render::artemis::FramePlan;
    use art3m1s_render::artemis::{FramePacket, FrameShaderLanguage, SpriteRenderer};
    use art3m1s_render::backend::gl::GlBackend;
    #[cfg(target_os = "macos")]
    use art3m1s_render::backend::gl::platform;
    use art3m1s_render::backend::{Extent2D, FrameTarget, GpuBackend};
    #[cfg(target_os = "macos")]
    use art3m1s_render::backend::{NativeSurface, NativeSurfaceKind};
    #[cfg(target_os = "macos")]
    use art3m1s_render::draw::NativeEmoteMaterial;
    use std::io::Read;
    use std::path::PathBuf;

    #[cfg(target_os = "macos")]
    let angle = std::env::var_os("ARTEMIS_GL_SMOKE_ANGLE_DIR").map(PathBuf::from);
    #[cfg(target_os = "macos")]
    let new_backend = |width, height| match &angle {
        Some(directory) => GlBackend::new_host_angle(directory, width, height),
        None => GlBackend::new(platform::GfxBackend::Cgl, width, height),
    };
    #[cfg(target_os = "linux")]
    let new_backend = GlBackend::new_host_egl;
    #[cfg(target_os = "macos")]
    let gles = angle.is_some();
    #[cfg(target_os = "linux")]
    let gles = true;
    let sources: Vec<_> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    if !sources.is_empty() {
        let mut backend = new_backend(4, 4)?;
        backend.begin_access();
        let mut failures = 0;
        for path in &sources {
            let result = std::fs::read(path)
                .map_err(|error| error.to_string())
                .and_then(|source| {
                    backend
                        .register_glsl_shader("release-effect", &source)
                        .map_err(|error| error.to_string())
                });
            if let Err(error) = result {
                failures += 1;
                eprintln!(
                    "{}: {error}",
                    path.file_name().unwrap_or_default().to_string_lossy()
                );
            }
        }
        backend.end_access();
        println!(
            "GLSL compile and link: {}/{} passed",
            sources.len() - failures,
            sources.len()
        );
        return if failures == 0 {
            Ok(())
        } else {
            Err("one or more GLSL programs failed".into())
        };
    }

    let mut input = Vec::new();
    std::io::stdin().read_to_end(&mut input)?;
    let len =
        u32::from_le_bytes(input.get(8..12).ok_or("missing ARFP packet")?.try_into()?) as usize;
    let (packet, mut rgba) = input.split_at_checked(len).ok_or("truncated ARFP packet")?;
    let packet = FramePacket::decode(packet)?;
    let sprite = &packet.sprites()[0];
    let effect = sprite.shader.as_ref().ok_or("missing GLSL effect")?;
    assert_eq!(effect.shader.language, FrameShaderLanguage::Glsl);
    assert!(effect.mask.is_some() && effect.user.is_some());
    let mut textures = Vec::new();
    for descriptor in packet.textures() {
        let (pixels, remaining) = rgba.split_at(descriptor.rgba_len().unwrap());
        textures.push(pixels);
        rgba = remaining;
    }
    assert!(rgba.is_empty());

    let [width, height] = packet.stage_size;
    let extent = Extent2D::new(width, height);
    #[cfg(target_os = "macos")]
    let surface = angle
        .as_ref()
        .map(|_| shared_surface::SharedSurface::new(extent))
        .transpose()?;
    let expected = std::env::var("ARTEMIS_GL_SMOKE_EXPECT_RGBA")
        .ok()
        .map(|value| serde_json::from_str::<[u8; 4]>(&value))
        .transpose()?
        .unwrap_or([5, 24, 24, 255]);
    let mut renderer = SpriteRenderer::new(new_backend(width, height)?);
    renderer.backend_mut().begin_access();
    renderer.backend_mut().begin_frame(FrameTarget::Main)?;
    renderer.render_packet(1, &packet, |index| Ok(textures[index]), [0.0; 4])?;
    renderer.backend_mut().end_frame();
    let mut rgba = vec![0; (width * height * 4) as usize];
    assert_eq!(
        renderer
            .backend_mut()
            .readback(FrameTarget::Main, extent, &mut rgba)?,
        rgba.len()
    );
    for pixel in rgba.chunks_exact(4) {
        assert!(
            pixel.iter().zip(expected).all(|(&a, b)| a.abs_diff(b) <= 1),
            "expected {expected:?}, got {pixel:?}"
        );
    }
    #[cfg(target_os = "macos")]
    if let Some(surface) = &surface {
        let backend = renderer.backend_mut();
        backend.set_native_surface(NativeSurface {
            kind: NativeSurfaceKind::AppleIoSurface,
            handle: surface.handle()?.as_ptr(),
            extent,
        })?;
        backend.present(None)?;
        assert_eq!(surface.pixels()?, rgba, "IOSurface pixels");

        let mut emote = sprite.clone();
        emote.shader = None;
        emote.mask = None;
        emote.emote_material = Some(NativeEmoteMaterial {
            corner_colors: [[0.5, 1.0, 1.0, 1.0]; 4],
            uv_rect: [0.0, 0.0, 1.0, 1.0],
            blend_mode: 0,
            clip_rect: [0.0, 0.0, 1.0, 1.0],
            wipe: [1.0, 0.0, 0.0],
        });
        backend.begin_frame(FrameTarget::Main)?;
        backend.clear([0.0; 4]);
        renderer.render_plan(
            FramePlan {
                runtime: 1,
                epoch: packet.session_epoch,
                stage_size: packet.stage_size,
                sprites: &[emote],
                groups: &[],
                masks: &[],
                transition: None,
            },
            |index| packet.textures().get(index).copied(),
            |index| Ok(textures[index]),
            [0.0; 4],
        )?;
        let backend = renderer.backend_mut();
        backend.end_frame();
        backend.readback(FrameTarget::Main, extent, &mut rgba)?;
        assert!(
            rgba.chunks_exact(4)
                .all(|pixel| { pixel[0].abs_diff(9) <= 1 && pixel[1..] == [34, 51, 255] })
        );
        backend.present(None)?;
        let presented = surface.pixels()?;
        assert!(
            presented == rgba,
            "final copy must not apply the E-Mote tint again: {:?} != {:?}",
            &presented[..4],
            &rgba[..4]
        );
        backend.clear_native_surface();
    }
    renderer.clear_cached_shaders();
    renderer.clear_cached_textures();
    println!("Artemis GLSL frame: {width}x{height} pixels verified");
    if std::env::var_os("ARTEMIS_GL_SMOKE_SOURCE").is_none() {
        if !gles {
            renderer.backend_mut().end_access();
        }
        check_backdrop(new_backend(4, 4)?, gles)?;
        if gles {
            let backend = renderer.backend_mut();
            assert_eq!(
                backend.readback_owned(FrameTarget::Main, extent)?,
                rgba,
                "closing the second EGL renderer must restore the first context"
            );
            backend.end_access();
            println!("Artemis EGL context restore verified");
        }
    } else {
        renderer.backend_mut().end_access();
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn check_backdrop(
    backend: art3m1s_render::backend::gl::GlBackend,
    gles: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use art3m1s_render::artemis::{
        FrameGroup, FramePlan, FrameShader, FrameShaderEffect, FrameShaderLanguage, FrameSprite,
        FrameTextureDescriptor, SpriteBlend, SpriteRenderer,
    };
    use art3m1s_render::backend::{Extent2D, FrameTarget, GpuBackend};
    #[cfg(target_os = "macos")]
    use art3m1s_render::backend::{NativeSurface, NativeSurfaceKind};
    use std::sync::Arc;

    let extent = Extent2D::new(4, 4);
    let top = [32, 64, 96, 255];
    let bottom = [64, 96, 128, 255];
    let clear = [12, 20, 28, 255];
    let base = [top; 8]
        .into_iter()
        .chain([bottom; 8])
        .flatten()
        .collect::<Vec<_>>();
    let pixels: [&[u8]; 4] = [&base, &[16, 0, 0, 255], &[8, 0, 0, 255], &[4, 0, 0, 255]];
    let effect = |revision, sampler, expression: &str, inputs: bool| {
        let (header, sample, output) = if gles {
            (
                "varying mediump vec2 resultCoord0, resultCoord1;",
                "texture2D",
                "gl_FragColor",
            )
        } else {
            (
                "#version 330 core\nin vec2 resultCoord0, resultCoord1;\nout vec4 color;",
                "texture",
                "color",
            )
        };
        let expression = expression.replace("SAMPLE", sample);
        let source = format!(
            "{header}\nuniform sampler2D {sampler}, textureFore, textureMask, textureUser;\n\
             void main() {{ {output} = {expression}; }}"
        );
        FrameShaderEffect {
            shader: Arc::new(FrameShader {
                revision,
                language: FrameShaderLanguage::Glsl,
                source: source.into_bytes().into_boxed_slice(),
            }),
            uniforms: Default::default(),
            mask: inputs.then_some(2),
            user: inputs.then_some(3),
        }
    };
    let sprite = |layer: &str, texture, clip_bounds, shader| FrameSprite {
        layer_id: layer.into(),
        texture,
        mask: None,
        transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
        quad_size: [4.0; 2],
        uv_offset: [0.0; 2],
        uv_scale: [1.0; 2],
        clip_bounds,
        opacity: 1.0,
        color_multiply: [1.0; 3],
        grayscale: false,
        negative: false,
        blend: SpriteBlend::Alpha,
        mesh: None,
        emote_material: None,
        shader,
    };
    let sprites = [
        sprite("base", 0, Some([0.0, 0.0, 3.0, 4.0]), None),
        sprite(
            "first",
            1,
            Some([1.0, 0.0, 2.0, 3.0]),
            Some(effect(
                1,
                "textureBack",
                "SAMPLE(textureBack, resultCoord0) + vec4(SAMPLE(textureFore, resultCoord1).r + \
             SAMPLE(textureMask, resultCoord1).r + SAMPLE(textureUser, resultCoord1).r, 0.0, 0.0, 0.0)",
                true,
            )),
        ),
        sprite(
            "second",
            1,
            Some([2.0, 1.0, 2.0, 3.0]),
            Some(effect(
                2,
                "samplerBack",
                "SAMPLE(samplerBack, resultCoord0 - vec2(0.25, 0.0)) + vec4(0.0, 32.0 / 255.0, 0.0, 0.0)",
                false,
            )),
        ),
    ];
    let group = FrameGroup {
        sprites: 0..sprites.len(),
        mask: None,
        clip_bounds: None,
        opacity: 1.0,
        color_multiply: [0.5, 1.0, 1.0],
        grayscale: false,
        negative: false,
        opaque: false,
        blend: SpriteBlend::Alpha,
        stencil: None,
        shader: None,
    };
    let mut composed = sprites.clone();
    let composite_effect = FrameGroup {
        sprites: 2..3,
        clip_bounds: composed[2].clip_bounds.take(),
        shader: composed[2].shader.take(),
        ..group.clone()
    };
    let mut renderer = SpriteRenderer::new(backend);
    #[cfg(target_os = "macos")]
    let surface = gles
        .then(|| shared_surface::SharedSurface::new(extent))
        .transpose()?;
    renderer.backend_mut().begin_access();
    for (layout, sprites, groups) in [
        ("stage", &sprites, &[][..]),
        ("inside group", &sprites, std::slice::from_ref(&group)),
        (
            "group composite",
            &composed,
            std::slice::from_ref(&composite_effect),
        ),
    ] {
        renderer.backend_mut().begin_frame(FrameTarget::Main)?;
        renderer.render_plan(
            FramePlan {
                runtime: 2,
                epoch: 1,
                stage_size: [4; 2],
                sprites,
                groups,
                masks: &[],
                transition: None,
            },
            |index| {
                Some(FrameTextureDescriptor {
                    revision: index as u64 + 1,
                    width: if index == 0 { 4 } else { 1 },
                    height: if index == 0 { 4 } else { 1 },
                })
            },
            |index| Ok(pixels[index]),
            clear.map(|channel| f32::from(channel) / 255.0),
        )?;
        let backend = renderer.backend_mut();
        backend.end_frame();
        let mut rgba = [0; 64];
        backend.readback(FrameTarget::Main, extent, &mut rgba)?;
        for (i, pixel) in rgba.chunks_exact(4).enumerate() {
            let (x, y) = (i % 4, i / 4);
            let mut expected = if (x, y) == (3, 0) {
                clear
            } else if y < 2 {
                top
            } else {
                bottom
            };
            if (1..=2).contains(&x) && y < 3 || x >= 2 && (1..3).contains(&y) {
                expected[0] += 28;
            }
            if x >= 2 && y >= 1 {
                expected[1] += 32;
            }
            if layout == "inside group" && (x, y) != (3, 0) {
                expected[0] /= 2;
            }
            assert!(
                pixel.iter().zip(expected).all(|(&a, b)| a.abs_diff(b) <= 1),
                "GLSL backdrop on {layout} at ({x}, {y}): expected {expected:?}, got {pixel:?}"
            );
        }
        #[cfg(target_os = "macos")]
        if let Some(surface) = &surface {
            backend.set_native_surface(NativeSurface {
                kind: NativeSurfaceKind::AppleIoSurface,
                handle: surface.handle()?.as_ptr(),
                extent,
            })?;
            backend.present(None)?;
            assert_eq!(surface.pixels()?, rgba, "GLSL backdrop IOSurface pixels");
            backend.clear_native_surface();
        }
    }
    renderer.clear_cached_shaders();
    renderer.clear_cached_textures();
    renderer.backend_mut().end_access();
    println!("Artemis GLSL backdrop: stage and group pixels verified");
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn main() {
    eprintln!("This probe requires macOS or Linux.");
    std::process::exit(2);
}
