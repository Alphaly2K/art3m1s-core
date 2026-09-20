//! Dumps raw RFVP ABI frames (draw + texture commands) for diagnosis.
//!
//! Drives the v1 host ABI directly so upstream state can be inspected before
//! any adapter conversion runs. Clicks use the same `RFVP_SMOKE_CLICK`
//! "frame,x,y;..." syntax as `rfvp_host_runtime_smoke`. Set
//! `RFVP_DUMP_FRAMES` to a comma-separated list (or `all`) to print full
//! per-command details for those frames; every acquired frame prints a one
//! line summary regardless.

use std::path::PathBuf;
use std::ptr;

use anyhow::{Context, Result, bail};
use rfvp::host_abi::runtime::{
    rfvp_frame_get_commands, rfvp_frame_get_textures, rfvp_frame_release, rfvp_resources_create,
    rfvp_resources_destroy, rfvp_resources_mount_directory, rfvp_runtime_acquire_frame,
    rfvp_runtime_create, rfvp_runtime_destroy, rfvp_runtime_push_input, rfvp_runtime_step,
};
use rfvp::host_abi::v1::{
    RFVP_INPUT_FOCUS, RFVP_INPUT_PHASE_DOWN, RFVP_INPUT_PHASE_MOVE, RFVP_INPUT_PHASE_UP,
    RFVP_INPUT_POINTER_BUTTON, RFVP_INPUT_POINTER_MOVE, RFVP_NLS_SHIFT_JIS, RFVP_POINTER_LEFT,
    RFVP_STATUS_NO_FRAME, RFVP_STATUS_OK, RfvpInputEventV1, RfvpResourcesConfigV1,
    RfvpRuntimeConfigV1,
};

fn input(kind: u32, phase: u32, x: i32, y: i32) -> RfvpInputEventV1 {
    RfvpInputEventV1 {
        struct_size: std::mem::size_of::<RfvpInputEventV1>() as u32,
        kind,
        code: RFVP_POINTER_LEFT,
        phase,
        x,
        y,
        value: 0,
        modifiers: 0,
        id: 0,
    }
}

fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let game_root = args
        .next()
        .map(PathBuf::from)
        .context("usage: rfvp_frame_dump <game_root> [frames]")?;
    let frame_count = args
        .next()
        .map(|value| {
            value
                .to_str()
                .context("frame count is not valid UTF-8")?
                .parse::<u32>()
                .context("frame count must be a positive integer")
        })
        .transpose()?
        .unwrap_or(600);
    let clicks = std::env::var("RFVP_SMOKE_CLICK")
        .ok()
        .map(|value| {
            value
                .split(';')
                .map(|click| {
                    let mut parts = click.split(',');
                    let frame = parts.next().context("click needs frame")?.parse::<u32>()?;
                    let x = parts.next().context("click needs x")?.parse::<i32>()?;
                    let y = parts.next().context("click needs y")?.parse::<i32>()?;
                    Ok::<_, anyhow::Error>((frame, x, y))
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    let dump_all = std::env::var("RFVP_DUMP_FRAMES").ok().as_deref() == Some("all");
    let dump_texture: Option<u32> = std::env::var("RFVP_DUMP_TEXTURE")
        .ok()
        .map(|value| value.parse::<u32>().context("RFVP_DUMP_TEXTURE id"))
        .transpose()?;
    // Tracks (nonzero_alpha, frame, width, height, pixels) with the most ink.
    let mut saved_texture: Option<(usize, u32, u32, u32, Vec<u8>)> = None;
    let dump_frames: Vec<u32> = if dump_all {
        Vec::new()
    } else {
        std::env::var("RFVP_DUMP_FRAMES")
            .unwrap_or_default()
            .split(',')
            .filter(|entry| !entry.is_empty())
            .map(|entry| entry.parse::<u32>().context("RFVP_DUMP_FRAMES entry"))
            .collect::<Result<Vec<_>, _>>()?
    };

    let game_root = game_root.to_str().context("game root is not UTF-8")?;

    let resources_config = RfvpResourcesConfigV1 {
        struct_size: std::mem::size_of::<RfvpResourcesConfigV1>() as u32,
        flags: 0,
        nls: RFVP_NLS_SHIFT_JIS,
        reserved0: 0,
        save_root_utf8: ptr::null(),
        save_root_len: 0,
        reserved: [0; 4],
    };
    let mut resources = 0u64;
    let status = unsafe { rfvp_resources_create(&resources_config, &mut resources) };
    if status != RFVP_STATUS_OK {
        bail!("resources_create status={status}");
    }
    let status =
        unsafe { rfvp_resources_mount_directory(resources, game_root.as_ptr(), game_root.len()) };
    if status != RFVP_STATUS_OK {
        bail!("mount_directory status={status}");
    }
    let runtime_config = RfvpRuntimeConfigV1 {
        struct_size: std::mem::size_of::<RfvpRuntimeConfigV1>() as u32,
        flags: 0,
        resources,
        requested_width: 1024,
        requested_height: 640,
        reserved: [0; 4],
    };
    let mut runtime = 0u64;
    let status = unsafe { rfvp_runtime_create(&runtime_config, &mut runtime) };
    if status != RFVP_STATUS_OK {
        bail!("runtime_create status={status}");
    }

    for frame in 0..frame_count {
        if frame == 1 {
            let focus = input(RFVP_INPUT_FOCUS, 1, 0, 0);
            unsafe { rfvp_runtime_push_input(runtime, &focus, 1) };
        }
        for (click_frame, x, y) in &clicks {
            if frame == *click_frame {
                let events = [
                    input(RFVP_INPUT_POINTER_MOVE, RFVP_INPUT_PHASE_MOVE, *x, *y),
                    input(RFVP_INPUT_POINTER_BUTTON, RFVP_INPUT_PHASE_DOWN, *x, *y),
                ];
                unsafe { rfvp_runtime_push_input(runtime, events.as_ptr(), events.len()) };
            }
            if frame == click_frame.saturating_add(2) {
                let events = [input(
                    RFVP_INPUT_POINTER_BUTTON,
                    RFVP_INPUT_PHASE_UP,
                    *x,
                    *y,
                )];
                unsafe { rfvp_runtime_push_input(runtime, events.as_ptr(), events.len()) };
            }
        }
        let status = unsafe { rfvp_runtime_step(runtime, 16) };
        if status != RFVP_STATUS_OK {
            bail!("step frame={frame} status={status}");
        }
        let mut native_frame = 0u64;
        let status = unsafe { rfvp_runtime_acquire_frame(runtime, &mut native_frame) };
        if status == RFVP_STATUS_NO_FRAME {
            continue;
        }
        if status != RFVP_STATUS_OK {
            bail!("acquire frame={frame} status={status}");
        }

        let mut commands_ptr = ptr::null();
        let mut command_count = 0usize;
        unsafe { rfvp_frame_get_commands(native_frame, &mut commands_ptr, &mut command_count) };
        let mut textures_ptr = ptr::null();
        let mut texture_count = 0usize;
        unsafe { rfvp_frame_get_textures(native_frame, &mut textures_ptr, &mut texture_count) };
        let commands = unsafe { std::slice::from_raw_parts(commands_ptr, command_count) };
        let textures = unsafe { std::slice::from_raw_parts(textures_ptr, texture_count) };

        println!(
            "frame={frame} draws={} textures={}",
            commands.len(),
            textures.len()
        );
        if let Some(want_id) = dump_texture {
            for texture in textures {
                if texture.texture_id == want_id
                    && texture.kind == 1
                    && !texture.pixels.is_null()
                    && texture.format == 1
                {
                    let pixels = unsafe {
                        std::slice::from_raw_parts(texture.pixels, texture.pixels_size).to_vec()
                    };
                    let nonzero_a = pixels.chunks_exact(4).filter(|pixel| pixel[3] != 0).count();
                    if saved_texture
                        .as_ref()
                        .is_none_or(|(best, ..)| nonzero_a > *best)
                    {
                        saved_texture =
                            Some((nonzero_a, frame, texture.width, texture.height, pixels));
                    }
                }
            }
        }
        if dump_all || dump_frames.contains(&frame) {
            for (index, command) in commands.iter().enumerate() {
                println!(
                    "  draw[{index}] kind={} flags={:#x} tex={} blend={} filter={} effect={} src=({},{} {}x{}) dst=({},{} {}x{}) clip=({},{} {}x{}) color=({:.3},{:.3},{:.3},{:.3})",
                    command.kind,
                    command.flags,
                    command.texture_id,
                    command.blend,
                    command.filter,
                    command.effect_id,
                    command.src_rect.x,
                    command.src_rect.y,
                    command.src_rect.width,
                    command.src_rect.height,
                    command.dst_rect.x,
                    command.dst_rect.y,
                    command.dst_rect.width,
                    command.dst_rect.height,
                    command.clip_rect.x,
                    command.clip_rect.y,
                    command.clip_rect.width,
                    command.clip_rect.height,
                    command.color.r,
                    command.color.g,
                    command.color.b,
                    command.color.a,
                );
                for vertex in &command.vertices {
                    println!(
                        "    v=({:.2},{:.2} uv {:.5},{:.5} rgba {:.3},{:.3},{:.3},{:.3})",
                        vertex.x,
                        vertex.y,
                        vertex.u,
                        vertex.v,
                        vertex.color.r,
                        vertex.color.g,
                        vertex.color.b,
                        vertex.color.a,
                    );
                }
            }
            for (index, texture) in textures.iter().enumerate() {
                let (mut nonzero_rgb, mut nonzero_a) = (0usize, 0usize);
                if !texture.pixels.is_null() {
                    let pixels =
                        unsafe { std::slice::from_raw_parts(texture.pixels, texture.pixels_size) };
                    if texture.format == 1 {
                        for pixel in pixels.chunks_exact(4) {
                            nonzero_rgb +=
                                usize::from(pixel[0] != 0 || pixel[1] != 0 || pixel[2] != 0);
                            nonzero_a += usize::from(pixel[3] != 0);
                        }
                    } else if texture.format == 2 {
                        for pixel in pixels.chunks_exact(2) {
                            nonzero_rgb += usize::from(pixel[0] != 0);
                            nonzero_a += usize::from(pixel[1] != 0);
                        }
                    }
                }
                println!(
                    "  tex[{index}] kind={} id={} fmt={} size={}x{} rect=({},{} {}x{}) bytes={} nzrgb={} nza={} gen={}",
                    texture.kind,
                    texture.texture_id,
                    texture.format,
                    texture.width,
                    texture.height,
                    texture.rect.x,
                    texture.rect.y,
                    texture.rect.width,
                    texture.rect.height,
                    texture.pixels_size,
                    nonzero_rgb,
                    nonzero_a,
                    texture.generation,
                );
            }
        }
        unsafe { rfvp_frame_release(native_frame) };
    }

    unsafe {
        rfvp_runtime_destroy(runtime);
        rfvp_resources_destroy(resources);
    }
    if let Some((nonzero_a, frame, width, height, pixels)) = saved_texture {
        let path = format!("/tmp/rfvp_dump_texture_{}.png", dump_texture.unwrap_or(0));
        image::save_buffer(&path, &pixels, width, height, image::ColorType::Rgba8)
            .with_context(|| format!("save texture dump {path}"))?;
        println!(
            "saved texture dump {path} ({width}x{height}) max_nza={nonzero_a} at frame={frame}"
        );
    }
    Ok(())
}
