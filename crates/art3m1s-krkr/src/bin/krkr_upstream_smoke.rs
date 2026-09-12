use std::collections::HashMap;
use std::env;
use std::ffi::{CString, OsString};
use std::fs;
use std::io;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use art3m1s_krkr::native::load_api_v1;
use art3m1s_krkr::protocol::{
    ART3M1S_KRKR_AUDIO_CREATE_STREAM, ART3M1S_KRKR_AUDIO_DESTROY_STREAM,
    ART3M1S_KRKR_AUDIO_FORMAT_F32, ART3M1S_KRKR_AUDIO_FORMAT_I8, ART3M1S_KRKR_AUDIO_FORMAT_I16,
    ART3M1S_KRKR_AUDIO_FORMAT_I24, ART3M1S_KRKR_AUDIO_FORMAT_I32, ART3M1S_KRKR_AUDIO_PAUSE,
    ART3M1S_KRKR_AUDIO_PLAY, ART3M1S_KRKR_AUDIO_SET_PARAMS, ART3M1S_KRKR_AUDIO_STOP,
    ART3M1S_KRKR_AUDIO_SUBMIT_PCM, ART3M1S_KRKR_INPUT_PHASE_DOWN, ART3M1S_KRKR_INPUT_PHASE_UP,
    ART3M1S_KRKR_INPUT_POINTER_BUTTON, ART3M1S_KRKR_INPUT_POINTER_MOVE, ART3M1S_KRKR_POINTER_LEFT,
    ART3M1S_KRKR_STATUS_NO_COMMAND, ART3M1S_KRKR_STATUS_NO_FRAME, ART3M1S_KRKR_STATUS_OK,
    Art3m1sKrkrAudioCommandV1, Art3m1sKrkrAudioConsumedV1, Art3m1sKrkrFrameV1,
    Art3m1sKrkrInputEventV1, Art3m1sKrkrRuntimeConfigV1,
};

fn main() {
    if let Err(error) = run() {
        eprintln!("krkr_upstream_smoke: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let options = parse_args()?;

    let probe = art3m1s_krkr::probe_project(&options.game_root)?;
    if !probe.is_krkr() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "directory does not contain recognized KRKR entry points",
        )
        .into());
    }

    let api = load_api_v1()?;
    let game_root = CString::new(options.game_root.to_string_lossy().as_bytes())?;
    eprintln!("runtime_create: begin");
    let mut runtime = 0u64;
    let status = unsafe {
        api.runtime_create.expect("runtime_create is required")(
            game_root.as_ptr(),
            std::ptr::null(),
            &Art3m1sKrkrRuntimeConfigV1::new(1280, 720),
            &mut runtime,
        )
    };
    if status != ART3M1S_KRKR_STATUS_OK {
        return Err(io::Error::other(format!("runtime_create failed with status {status}")).into());
    }
    eprintln!("runtime_create: ok");

    let result = (|| {
        let mut audio = AudioPump::default();
        let mut last_frame = None;
        for frame_index in 0..options.frame_count {
            if frame_index == 0 {
                eprintln!("runtime_tick: first");
            }

            if let Some(click) = options.click {
                if frame_index == click.frame {
                    push_click(api, runtime, click, ART3M1S_KRKR_INPUT_PHASE_DOWN)?;
                } else if frame_index == click.frame + 1 {
                    push_click(api, runtime, click, ART3M1S_KRKR_INPUT_PHASE_UP)?;
                }
            }

            let status = unsafe { api.runtime_tick.expect("runtime_tick is required")(runtime) };
            if status != ART3M1S_KRKR_STATUS_OK {
                return Err(
                    io::Error::other(format!("runtime_tick failed with status {status}")).into(),
                );
            }
            audio.pump(api, runtime)?;

            let mut frame = Art3m1sKrkrFrameV1::default();
            let status = unsafe {
                api.runtime_acquire_frame
                    .expect("runtime_acquire_frame is required")(runtime, &mut frame)
            };
            if status == ART3M1S_KRKR_STATUS_OK {
                if last_frame.is_none() {
                    eprintln!(
                        "runtime_acquire_frame: first frame {}x{}",
                        frame.width, frame.height
                    );
                }
                last_frame = Some(copy_frame(&frame)?);
                unsafe {
                    let _ = api
                        .runtime_release_frame
                        .expect("runtime_release_frame is required")(
                        runtime, frame.frame_id
                    );
                }
            } else if status != ART3M1S_KRKR_STATUS_NO_FRAME {
                return Err(io::Error::other(format!(
                    "runtime_acquire_frame failed with status {status}"
                ))
                .into());
            }

            thread::sleep(Duration::from_millis(16));
        }

        let frame = last_frame
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "no frame was produced"))?;
        if let Some(output) = options.output.as_ref() {
            write_ppm(output, &frame)?;
            println!(
                "wrote {} ({}x{}, generation {})",
                output.display(),
                frame.width,
                frame.height,
                frame.generation
            );
        } else {
            println!(
                "captured {}x{} frame generation {}, no output requested",
                frame.width, frame.height, frame.generation
            );
        }
        println!(
            "audio: {} stream(s), {} PCM chunk(s), {} byte(s), non-zero PCM={}",
            audio.created_streams, audio.pcm_chunks, audio.pcm_bytes, audio.nonzero_pcm
        );
        Ok(())
    })();

    unsafe {
        api.runtime_destroy.expect("runtime_destroy is required")(runtime);
    }
    result
}

