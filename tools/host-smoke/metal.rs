#[cfg(target_os = "macos")]
mod metal_abi;
#[cfg(target_os = "macos")]
mod shared_surface;

#[cfg(target_os = "macos")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use crate::metal_abi::{Frame, NativeHost, Session};
    use crate::shared_surface::SharedSurface;
    use art3m1s_render::artemis::{FramePacket, SpriteRenderer};
    use art3m1s_render::backend::{FrameTarget, GpuBackend, RenderRegion, metal::MetalBackend};
    use art3m1s_render::types::Extent2D;
    use image::{ImageFormat, RgbaImage, imageops::FilterType};
    use serde_json::{Value, json};
    use std::{ffi::CString, fs, os::unix::ffi::OsStrExt, path::PathBuf};

    const IO_STAT: (u16, u32) = (3, 1);
    const IO_READ: (u16, u32) = (3, 2);
    const RENDER_CAPTURE: (u16, u32) = (6, 1);
    const RENDER_SAVE_CAPTURE: (u16, u32) = (6, 2);

    struct SaveDirectory(PathBuf);
    impl Drop for SaveDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    let save_dir = std::env::temp_dir().join(format!("artemis-metal-{}", std::process::id()));
    fs::create_dir(&save_dir)?;
    let save_dir = SaveDirectory(save_dir);

    let boot = br#"*top
[lyprop id=group intermediate_render=1 alpha=128]
[calllua function=create]
[flip]
[lydel id=group]
[lyshader id=solid file=solid.hlsl]
[lyc id=blue width=4 height=4 color=ffffffff]
[lyprop id=blue shader=solid shaderconstant=tint tint="0,0,1"]
[trans type=1 time=100 input=0]
[flip]
[lydel id=blue]
[lyc id=green width=4 height=2 color=ff00ff00]
[lyc id=yellow width=4 height=2 color=ffffff00]
[lyprop id=yellow top=2]
[trans type=2 rule=rule vague=64 time=100 input=0]
[takess]
[lydel id=green]
[lydel id=yellow]
[lyc id=menu width=4 height=4 color=ffff0000]
[savess file=thumb width=2 height=2]
[stop]
[lua]
function create(e)
    local layer = e:createEmoteLayer{id='group.red', files={'red.psb'}, width=4, height=4}
    layer:setCoord(-2, -2, 0, 0)
end
[/lua]
"#;
    let mut rule = std::io::Cursor::new(Vec::new());
    RgbaImage::from_fn(4, 4, |x, _| {
        let level = [0, 128, 255, 255][x as usize];
        image::Rgba([level, level, level, 255])
    })
    .write_to(&mut rule, ImageFormat::Png)?;
    let rule = rule.into_inner();
    let model = include_bytes!("../../src/host/script_io/fixtures/emote-red.psb");
    let source = br#"
