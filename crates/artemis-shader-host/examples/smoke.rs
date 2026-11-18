use art3m1s_artemis_shader_host::{Draw, Error, Image, Worker};
use std::process::{Command, Stdio};
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let executable = arguments.next().ok_or("expected worker executable")?;
    let mut process = Command::new(executable)
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    let stdin = process.stdin.take().unwrap();
    let stdout = process.stdout.take().unwrap();
    let result = (|| smoke(Worker::connect(stdout, stdin)?))();
    if result.is_err() {
        let _ = process.kill();
    }
    let status = process.wait()?;
    result?;
    assert!(status.success(), "worker did not exit after EOF: {status}");
    println!("D3D11 SDK passed compile, redraw, invalid uniform, and release checks");
    Ok(())
}

fn smoke<R: std::io::Read, W: std::io::Write>(mut worker: Worker<R, W>) -> Result<(), Error> {
    assert!(
        matches!(worker.compile(b"invalid hlsl"), Err(Error::Worker { diagnostic, .. }) if !diagnostic.is_empty())
    );
    let original = worker.compile(include_bytes!(
        "../../../tools/host-smoke/original-shader/probe.hlsl"
    ))?;
    let adjustable = worker.compile(
        b"cbuffer Params : register(b0) { float gain; float4 color; }; \
                                    float4 main() : SV_Target { return gain * color; }",
    )?;
    let mut draw = Draw {
        width: 4,
        height: 4,
        alpha_blend: false,
        point_sampler: false,
        clear_rgba: [0.0; 4],
        quad: [
            [-1.0, 1.0, 0.0, 0.0, 0.0, 0.0],
            [1.0, 1.0, 1.0, 0.0, 1.0, 0.0],
            [-1.0, -1.0, 0.0, 1.0, 0.0, 1.0],
            [1.0, -1.0, 1.0, 1.0, 1.0, 1.0],
        ],
        uniforms: &[],
        textures: [None; 4],
    };
    assert_eq!(worker.draw(original, &draw)?, [128, 64, 32, 255].repeat(16));
    draw.width = 6;
    draw.height = 8;
    draw.uniforms = &[("gain", &[1.0, 1.0])];
    assert!(matches!(
        worker.draw(adjustable, &draw),
        Err(Error::Worker { .. })
    ));
    draw.uniforms = &[("gain", &[1.0]), ("color", &[0.75, 0.25, 0.5, 1.0])];
    assert_eq!(
        worker.draw(adjustable, &draw)?,
        [128, 64, 191, 255].repeat(48)
    );
    worker.release(original)?;
    draw.uniforms = &[];
    assert!(matches!(
        worker.draw(original, &draw),
        Err(Error::Worker { .. })
    ));
    worker.release(adjustable)?;
    if std::env::var_os("ARTEMIS_D3D11_MEASURE").is_some() {
        measure(&mut worker)?;
    }
    Ok(())
}

fn measure<R: std::io::Read, W: std::io::Write>(worker: &mut Worker<R, W>) -> Result<(), Error> {
    const WIDTH: u32 = 1920;
    const HEIGHT: u32 = 1080;
    let program = worker.compile(
        b"Texture2D foreground : register(t0); \
        Texture2D background : register(t1); SamplerState s : register(s0); \
        float4 main(float2 uv : TEXCOORD0) : SV_Target { \
            return lerp(foreground.Sample(s, uv), background.Sample(s, uv), 0.5); }",
    )?;
    let pixel_count = (WIDTH * HEIGHT) as usize;
    let foreground = [128, 64, 32, 255].repeat(pixel_count);
    let background = [64, 32, 16, 255].repeat(pixel_count);
    let image = |bgra| {
        Some(Image {
            width: WIDTH,
            height: HEIGHT,
            bgra,
        })
    };
    let draw = Draw {
        width: WIDTH,
        height: HEIGHT,
        alpha_blend: false,
        point_sampler: true,
        clear_rgba: [0.0; 4],
        quad: [
            [-1.0, 1.0, 0.0, 0.0, 0.0, 0.0],
            [1.0, 1.0, 1.0, 0.0, 1.0, 0.0],
            [-1.0, -1.0, 0.0, 1.0, 0.0, 1.0],
            [1.0, -1.0, 1.0, 1.0, 1.0, 1.0],
        ],
        uniforms: &[],
        textures: [image(&foreground), image(&background), None, None],
    };
    for _ in 0..3 {
        let frame = worker.draw(program, &draw)?;
        assert_eq!(&frame[..4], &[96, 48, 24, 255]);
    }
    let mut millis = [0.0; 12];
    for timing in &mut millis {
        let start = Instant::now();
        let frame = worker.draw(program, &draw)?;
        *timing = start.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(&frame[frame.len() - 4..], &[96, 48, 24, 255]);
    }
    millis.sort_by(f64::total_cmp);
    println!(
        "D3D11 1080p, two BGRA inputs and one output: median {:.1} ms, max {:.1} ms",
        (millis[5] + millis[6]) / 2.0,
        millis[11]
    );
    worker.release(program)
}