struct SmokeOptions {
    game_root: PathBuf,
    frame_count: u32,
    output: Option<PathBuf>,
    click: Option<Click>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Click {
    frame: u32,
    x: i32,
    y: i32,
}

fn parse_args() -> Result<SmokeOptions, Box<dyn std::error::Error>> {
    let mut positional = Vec::<OsString>::new();
    let mut click = None;
    let mut args = env::args_os().skip(1);

    while let Some(arg) = args.next() {
        if arg == "--click" {
            let value = args.next().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "--click requires frame:x:y")
            })?;
            click = Some(parse_click(&value)?);
        } else {
            positional.push(arg);
        }
    }

    let game_root = positional
        .first()
        .cloned()
        .map(PathBuf::from)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "usage: krkr_upstream_smoke <game-root> [frames] [output.ppm] [--click frame:x:y]",
            )
        })?;
    let frame_count = positional
        .get(1)
        .map(|value| value.to_string_lossy().parse::<u32>())
        .transpose()?
        .unwrap_or(120);
    let output = positional.get(2).cloned().map(PathBuf::from);

    if let Some(click) = click
        && click.frame + 1 >= frame_count
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "click frame must leave at least one frame for pointer-up",
        )
        .into());
    }

    Ok(SmokeOptions {
        game_root,
        frame_count,
        output,
        click,
    })
}

fn parse_click(value: &OsString) -> Result<Click, Box<dyn std::error::Error>> {
    let value = value.to_string_lossy();
    let mut parts = value.split(':');
    let frame = parts.next().and_then(|part| part.parse::<u32>().ok());
    let x = parts.next().and_then(|part| part.parse::<i32>().ok());
    let y = parts.next().and_then(|part| part.parse::<i32>().ok());
    if parts.next().is_some() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid --click value").into());
    }
    match (frame, x, y) {
        (Some(frame), Some(x), Some(y)) => Ok(Click { frame, x, y }),
        _ => Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid --click value").into()),
    }
}

fn push_click(
    api: &art3m1s_krkr::abi::Art3M1sKrkrApiV1,
    runtime: u64,
    click: Click,
    phase: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut move_event = Art3m1sKrkrInputEventV1::new(ART3M1S_KRKR_INPUT_POINTER_MOVE);
    move_event.x = click.x;
    move_event.y = click.y;

    let mut button_event = Art3m1sKrkrInputEventV1::new(ART3M1S_KRKR_INPUT_POINTER_BUTTON);
    button_event.code = ART3M1S_KRKR_POINTER_LEFT;
    button_event.phase = phase;
    button_event.x = click.x;
    button_event.y = click.y;

    let events = [move_event, button_event];
    let status = unsafe {
        api.runtime_push_input
            .expect("runtime_push_input is required")(runtime, events.as_ptr(), events.len())
    };
    if status != ART3M1S_KRKR_STATUS_OK {
        return Err(
            io::Error::other(format!("runtime_push_input failed with status {status}")).into(),
        );
    }
    Ok(())
}

struct OwnedFrame {
    width: u32,
    height: u32,
    generation: u64,
    rgba: Vec<u8>,
}

#[derive(Default)]
struct AudioPump {
    streams: HashMap<u32, AudioStream>,
    last_advance: Option<Instant>,
    created_streams: u64,
    pcm_chunks: u64,
    pcm_bytes: u64,
    nonzero_pcm: bool,
}

struct AudioStream {
    sample_format: u32,
    sample_rate: u32,
    channels: u32,
    playing: bool,
    consumed_samples: u64,
    fractional_samples: f64,
}

