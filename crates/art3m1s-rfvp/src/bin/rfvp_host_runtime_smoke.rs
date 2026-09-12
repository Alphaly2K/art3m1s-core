use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use art3m1s_render::backend::metal::MetalBackend;
use art3m1s_rfvp::{RfvpHostRuntime, RfvpNls};

fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let game_root = args.next().map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../rfvp/win95_painter_demo")
    });
    let output = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp/rfvp_host_runtime_smoke.png"));
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
        .unwrap_or(180);

    let size = (1024u32, 640u32);
    let backend = MetalBackend::new(size.0, size.1).map_err(anyhow::Error::msg)?;
    let mut runtime = RfvpHostRuntime::new_directory(
        &game_root,
        None,
        size.0,
        size.1,
        RfvpNls::ShiftJis,
        Box::new(backend),
        [0.05, 0.06, 0.08, 1.0],
    )?;

    let mut rendered_frames = 0u32;
    for frame in 0..frame_count {
        runtime
            .step(16)
            .with_context(|| format!("step frame {frame}"))?;
        if runtime
            .render_pending_frame()
            .with_context(|| format!("render frame {frame}"))?
            .is_some()
        {
            rendered_frames += 1;
        }
        if frame % 30 == 0 {
            println!(
                "frame={frame} commands={} textures={}",
                runtime.last_command_count(),
                runtime.last_texture_count(),
            );
        }
    }

    if rendered_frames == 0 {
        bail!("RFVP host runtime produced no frames");
    }
    let pixels = runtime.readback_rgba()?;
    let non_black = pixels
        .chunks_exact(4)
        .filter(|pixel| pixel[0] > 8 || pixel[1] > 8 || pixel[2] > 8)
        .count();
    if non_black == 0 {
        bail!("RFVP host runtime produced only a black frame");
    }
    image::save_buffer(&output, &pixels, size.0, size.1, image::ColorType::Rgba8)
        .with_context(|| format!("save screenshot {}", output.display()))?;
    println!(
        "rendered_frames={rendered_frames} non_black={non_black} saved={}",
        output.display()
    );
    Ok(())
}
