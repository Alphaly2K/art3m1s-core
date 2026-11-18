#[cfg(target_os = "macos")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use art3m1s_core::host::{
        frame::FrameLease,
        protocol::{Domain, HostReply, MessageBody, MessageKind, RequestIdentity, TransportLimits},
        renderer::SpriteRenderer,
        runtime,
        script_io::{
            HostFrame, IO_READ, IO_STAT, RENDER_CAPTURE, RENDER_SAVE_CAPTURE, ScriptIoProgress,
        },
    };
    use art3m1s_render::backend::{FrameTarget, GpuBackend, metal::MetalBackend};
    use art3m1s_render::types::Extent2D;
    use image::{ImageFormat, RgbaImage, imageops::FilterType};
    use serde_json::{Value, json};
    use std::{fs, path::PathBuf};

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
    let handle = runtime::create(0, TransportLimits::default(), 4096)?;
    runtime::open_project(
        handle,
        b"[WINDOWS]\nWIDTH=4\nHEIGHT=4\nBOOT=boot.iet\n",
        "windows",
        None,
    )?;
    let step = |id, ms: u64| {
        runtime::step(
            handle,
            HostFrame {
                id,
                monotonic_ns: ms * 1_000_000,
                utc_unix_ms: 0,
                utc_offset_minutes: 0,
            },
        )
    };
    let mut renderer = SpriteRenderer::new(MetalBackend::new(4, 4)?);
    let mut capture = None;
    let mut service_until_event = |renderer: &mut SpriteRenderer<MetalBackend>,
                                   id,
                                   ms|
     -> Result<(), Box<dyn std::error::Error>> {
        for turn in 0..32 {
            match step(id, ms)? {
                ScriptIoProgress::Event => break,
                ScriptIoProgress::Waiting => {}
                progress => panic!("unexpected boot progress: {progress:?}"),
            }
            assert!(turn < 31, "Host did not finish booting");
            let mut bytes = [0; 4096];
            let poll = runtime::poll_events(handle, &mut bytes)?;
            let mut remaining = &bytes[..poll.written_bytes];
            while !remaining.is_empty() {
                let u32_at = |i| u32::from_le_bytes(remaining[i..i + 4].try_into().unwrap());
                let u64_at = |i| u64::from_le_bytes(remaining[i..i + 8].try_into().unwrap());
                let length = u32_at(0) as usize;
                let request_id = u64_at(40);
                if request_id != 0 {
                    let service = MessageKind {
                        domain: match u16::from_le_bytes([remaining[6], remaining[7]]) {
                            domain if domain == Domain::Io as u16 => Domain::Io,
                            domain if domain == Domain::Render as u16 => Domain::Render,
                            domain => panic!("unexpected request domain: {domain}"),
                        },
                        kind: u32_at(8),
                    };
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
                            let frame = runtime::acquire_latest(handle, expected_id - 1)?
                                .expect("capture frame");
                            assert_eq!(frame.render_frame_id, expected_id);
                            let pixels = draw(renderer, &frame)?;
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
                    runtime::submit_replies(
                        handle,
                        &[HostReply {
                            identity: RequestIdentity {
                                runtime: handle.as_raw(),
                                epoch: u64_at(24),
                                request_id,
                                service,
                            },
                            body: MessageBody::new(metadata.to_string(), blob)?,
                        }],
                    )?;
                }
                remaining = &remaining[length..];
            }
        }
        Ok(())
    };
    service_until_event(&mut renderer, 1, 0)?;

    const BLUE: [u8; 4] = [0, 0, 255, 255];
    const GREEN: [u8; 4] = [0, 255, 0, 255];
    const YELLOW: [u8; 4] = [255, 255, 0, 255];

    fn draw(
        renderer: &mut SpriteRenderer<MetalBackend>,
        frame: &FrameLease,
    ) -> Result<Vec<u8>, String> {
        let target = FrameTarget::Main;
        renderer.backend_mut().begin_access();
        let result = (|| {
            renderer.backend_mut().begin_frame(target)?;
            renderer.backend_mut().clear([0.0, 0.0, 0.0, 1.0]);
            let draw = renderer.render(frame);
            renderer.backend_mut().end_frame();
            draw?;
            renderer
                .backend_mut()
                .readback_owned(target, Extent2D::new(4, 4))
        })();
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

    let old = runtime::acquire_latest(handle, 0)?.expect("group frame");
    check(&draw(&mut renderer, &old)?, |_, _| [128, 0, 0, 255]);
    drop(old);
    for (id, ms, expected) in [
        (2, 10, [128, 0, 0, 255]),
        (3, 60, [64, 0, 128, 255]),
        (4, 110, BLUE),
    ] {
        service_until_event(&mut renderer, id, ms)?;
        let frame = runtime::acquire_latest(handle, 0)?.expect("transition frame");
        check(&draw(&mut renderer, &frame)?, |_, _| expected);
    }
    service_until_event(&mut renderer, 5, 120)?;
    let start = runtime::acquire_latest(handle, 0)?.expect("rule frame");
    assert!(start.transition.as_ref().unwrap().rule.is_some());
    check(&draw(&mut renderer, &start)?, |_, _| BLUE);
    drop(start);
    service_until_event(&mut renderer, 6, 170)?;
    let middle = runtime::acquire_latest(handle, 0)?.expect("rule frame");
    check(&draw(&mut renderer, &middle)?, |x, y| match (x, y < 2) {
        (0, true) => GREEN,
        (0, false) => YELLOW,
        (1, true) => [0, 125, 130, 255],
        (1, false) => [125, 125, 130, 255],
        _ => BLUE,
    });
    drop(middle);
    service_until_event(&mut renderer, 7, 220)?;
    let menu = runtime::acquire_latest(handle, 0)?.expect("menu frame");
    check(&draw(&mut renderer, &menu)?, |_, _| [255, 0, 0, 255]);
    drop(menu);
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
    runtime::destroy(handle)?;
    println!("Artemis v2 Metal pixels passed (E-Mote, groups, transitions, HLSL and thumbnails)");
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("This probe requires macOS and a Metal device");
    std::process::exit(1);
}