impl AudioStream {
    fn new(command: &Art3m1sKrkrAudioCommandV1) -> Self {
        Self {
            sample_format: command.sample_format,
            sample_rate: command.sample_rate,
            channels: command.channels,
            playing: false,
            consumed_samples: 0,
            fractional_samples: 0.0,
        }
    }

    fn sample_bytes(&self) -> Option<usize> {
        match self.sample_format {
            ART3M1S_KRKR_AUDIO_FORMAT_I8 => Some(1),
            ART3M1S_KRKR_AUDIO_FORMAT_I16 => Some(2),
            ART3M1S_KRKR_AUDIO_FORMAT_I24 => Some(3),
            ART3M1S_KRKR_AUDIO_FORMAT_I32 | ART3M1S_KRKR_AUDIO_FORMAT_F32 => Some(4),
            _ => None,
        }
    }
}

impl AudioPump {
    fn pump(
        &mut self,
        api: &art3m1s_krkr::abi::Art3M1sKrkrApiV1,
        runtime: u64,
    ) -> Result<(), Box<dyn std::error::Error>> {
        loop {
            let mut command = Art3m1sKrkrAudioCommandV1::default();
            let status = unsafe {
                api.runtime_poll_audio_command
                    .expect("runtime_poll_audio_command is required")(
                    runtime, &mut command
                )
            };
            match status {
                ART3M1S_KRKR_STATUS_OK => self.handle_command(command)?,
                ART3M1S_KRKR_STATUS_NO_COMMAND => break,
                _ => {
                    return Err(io::Error::other(format!(
                        "runtime_poll_audio_command failed with status {status}"
                    ))
                    .into());
                }
            }
        }

        let now = Instant::now();
        let elapsed = self
            .last_advance
            .map(|last| now.saturating_duration_since(last))
            .unwrap_or_default();
        self.last_advance = Some(now);

        for (&stream_id, stream) in &mut self.streams {
            if !stream.playing || stream.sample_rate == 0 {
                continue;
            }
            stream.fractional_samples += elapsed.as_secs_f64() * f64::from(stream.sample_rate);
            let samples = stream.fractional_samples.floor() as u64;
            if samples == 0 {
                continue;
            }
            stream.fractional_samples -= samples as f64;
            stream.consumed_samples += samples;

            let consumed = Art3m1sKrkrAudioConsumedV1::new(stream_id, stream.consumed_samples, 0);
            let status = unsafe {
                api.runtime_submit_audio_consumed
                    .expect("runtime_submit_audio_consumed is required")(
                    runtime, &consumed
                )
            };
            if status != ART3M1S_KRKR_STATUS_OK {
                return Err(io::Error::other(format!(
                    "runtime_submit_audio_consumed failed with status {status}"
                ))
                .into());
            }
        }
        Ok(())
    }

    fn handle_command(
        &mut self,
        command: Art3m1sKrkrAudioCommandV1,
    ) -> Result<(), Box<dyn std::error::Error>> {
        match command.kind {
            ART3M1S_KRKR_AUDIO_CREATE_STREAM => {
                if command.sample_rate == 0 || command.channels == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "audio stream has an invalid format",
                    )
                    .into());
                }
                self.streams
                    .insert(command.stream_id, AudioStream::new(&command));
                self.created_streams += 1;
            }
            ART3M1S_KRKR_AUDIO_SUBMIT_PCM => {
                let stream = self.streams.get(&command.stream_id).ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "PCM for an unknown audio stream",
                    )
                })?;
                let frame_bytes = stream
                    .sample_bytes()
                    .and_then(|bytes| bytes.checked_mul(usize::try_from(stream.channels).ok()?))
                    .ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidData, "unsupported PCM format")
                    })?;
                let expected = usize::try_from(command.sample_count)
                    .ok()
                    .and_then(|samples| samples.checked_mul(frame_bytes))
                    .ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidData, "PCM payload is too large")
                    })?;
                if command.payload_size != expected || (expected != 0 && command.payload.is_null())
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "PCM payload does not match its format: format={}, channels={}, \
                             samples={}, expected={}, actual={}",
                            command.sample_format,
                            command.channels,
                            command.sample_count,
                            expected,
                            command.payload_size
                        ),
                    )
                    .into());
                }
                if expected != 0 {
                    let payload = unsafe {
                        std::slice::from_raw_parts(command.payload, command.payload_size)
                    };
                    self.nonzero_pcm |= payload.iter().any(|byte| *byte != 0);
                }
                self.pcm_chunks += 1;
                self.pcm_bytes += command.payload_size as u64;
            }
            ART3M1S_KRKR_AUDIO_PLAY => {
                if let Some(stream) = self.streams.get_mut(&command.stream_id) {
                    stream.playing = true;
                }
            }
            ART3M1S_KRKR_AUDIO_PAUSE => {
                if let Some(stream) = self.streams.get_mut(&command.stream_id) {
                    stream.playing = false;
                }
            }
            ART3M1S_KRKR_AUDIO_STOP => {
                if let Some(stream) = self.streams.get_mut(&command.stream_id) {
                    stream.playing = false;
                    stream.consumed_samples = 0;
                    stream.fractional_samples = 0.0;
                }
            }
            ART3M1S_KRKR_AUDIO_DESTROY_STREAM => {
                self.streams.remove(&command.stream_id);
            }
            ART3M1S_KRKR_AUDIO_SET_PARAMS => {}
            kind => {
                return Err(io::Error::other(format!("unknown audio command kind {kind}")).into());
            }
        }
        Ok(())
    }
}

