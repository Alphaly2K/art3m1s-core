use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use art3m1s_render::backend::metal::MetalBackend;
use art3m1s_rfvp::{RfvpHostRuntime, RfvpNls, RfvpPointerButton};

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
    let clicks = std::env::var("RFVP_SMOKE_CLICK")
        .ok()
        .map(|value| {
            value
                .split(';')
                .map(|click| {
                    let mut parts = click.split(',');
                    let frame = parts
                        .next()
                        .context("RFVP_SMOKE_CLICK needs frame")?
                        .parse::<u32>()
                        .context("RFVP_SMOKE_CLICK frame must be an integer")?;
                    let x = parts
                        .next()
                        .context("RFVP_SMOKE_CLICK needs x")?
                        .parse::<i32>()
                        .context("RFVP_SMOKE_CLICK x must be an integer")?;
                    let y = parts
                        .next()
                        .context("RFVP_SMOKE_CLICK needs y")?
                        .parse::<i32>()
                        .context("RFVP_SMOKE_CLICK y must be an integer")?;
                    if parts.next().is_some() {
                        bail!("RFVP_SMOKE_CLICK entry has trailing fields: {click}");
                    }
                    Ok::<_, anyhow::Error>((frame, x, y))
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    let dump_hit_proxies = std::env::var_os("RFVP_SMOKE_HIT_PROXIES").is_some();
    let profiler_enabled = std::env::var_os("RFVP_SMOKE_PROFILER").is_some();
    // Enables online translation and answers every request with a fixed
    // marker string so the async re-rasterization path is exercised.
    let translate_enabled = std::env::var_os("RFVP_SMOKE_TRANSLATE").is_some();
    let rclicks = std::env::var("RFVP_SMOKE_RCLICK")
        .ok()
        .map(|value| {
            value
                .split(';')
                .map(|click| {
                    let mut parts = click.split(',');
                    let frame = parts
                        .next()
                        .context("RFVP_SMOKE_RCLICK needs frame")?
                        .parse::<u32>()
                        .context("RFVP_SMOKE_RCLICK frame must be an integer")?;
                    let x = parts
                        .next()
                        .context("RFVP_SMOKE_RCLICK needs x")?
                        .parse::<i32>()
                        .context("RFVP_SMOKE_RCLICK x must be an integer")?;
                    let y = parts
                        .next()
                        .context("RFVP_SMOKE_RCLICK needs y")?
                        .parse::<i32>()
                        .context("RFVP_SMOKE_RCLICK y must be an integer")?;
                    if parts.next().is_some() {
                        bail!("RFVP_SMOKE_RCLICK entry has trailing fields: {click}");
                    }
                    Ok::<_, anyhow::Error>((frame, x, y))
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    let keys = std::env::var("RFVP_SMOKE_KEY")
        .ok()
        .map(|value| {
            value
                .split(';')
                .map(|key| {
                    let mut parts = key.split(',');
                    let frame = parts
                        .next()
                        .context("RFVP_SMOKE_KEY needs frame")?
                        .parse::<u32>()
                        .context("RFVP_SMOKE_KEY frame must be an integer")?;
                    let code = parts
                        .next()
                        .context("RFVP_SMOKE_KEY needs code")?
                        .parse::<u32>()
                        .context("RFVP_SMOKE_KEY code must be an integer")?;
                    if parts.next().is_some() {
                        bail!("RFVP_SMOKE_KEY entry has trailing fields: {key}");
                    }
                    Ok::<_, anyhow::Error>((frame, code))
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();

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
    let stage_size = (runtime.width(), runtime.height());
    if profiler_enabled {
        runtime.set_profiler_enabled(true);
    }
    if std::env::var_os("RFVP_SMOKE_DAMAGE").is_some() {
        runtime.set_damage_visualization(true);
    }
    if translate_enabled {
        runtime.set_text_translation_enabled(true)?;
    }

    let mut rendered_frames = 0u32;
    for frame in 0..frame_count {
        if frame == 1 {
            runtime
                .push_input(&[art3m1s_rfvp::RfvpHostInputEvent::Focus { focused: true }])
                .context("focus smoke runtime")?;
        }
        runtime
            .step(16)
            .with_context(|| format!("step frame {frame}"))?;
        if translate_enabled {
            for event in runtime
                .poll_events()
                .with_context(|| format!("poll events frame {frame}"))?
            {
                match event {
                    art3m1s_rfvp::RfvpHostEvent::TextTranslation {
                        serial,
                        slot,
                        source,
                        ruby,
                        ..
                    } => {
                        println!(
                            "translate frame={frame} serial={serial} slot={slot} ruby={ruby:?} source={source:?}",
                        );
                        runtime
                            .submit_text_translation(serial, Some("【翻译测试】"))
                            .with_context(|| format!("submit translation frame {frame}"))?;
                    }
                }
            }
        }
        for (click_frame, x, y) in &clicks {
            if frame == *click_frame {
                runtime
                    .push_input(&[
                        art3m1s_rfvp::RfvpHostInputEvent::PointerMove { x: *x, y: *y },
                        art3m1s_rfvp::RfvpHostInputEvent::PointerButton {
                            button: RfvpPointerButton::Left,
                            pressed: true,
                            x: *x,
                            y: *y,
                        },
                    ])
                    .with_context(|| format!("click down frame {frame} at {x},{y}"))?;
            }
            if frame == click_frame.saturating_add(2) {
                runtime
                    .push_input(&[art3m1s_rfvp::RfvpHostInputEvent::PointerButton {
                        button: RfvpPointerButton::Left,
                        pressed: false,
                        x: *x,
                        y: *y,
                    }])
                    .with_context(|| format!("click up frame {frame} at {x},{y}"))?;
            }
        }
        for (click_frame, x, y) in &rclicks {
            if frame == *click_frame {
                runtime
                    .push_input(&[
                        art3m1s_rfvp::RfvpHostInputEvent::PointerMove { x: *x, y: *y },
                        art3m1s_rfvp::RfvpHostInputEvent::PointerButton {
                            button: RfvpPointerButton::Right,
                            pressed: true,
                            x: *x,
                            y: *y,
                        },
                    ])
                    .with_context(|| format!("right-click down frame {frame} at {x},{y}"))?;
            }
            if frame == click_frame.saturating_add(2) {
                runtime
                    .push_input(&[art3m1s_rfvp::RfvpHostInputEvent::PointerButton {
                        button: RfvpPointerButton::Right,
                        pressed: false,
                        x: *x,
                        y: *y,
                    }])
                    .with_context(|| format!("right-click up frame {frame} at {x},{y}"))?;
            }
        }
        for (key_frame, code) in &keys {
            if frame == *key_frame {
                runtime
                    .push_input(&[art3m1s_rfvp::RfvpHostInputEvent::Key {
                        code: *code,
                        pressed: true,
                        repeat: false,
                        modifiers: 0,
                    }])
                    .with_context(|| format!("key down frame {frame} code {code}"))?;
            }
            if frame == key_frame.saturating_add(2) {
                runtime
                    .push_input(&[art3m1s_rfvp::RfvpHostInputEvent::Key {
                        code: *code,
                        pressed: false,
                        repeat: false,
                        modifiers: 0,
                    }])
                    .with_context(|| format!("key up frame {frame} code {code}"))?;
            }
        }
        if let Some(result) = runtime
            .render_pending_frame()
            .with_context(|| format!("render frame {frame}"))?
        {
            rendered_frames += 1;
            if dump_hit_proxies {
                for proxy in &result.hit_proxies.proxies {
                    println!(
                        "hit frame={frame} prim={} rect=({}, {}, {}, {}) enabled={} visible={} order={}",
                        proxy.prim_id.0,
                        proxy.rect.x,
                        proxy.rect.y,
                        proxy.rect.w,
                        proxy.rect.h,
                        proxy.enabled,
                        proxy.visible,
                        proxy.order,
                    );
                }
            }
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
    image::save_buffer(
        &output,
        &pixels,
        stage_size.0,
        stage_size.1,
        image::ColorType::Rgba8,
    )
    .with_context(|| format!("save screenshot {}", output.display()))?;
    if profiler_enabled {
        println!("profiler={}", runtime.profiler_snapshot_json());
    }
    println!(
        "rendered_frames={rendered_frames} non_black={non_black} stage={}x{} saved={}",
        stage_size.0,
        stage_size.1,
        output.display(),
    );
    Ok(())
}
