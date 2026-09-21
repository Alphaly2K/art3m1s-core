use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use art3m1s_render::backend::metal::MetalBackend;
use art3m1s_render::{Extent2D, FrameTarget, GpuBackend};
use art3m1s_siglus::{SiglusAdapter, is_siglus_project};
use siglus_scene_vm::host::SiglusHostConfig;

fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let root = PathBuf::from(
        args.next()
            .context("usage: siglus_frame_dump GAME_DIR OUTPUT.png [FRAMES]")?,
    );
    let output = PathBuf::from(args.next().context("missing output PNG path")?);
    let frames = args
        .next()
        .map(|value| value.to_string_lossy().parse::<u32>())
        .transpose()?
        .unwrap_or(120);
    if !is_siglus_project(&root) {
        bail!(
            "{} has no recognizable Siglus Scene.pck/Gameexe.dat",
            root.display()
        );
    }

    let mut adapter = SiglusAdapter::open(SiglusHostConfig::new(root))?;
    let (width, height) = adapter.logical_size();
    let mut gpu = MetalBackend::new(width, height).map_err(anyhow::Error::msg)?;
    for frame in 0..frames {
        let exited = adapter
            .step_without_present(16, &mut gpu)
            .with_context(|| format!("Siglus frame {frame}"))?;
        if frame % 30 == 0 || exited {
            let (sprites, commands) = adapter.last_frame_counts();
            eprintln!(
                "frame={frame} exited={exited} sprites={sprites} commands={commands} {}",
                adapter.host().debug_status_summary()
            );
        }
        if exited {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(16));
    }
    gpu.begin_access();
    let pixels = gpu
        .readback_owned(FrameTarget::Main, Extent2D::new(width, height))
        .map_err(anyhow::Error::msg);
    gpu.end_access();
    let pixels = pixels?;
    let non_black = pixels
        .chunks_exact(4)
        .filter(|pixel| pixel[0] > 8 || pixel[1] > 8 || pixel[2] > 8)
        .count();
    if non_black == 0 {
        bail!("Siglus rendered a blank {}x{} frame", width, height);
    }
    image::save_buffer(&output, &pixels, width, height, image::ColorType::Rgba8)
        .with_context(|| format!("save {}", output.display()))?;
    adapter.release_textures(&mut gpu);
    println!(
        "saved {} ({}x{}, non_black={non_black})",
        output.display(),
        width,
        height
    );
    Ok(())
}