fn copy_frame(frame: &Art3m1sKrkrFrameV1) -> Result<OwnedFrame, Box<dyn std::error::Error>> {
    if frame.pixels.is_null() || frame.width == 0 || frame.height == 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "empty native frame").into());
    }
    let stride = usize::try_from(frame.stride)?;
    let row_bytes = usize::try_from(frame.width)? * 4;
    let height = usize::try_from(frame.height)?;
    if stride < row_bytes || frame.pixels_len < stride.saturating_mul(height) {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid frame stride").into());
    }

    let source = unsafe { std::slice::from_raw_parts(frame.pixels, frame.pixels_len) };
    let mut rgba = vec![0u8; row_bytes * height];
    for row in 0..height {
        rgba[row * row_bytes..(row + 1) * row_bytes]
            .copy_from_slice(&source[row * stride..row * stride + row_bytes]);
    }

    Ok(OwnedFrame {
        width: frame.width,
        height: frame.height,
        generation: frame.generation,
        rgba,
    })
}

fn write_ppm(path: &PathBuf, frame: &OwnedFrame) -> io::Result<()> {
    let mut ppm = format!("P6\n{} {}\n255\n", frame.width, frame.height).into_bytes();
    ppm.reserve((frame.rgba.len() / 4) * 3);
    for pixel in frame.rgba.chunks_exact(4) {
        ppm.extend_from_slice(&pixel[..3]);
    }
    fs::write(path, ppm)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_click_option() {
        assert_eq!(
            parse_click(&OsString::from("700:580:930")).unwrap(),
            Click {
                frame: 700,
                x: 580,
                y: 930,
            }
        );
    }

    #[test]
    fn rejects_malformed_click_options() {
        for value in ["", "700", "700:580", "700:580:930:1", "x:580:930"] {
            assert!(parse_click(&OsString::from(value)).is_err(), "{value}");
        }
    }

    #[test]
    fn audio_pump_validates_i16_stereo_pcm() {
        let mut audio = AudioPump::default();
        let create = Art3m1sKrkrAudioCommandV1 {
            kind: ART3M1S_KRKR_AUDIO_CREATE_STREAM,
            stream_id: 7,
            sample_format: ART3M1S_KRKR_AUDIO_FORMAT_I16,
            sample_rate: 48_000,
            channels: 2,
            ..Default::default()
        };
        audio.handle_command(create).unwrap();

        let payload = [1u8, 0, 0, 0, 2, 0, 0, 0];
        let mut pcm = Art3m1sKrkrAudioCommandV1 {
            kind: ART3M1S_KRKR_AUDIO_SUBMIT_PCM,
            stream_id: 7,
            sample_count: 2,
            payload: payload.as_ptr(),
            payload_size: payload.len(),
            ..Default::default()
        };
        audio.handle_command(pcm).unwrap();
        assert_eq!(audio.pcm_chunks, 1);
        assert_eq!(audio.pcm_bytes, payload.len() as u64);
        assert!(audio.nonzero_pcm);

        pcm.payload_size -= 1;
        assert!(audio.handle_command(pcm).is_err());
    }

    #[test]
    fn audio_pump_rejects_pcm_for_unknown_stream() {
        let mut audio = AudioPump::default();
        let payload = [0u8; 4];
        let pcm = Art3m1sKrkrAudioCommandV1 {
            kind: ART3M1S_KRKR_AUDIO_SUBMIT_PCM,
            stream_id: 99,
            sample_count: 1,
            payload: payload.as_ptr(),
            payload_size: payload.len(),
            ..Default::default()
        };
        assert!(audio.handle_command(pcm).is_err());
    }
}