float3 tint;
float alpha;
void ps(float2 uv : TEXCOORD0, out float4 result : COLOR0) {
    result = float4(tint, alpha);
}
"#;
    let mut args = std::env::args_os().skip(1);
    let library = args.next().ok_or("expected the Core dylib path")?;
    let adapter = args.next().ok_or("expected the Metal Host dylib path")?;
    let session = Session::open(&CString::new(library.as_bytes())?)?;
    let shared = SharedSurface::new(Extent2D::new(4, 4))?;
    let replacement = SharedSurface::new(Extent2D::new(8, 8))?;
    let mut native = NativeHost::open(&CString::new(adapter.as_bytes())?, &session)?;
    assert_eq!(native.present(0), Err(-6));
    assert_eq!(native.wrong_thread_status(), -3);
    native.set_surface(Some(&shared))?;
    assert_eq!(native.present(0), Ok(None));
    let mut renderer = SpriteRenderer::new(MetalBackend::new(4, 4)?);
    let mut capture = None;
    let mut service_until_event = |native: &NativeHost<'_>,
                                   id,
                                   ms|
     -> Result<(), Box<dyn std::error::Error>> {
        for turn in 0..32 {
            let waiting = session.step(id, ms)?;
            let mut bytes = [0; 4096];
            let mut remaining = session.poll(&mut bytes)?;
            while !remaining.is_empty() {
                let u32_at = |i| u32::from_le_bytes(remaining[i..i + 4].try_into().unwrap());
                let u64_at = |i| u64::from_le_bytes(remaining[i..i + 8].try_into().unwrap());
                let length = u32_at(0) as usize;
                let request_id = u64_at(40);
                if request_id != 0 {
                    let service = (u16::from_le_bytes([remaining[6], remaining[7]]), u32_at(8));
                    let args: Value =
                        serde_json::from_slice(&remaining[64..64 + u32_at(56) as usize])?;
                    let bytes = match args["key"].as_str() {
                        Some("boot.iet") => Some(boot.as_slice()),
                        Some("red.psb") => Some(model.as_slice()),
                        Some("solid.hlsl") => Some(source.as_slice()),
                        Some("rule.png") => Some(rule.as_slice()),
                        _ => None,
                    };
                    let (metadata, blob) = match (service, bytes) {
                        (IO_STAT, Some(bytes)) => (
                            json!({"status":"ok","found":true,"size":bytes.len().to_string(),"revision":"1"}),
                            Vec::new(),
                        ),
                        (IO_STAT, None) => (json!({"status":"ok","found":false}), Vec::new()),
                        (IO_READ, Some(bytes)) => (
                            json!({"status":"ok","revision":"1","eof":true}),
                            bytes.to_vec(),
                        ),
                        (RENDER_CAPTURE, _) => {
                            let expected_id =
                                args["render_frame_id"].as_str().unwrap().parse::<u64>()?;
                            let mut pixels = vec![0; 4 * 4 * 4];
                            let extent = Extent2D::new(4, 4);
                            assert_eq!(
                                native.capture(expected_id, extent, &mut pixels[..63]),
                                Err(-5)
                            );
                            assert_eq!(
                                native.capture(expected_id - 1, extent, &mut pixels),
                                Err(-7)
                            );
                            native
                                .capture(expected_id, extent, &mut pixels)
                                .map_err(|code| format!("Metal capture returned {code}"))?;
                            check(&pixels, |_, y| if y < 2 { GREEN } else { YELLOW });
                            capture = Some((request_id, pixels));
                            (json!({"status":"ok"}), Vec::new())
                        }
                        (RENDER_SAVE_CAPTURE, _) => {
                            let (capture_id, pixels) = capture.as_ref().expect("confirmed capture");
                            assert_eq!(
                                args,
                                json!({
                                    "capture_id": capture_id.to_string(), "namespace": "save",
                                    "key": "thumb.png", "width": 2, "height": 2,
                                })
                            );
                            let image = RgbaImage::from_raw(4, 4, pixels.clone()).unwrap();
                            let thumbnail =
                                image::imageops::resize(&image, 2, 2, FilterType::Triangle);
                            let pending = save_dir.0.join("thumb.pending");
                            thumbnail.save_with_format(&pending, ImageFormat::Png)?;
                            fs::rename(pending, save_dir.0.join("thumb.png"))?;
                            (json!({"status":"ok"}), Vec::new())
                        }
                        _ => panic!("unexpected service: {service:?}"),
                    };
                    session.reply(
                        service,
                        u64_at(24),
                        request_id,
                        &metadata.to_string(),
                        &blob,
                    )?;
                }
                remaining = &remaining[length..];
            }
            if !waiting {
                return Ok(());
            }
            assert!(turn < 31, "Host did not settle");
        }
        Ok(())
    };
    service_until_event(&native, 1, 0)?;

    const BLUE: [u8; 4] = [0, 0, 255, 255];
    const GREEN: [u8; 4] = [0, 255, 0, 255];
    const YELLOW: [u8; 4] = [255, 255, 0, 255];

    fn draw(
        renderer: &mut SpriteRenderer<MetalBackend>,
        frame: &Frame<'_>,
    ) -> Result<Vec<u8>, String> {
        let packet = frame.decode()?;
        draw_target(renderer, |renderer| draw_packet(renderer, frame, &packet))
    }

    fn draw_packet(
        renderer: &mut SpriteRenderer<MetalBackend>,
        frame: &Frame<'_>,
        packet: &FramePacket,
    ) -> Result<RenderRegion, String> {
        renderer.render_packet(
            frame.runtime(),
            packet,
            |index| frame.pixels(index),
            [0.0, 0.0, 0.0, 1.0],
        )
    }

    fn render_scene(
        renderer: &mut SpriteRenderer<MetalBackend>,
        draw: impl FnOnce(&mut SpriteRenderer<MetalBackend>) -> Result<RenderRegion, String>,
    ) -> Result<(), String> {
        renderer.backend_mut().begin_frame(FrameTarget::Main)?;
        let rendered = draw(renderer);
        renderer.backend_mut().end_frame();
        rendered.map(|_| ())
    }

    fn draw_target(
        renderer: &mut SpriteRenderer<MetalBackend>,
        draw: impl FnOnce(&mut SpriteRenderer<MetalBackend>) -> Result<RenderRegion, String>,
    ) -> Result<Vec<u8>, String> {
        renderer.backend_mut().begin_access();
        let result = render_scene(renderer, draw).and_then(|()| {
            renderer
                .backend_mut()
                .readback_owned(FrameTarget::Main, Extent2D::new(4, 4))
        });
        renderer.backend_mut().end_access();
        result
    }

    fn check(pixels: &[u8], expected: impl Fn(usize, usize) -> [u8; 4]) {
        assert_eq!(pixels.len(), 4 * 4 * 4);
        for (i, pixel) in pixels.chunks_exact(4).enumerate() {
            let (x, y) = (i % 4, i / 4);
            let expected = expected(x, y);
            assert!(
                pixel.iter().zip(expected).all(|(&a, b)| a.abs_diff(b) <= 1),
                "pixel ({x}, {y}): expected {expected:?}, got {pixel:?}"
            );
        }
    }

    let old = session.frame(0)?;
    let packet = old.decode()?;
    let bad_pixels = draw_target(&mut renderer, |renderer| {
        renderer.render_packet(session.id(), &packet, |_| Ok(&[]), [0.0, 0.0, 0.0, 1.0])
    });
    assert!(bad_pixels.unwrap_err().contains("texture byte length"));
    let pixels = draw(&mut renderer, &old)?;
    check(&pixels, |_, _| [128, 0, 0, 255]);
    let cached = draw_target(&mut renderer, |renderer| {
        renderer.render_packet(
            session.id(),
            &packet,
            |_| Err("unexpected pixel request".into()),
            [0.0, 0.25, 0.5, 1.0],
        )
    })?;
    check(&cached, |_, _| [128, 32, 64, 255]);
    drop(old);
    for (id, ms, expected) in [
        (2, 10, [128, 0, 0, 255]),
        (3, 60, [64, 0, 128, 255]),
        (4, 110, BLUE),
    ] {
        service_until_event(&native, id, ms)?;
        let frame = session.frame(0)?;
        check(&draw(&mut renderer, &frame)?, |_, _| expected);
    }
    service_until_event(&native, 5, 120)?;
    let start = session.frame(0)?;
    assert!(start.decode()?.transition().unwrap().rule.is_some());
    check(&draw(&mut renderer, &start)?, |_, _| BLUE);
    drop(start);
    service_until_event(&native, 6, 170)?;
    let middle = native.present(0).expect("present rule frame").unwrap();
    assert_eq!(native.present(middle), Ok(None));
    let middle_pixels = shared.pixels()?;
    check(&middle_pixels, |x, y| match (x, y < 2) {
        (0, true) => GREEN,
        (0, false) => YELLOW,
        (1, true) => [0, 125, 130, 255],
        (1, false) => [125, 125, 130, 255],
        _ => BLUE,
    });
    native.set_surface(None)?;
    assert_eq!(native.present(0), Err(-6));
    native.set_surface(Some(&replacement))?;
    assert_eq!(native.present(middle), Ok(None));
    assert_eq!(native.present(0), Ok(Some(middle)));
    let before_capture = replacement.pixels()?;
    service_until_event(&native, 7, 220)?;
    assert_eq!(
        replacement.pixels()?,
        before_capture,
        "capture updated the shared surface"
    );
    assert_eq!(
        shared.pixels()?,
        middle_pixels,
        "present repainted the detached surface"
    );
    let menu = native.present(middle).expect("present menu frame").unwrap();
    assert_eq!(native.present(menu), Ok(None));
    let pixels = replacement.pixels()?;
    assert_eq!(pixels.len(), 8 * 8 * 4);
    assert!(
        pixels
            .chunks_exact(4)
            .all(|pixel| pixel == [255, 0, 0, 255])
    );
    let thumbnail = image::open(save_dir.0.join("thumb.png"))?.into_rgba8();
    assert_eq!(thumbnail.dimensions(), (2, 2));
    assert_eq!(thumbnail.get_pixel(0, 0).0, [36, 255, 0, 255]);
    assert_eq!(thumbnail.get_pixel(1, 0).0, [36, 255, 0, 255]);
    assert_eq!(thumbnail.get_pixel(0, 1).0, [219, 255, 0, 255]);
    assert_eq!(thumbnail.get_pixel(1, 1).0, [219, 255, 0, 255]);
    renderer.backend_mut().begin_access();
    renderer.clear_cached_textures();
    renderer.clear_cached_shaders();
    renderer.backend_mut().end_access();
    println!(
        "Artemis v2 Metal pixels passed (E-Mote, groups, transitions, HLSL, thumbnails and IOSurface)"
    );
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("This probe requires macOS and a Metal device");
    std::process::exit(1);
}
