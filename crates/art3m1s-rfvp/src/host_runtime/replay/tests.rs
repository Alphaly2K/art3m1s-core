use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use rfvp::host_api::{DrawGlyphCmd, PortableTextureDesc, RectI16, RectU16, Rgba8, TextureRect};
use rfvp::rendering::external::RecordedTextureDestroy;
use rfvp::script::opcode::Opcode;

use super::*;

#[test]
fn directory_session_replays_after_skipped_frame_and_shutdown() {
    struct Project(PathBuf);
    impl Drop for Project {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let path = std::env::temp_dir().join(format!(
        "rfvp-replay-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    std::fs::create_dir(&path).unwrap();
    let project = Project(path);
    std::fs::write(project.0.join("tile.hcb"), tile_script()).unwrap();

    let mut runtime = RfvpFrameRuntime::new_directory(&project.0, None, RfvpNls::Utf8).unwrap();
    assert!(runtime.acquire_frame().is_none());
    runtime.step(16).unwrap();
    // Skip presenting (and acquiring) the frame that creates the white texture.
    runtime.step(16).unwrap();
    let frame = runtime.acquire_frame().unwrap();
    assert_eq!((frame.width, frame.height), (640, 480));
    runtime.step(16).unwrap();
    let next = runtime.acquire_frame().unwrap();
    assert!(!Arc::ptr_eq(&frame, &next));
    drop(runtime);

    std::thread::spawn(move || {
        let [RenderCommand::DrawImage(tile)] = frame.frame.commands.as_slice() else {
            panic!("expected one tile, found {:?}", frame.frame.commands);
        };
        assert_eq!(
            tile.dst,
            RectI16 {
                x: 8,
                y: 12,
                w: 16,
                h: 20
            }
        );
        let [texture] = frame.textures.as_slice() else {
            panic!("missing texture for tile");
        };
        assert_eq!(texture.handle, tile.texture);
        assert_eq!((texture.desc.width, texture.desc.height), (1, 1));
        let color = [tile.color.r, tile.color.g, tile.color.b, tile.color.a];
        let sampled: Vec<u8> = texture
            .pixels
            .iter()
            .zip(color)
            .map(|(&texel, tint)| (u16::from(texel) * u16::from(tint) / 255) as u8)
            .collect();
        assert_eq!(sampled, [255; 4]);
        assert!(Arc::ptr_eq(texture, &next.textures[0]));
    })
    .join()
    .unwrap();
}

fn tile_script() -> Vec<u8> {
    let calls: &[(&str, &[i8])] = &[
        ("PrimSetTile", &[1, 2, 8, 12, 16, 20]),
        ("PrimSetDraw", &[1, 1]),
        ("PrimGroupIn", &[1, 0]),
        ("ThreadSleep", &[100]),
    ];
    let mut hcb = vec![0; 4];
    for (index, (_, args)) in calls.iter().enumerate() {
        for &arg in *args {
            hcb.extend([Opcode::PushI8 as u8, arg as u8]);
        }
        hcb.push(Opcode::Syscall as u8);
        hcb.extend((index as u16).to_le_bytes());
    }
    let sleep = hcb.len() - 5;
    hcb.push(Opcode::Jmp as u8);
    hcb.extend((sleep as u32).to_le_bytes());

    let descriptor_offset = (hcb.len() as u32).to_le_bytes();
    hcb[..4].copy_from_slice(&descriptor_offset);
    hcb.extend(4u32.to_le_bytes()); // entry point
    hcb.extend([0; 4]); // global counts
    hcb.extend([0, 0, 1, 0]); // 640×480, reserved byte, empty title
    hcb.extend((calls.len() as u16).to_le_bytes());
    for &(name, args) in calls {
        hcb.extend([args.len() as u8, name.len() as u8 + 1]);
        hcb.extend(name.as_bytes());
        hcb.push(0);
    }
    hcb.extend([0; 2]); // custom syscalls
    hcb
}

#[test]
fn held_atlas_survives_partial_update_destruction_and_handle_reuse() {
    let handle = TextureHandle(4);
    let format = TextureFormat::LumaA8;
    let create = |pixels| {
        RecordedTextureCommand::Create(RecordedTextureCreate {
            handle,
            desc: PortableTextureDesc {
                width: 2,
                height: 2,
                format,
            },
            pixels,
            generation: 1,
        })
    };
    let snapshot = |cache: &mut FrameTextures, texture_commands| {
        cache
            .snapshot(NativeFrame {
                width: 640,
                height: 480,
                frame: RenderFrame {
                    commands: vec![RenderCommand::DrawGlyph(DrawGlyphCmd {
                        texture: handle,
                        src: RectU16::default(),
                        dst: RectI16::default(),
                        color: Rgba8 {
                            r: 255,
                            g: 255,
                            b: 255,
                            a: 255,
                        },
                        clip: None,
                    })],
                    ..RenderFrame::default()
                },
                texture_commands,
            })
            .unwrap()
            .textures
            .pop()
            .unwrap()
    };
    let mut cache = FrameTextures::default();
    let old = snapshot(
        &mut cache,
        vec![create(vec![0, 255, 1, 255, 2, 255, 3, 255])],
    );
    let updated = snapshot(
        &mut cache,
        vec![RecordedTextureCommand::Update(RecordedTextureUpdate {
            handle,
            rect: TextureRect {
                x: 1,
                y: 0,
                width: 1,
                height: 2,
            },
            format,
            pixels: vec![100, 200, 150, 250],
            generation: 2,
        })],
    );
    let recreated = snapshot(
        &mut cache,
        vec![
            RecordedTextureCommand::Destroy(RecordedTextureDestroy { handle }),
            create(vec![50; 8]),
        ],
    );
    assert_eq!(old.pixels, [0, 255, 1, 255, 2, 255, 3, 255]);
    assert_eq!(updated.pixels, [0, 255, 100, 200, 2, 255, 150, 250]);
    assert_eq!(recreated.pixels, [50; 8]);
    assert!(old.generation < updated.generation && updated.generation < recreated.generation);
}
