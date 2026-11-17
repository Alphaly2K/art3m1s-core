#[cfg(target_os = "macos")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use art3m1s_core::host::{
        frame::FrameLease,
        protocol::{Domain, HostReply, MessageBody, MessageKind, RequestIdentity, TransportLimits},
        renderer::SpriteRenderer,
        runtime,
        script_io::{HostFrame, IO_READ, IO_STAT, ScriptIoProgress},
    };
    use art3m1s_render::backend::{FrameTarget, GpuBackend, metal::MetalBackend};
    use art3m1s_render::types::Extent2D;
    use serde_json::{Value, json};

    let boot = br#"*top
[lyprop id=group intermediate_render=1 alpha=128]
[calllua function=create]
[flip]
[lydel id=group]
[lyc id=blue width=4 height=4 color=ff0000ff]
[trans type=1 time=100 input=0]
[stop]
[lua]
function create(e)
    local layer = e:createEmoteLayer{id='group.red', files={'red.psb'}, width=4, height=4}
    layer:setCoord(-2, -2, 0, 0)
end
[/lua]
"#;
    let model = include_bytes!("../../src/host/script_io/fixtures/emote-red.psb");
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
    for turn in 0..32 {
        match step(1, 0)? {
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
                assert_eq!(
                    u16::from_le_bytes([remaining[6], remaining[7]]),
                    Domain::Io as u16
                );
                let service = MessageKind {
                    domain: Domain::Io,
                    kind: u32_at(8),
                };
                let args: Value = serde_json::from_slice(&remaining[64..64 + u32_at(56) as usize])?;
                let bytes = match args["key"].as_str() {
                    Some("boot.iet") => Some(boot.as_slice()),
                    Some("red.psb") => Some(model.as_slice()),
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

    let mut renderer = SpriteRenderer::new(MetalBackend::new(4, 4)?);
    let mut check = |frame: &FrameLease, expected: [u8; 4]| -> Result<(), String> {
        let target = FrameTarget::Main;
        renderer.backend_mut().begin_access();
        renderer.backend_mut().begin_frame(target)?;
        renderer.backend_mut().clear([0.0, 0.0, 0.0, 1.0]);
        let result = renderer.render(frame);
        renderer.backend_mut().end_frame();
        result?;
        let pixels = renderer
            .backend_mut()
            .readback_owned(target, Extent2D::new(4, 4))?;
        renderer.backend_mut().end_access();
        assert!(
            pixels
                .chunks_exact(4)
                .all(|pixel| pixel.iter().zip(expected).all(|(&a, b)| a.abs_diff(b) <= 1)),
            "expected {expected:?}, got {:?}",
            &pixels[..4]
        );
        Ok(())
    };

    let old = runtime::acquire_latest(handle, 0)?.expect("group frame");
    check(&old, [128, 0, 0, 255])?;
    drop(old);
    for (id, ms, expected) in [
        (2, 10, [128, 0, 0, 255]),
        (3, 60, [64, 0, 128, 255]),
        (4, 110, [0, 0, 255, 255]),
    ] {
        assert_eq!(step(id, ms)?, ScriptIoProgress::Event);
        let frame = runtime::acquire_latest(handle, 0)?.expect("transition frame");
        check(&frame, expected)?;
    }
    renderer.backend_mut().begin_access();
    renderer.clear_cached_textures();
    renderer.backend_mut().end_access();
    runtime::destroy(handle)?;
    println!("Artemis v2 Metal pixels passed (E-Mote, group and transition)");
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("This probe requires macOS and a Metal device");
    std::process::exit(1);
}
