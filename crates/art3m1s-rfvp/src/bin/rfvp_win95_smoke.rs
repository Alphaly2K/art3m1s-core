use std::path::PathBuf;
use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
};

use anyhow::{Context, Result, bail};
use art3m1s_render::Extent2D;
use art3m1s_render::backend::metal::MetalBackend;
use art3m1s_rfvp::ExternalRenderer;
use rfvp::app::App;
use rfvp::script::parser::Nls;
use rfvp::subsystem::anzu_scene::AnzuScene;
use rfvp::subsystem::resources::thread_manager::ThreadManager;

fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let game_root = args.next().map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../rfvp/win95_painter_demo")
    });
    let game_root = game_root
        .to_str()
        .context("game root is not valid UTF-8")?
        .to_owned();
    let output = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp/rfvp_win95_smoke.png"));
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
    let frame_delay_ms = args
        .next()
        .map(|value| {
            value
                .to_str()
                .context("frame delay is not valid UTF-8")?
                .parse::<u64>()
                .context("frame delay must be a non-negative integer")
        })
        .transpose()?
        .unwrap_or(16);

    rfvp::utils::file::set_base_path(&game_root);
    let parser = rfvp::boot::load_script(Nls::ShiftJIS)?;
    let title = parser.get_title();
    let size = parser.get_screen_size();
    let script_engine = ThreadManager::new();

    let mut pump = App::app_with_config(rfvp::boot::app_config(&title, size))
        .with_scene::<AnzuScene>()
        .with_script_engine(script_engine)
        .with_window_title(&title)
        .with_window_size(size)
        .with_parser(parser)
        .with_system_font(true)
        .with_vfs(Nls::ShiftJIS)?
        .build_pump()?;

    let backend = MetalBackend::new(size.0, size.1).map_err(anyhow::Error::msg)?;
    let mut renderer = ExternalRenderer::new(Box::new(backend), [0.05, 0.06, 0.08, 1.0]);
    let mut last_pixels = Vec::new();
    let mut saw_non_black = false;
    let mut last_signature = 0;

    for frame in 0..frame_count {
        let _status = pump.host_step_without_render(frame_delay_ms as u32);
        let external = pump.capture_external_frame();
        let signature = frame_signature(&external);
        let changed = signature != last_signature;
        let mut non_black = 0;

        if changed {
            last_signature = signature;
            let _result = renderer.render_frame(&external)?;
            last_pixels = renderer.readback_rgba(Extent2D::new(size.0, size.1))?;
            non_black = last_pixels
                .chunks_exact(4)
                .filter(|pixel| pixel[0] > 8 || pixel[1] > 8 || pixel[2] > 8)
                .count();
            saw_non_black |= non_black > 0;
        }

        if frame % 30 == 0 {
            println!(
                "frame={frame} commands={} textures={} cached_textures={} changed={changed} non_black={non_black}",
                external.frame.commands.len(),
                external.textures.len(),
                renderer.cached_texture_count(),
            );
        }
    }

    if !saw_non_black {
        bail!("win95_painter produced only a blank Art3m1s frame");
    }
    image::save_buffer(
        &output,
        &last_pixels,
        size.0,
        size.1,
        image::ColorType::Rgba8,
    )
    .with_context(|| format!("save screenshot {}", output.display()))?;
    println!("saved {} ({}x{})", output.display(), size.0, size.1);
    Ok(())
}

fn frame_signature(external: &rfvp::rendering::external::ExternalFrame) -> u64 {
    use rfvp::host_api::{CommandBlendMode, RenderCommand};

    fn hash_f32(hasher: &mut DefaultHasher, value: f32) {
        value.to_bits().hash(hasher);
    }

    fn hash_blend(hasher: &mut DefaultHasher, blend: CommandBlendMode) {
        match blend {
            CommandBlendMode::Normal => 0u8,
            CommandBlendMode::Add => 1,
            CommandBlendMode::Sub => 2,
            CommandBlendMode::Mul => 3,
        }
        .hash(hasher);
    }

    let mut hasher = DefaultHasher::new();
    let frame = &external.frame;
    frame.commands.len().hash(&mut hasher);

    for command in &frame.commands {
        match command {
            RenderCommand::DrawImage(command) => {
                0u8.hash(&mut hasher);
                command.texture.0.hash(&mut hasher);
                [
                    command.color.r,
                    command.color.g,
                    command.color.b,
                    command.color.a,
                ]
                .hash(&mut hasher);
                hash_blend(&mut hasher, command.blend);
                command.effect_id.hash(&mut hasher);
                command.clip.is_some().hash(&mut hasher);
                if let Some(clip) = command.clip {
                    [clip.x, clip.y, clip.w, clip.h].hash(&mut hasher);
                }
                for vertex in &command.vertices {
                    for value in vertex.position {
                        hash_f32(&mut hasher, value);
                    }
                    for value in vertex.tex_coord {
                        hash_f32(&mut hasher, value);
                    }
                    for value in [
                        vertex.color.r,
                        vertex.color.g,
                        vertex.color.b,
                        vertex.color.a,
                    ] {
                        hash_f32(&mut hasher, value);
                    }
                }
            }
            RenderCommand::DrawGlyph(command) => {
                1u8.hash(&mut hasher);
                command.texture.0.hash(&mut hasher);
                [command.src.x, command.src.y, command.src.w, command.src.h].hash(&mut hasher);
                [command.dst.x, command.dst.y, command.dst.w, command.dst.h].hash(&mut hasher);
                [
                    command.color.r,
                    command.color.g,
                    command.color.b,
                    command.color.a,
                ]
                .hash(&mut hasher);
                command.clip.is_some().hash(&mut hasher);
                if let Some(clip) = command.clip {
                    [clip.x, clip.y, clip.w, clip.h].hash(&mut hasher);
                }
            }
            RenderCommand::SetClip(rect) => {
                2u8.hash(&mut hasher);
                [rect.x, rect.y, rect.w, rect.h].hash(&mut hasher);
            }
            RenderCommand::ClearClip => 3u8.hash(&mut hasher),
        }
    }

    for texture in &external.textures {
        texture.handle.0.hash(&mut hasher);
        texture.generation.hash(&mut hasher);
        texture.pixels.len().hash(&mut hasher);
        texture.desc.width.hash(&mut hasher);
        texture.desc.height.hash(&mut hasher);
    }

    hasher.finish()
}
