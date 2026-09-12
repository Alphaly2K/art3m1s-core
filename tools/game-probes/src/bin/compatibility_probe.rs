use art3m1s_core::{
    backend::gl::platform::{AngleBackend, GfxBackend},
    ffi, host_events,
    host_files::HostResources,
    runtime::CoreRuntime,
};
use std::{
    ffi::CString,
    path::{Path, PathBuf},
    sync::OnceLock,
    time::{Duration, Instant},
};

static OUTPUT_DIR: OnceLock<PathBuf> = OnceLock::new();
static HOST_EVENTS: OnceLock<host_events::HostEvents> = OnceLock::new();

fn drain_host_events() {
    let events = HOST_EVENTS.get().expect("host events initialized");
    loop {
        let next = unsafe {
            host_events::art3m1s_host_events_next_v1(
                events as *const host_events::HostEvents as *mut host_events::HostEvents,
            )
        };
        if next == 0 {
            break;
        }
        let mut bytes = vec![0u8; next];
        let mut count = 0u32;
        let written = unsafe {
            host_events::art3m1s_poll_events_v1(
                events as *const host_events::HostEvents as *mut host_events::HostEvents,
                bytes.as_mut_ptr(),
                bytes.len(),
                &mut count,
            )
        };
        if written == 0 || count == 0 {
            break;
        }
        let mut offset = 0usize;
        for _ in 0..count {
            if offset + 24 > written {
                break;
            }
            let kind = u32::from_ne_bytes(bytes[offset + 4..offset + 8].try_into().unwrap());
            let len =
                u32::from_ne_bytes(bytes[offset + 16..offset + 20].try_into().unwrap()) as usize;
            let aux = u32::from_ne_bytes(bytes[offset + 20..offset + 24].try_into().unwrap());
            offset += 24;
            if offset + len > written {
                break;
            }
            if kind == host_events::EVENT_KIND_LOG {
                let level = char::from_u32(aux).unwrap_or('I');
                let message = String::from_utf8_lossy(&bytes[offset..offset + len]);
                eprintln!("[{level}] {message}");
            }
            offset += len;
        }
    }
}

fn tick(rt: &mut CoreRuntime, count: usize, pixels: &mut Vec<u8>) {
    for _ in 0..count {
        rt.advance_and_render_into(17, pixels);
        drain_host_events();
    }
}

fn paced_tick(rt: &mut CoreRuntime, count: usize, pixels: &mut Vec<u8>) {
    let frame_time = Duration::from_nanos(16_666_667);
    for _ in 0..count {
        let started = Instant::now();
        rt.advance_and_render_into(17, pixels);
        std::thread::sleep(frame_time.saturating_sub(started.elapsed()));
    }
}

fn click(rt: &mut CoreRuntime, x: i32, y: i32, pixels: &mut Vec<u8>) {
    rt.feed_mouse(x, y);
    rt.feed_mouse_button(1, true);
    tick(rt, 1, pixels);
    rt.feed_mouse_button(1, false);
    tick(rt, 150, pixels);
}

fn snapshot(rt: &CoreRuntime, pixels: &[u8], name: &str) {
    image::save_buffer(
        OUTPUT_DIR
            .get()
            .expect("probe output directory is initialized")
            .join(format!("compat-{name}.png")),
        pixels,
        rt.stage_width(),
        rt.stage_height(),
        image::ColorType::Rgba8,
    )
    .unwrap();
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().unwrap_or_else(|| {
        eprintln!("usage: compatibility_probe <game.pfs> [mode] [output-dir]");
        std::process::exit(2);
    });
    let mode = args.next().unwrap_or("config".into());
    let output_dir = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("art3m1s-compatibility-probe"));
    std::fs::create_dir_all(&output_dir).expect("create probe output directory");
    OUTPUT_DIR.set(output_dir.clone()).unwrap();
    let save_dir = output_dir.join("saves");
    std::fs::create_dir_all(&save_dir).expect("create isolated save directory");
    let input = Path::new(&path);
    let resources = HostResources::new();
    if input.is_dir() {
        resources.mount_directory(input).unwrap();
    } else {
        resources.mount_pfs(input, "utf-8").unwrap();
    }
    resources.set_save_dir(Some(&save_dir)).unwrap();
    let events = HOST_EVENTS.get_or_init(host_events::HostEvents::new);
    events.set_enabled(true);
    unsafe {
        ffi::art3m1s_set_debug(1);
    }
    let ini = resources.read_file("system.ini").unwrap();
    let backend = match std::env::var("ART3M1S_PROBE_BACKEND").as_deref() {
        Ok("angle-metal") => GfxBackend::Angle(AngleBackend::Metal),
        Ok("angle-vulkan") => GfxBackend::Angle(AngleBackend::Vulkan),
        Ok("angle-opengl") => GfxBackend::Angle(AngleBackend::OpenGL),
        Ok("angle-d3d11") => GfxBackend::Angle(AngleBackend::D3D11),
        _ => GfxBackend::Cgl,
    };
    let mut rt = CoreRuntime::create(1280, 720, backend).unwrap();
    rt.set_resources(resources.clone());
    if let Ok(os) = std::env::var("ART3M1S_PROBE_OS") {
        let os = CString::new(os).unwrap();
        unsafe { ffi::art3m1s_runtime_set_reported_os(&mut rt, os.as_ptr()) };
    }
    if mode.starts_with("eluna") {
        let selected = unsafe { ffi::art3m1s_runtime_set_emote_backend(&mut rt, 1) };
        assert_eq!(selected, 1, "select Eluna backend");
        rt.set_profiler_enabled(true);
    }
    rt.load_project_bytes(&ini, "WINDOWS").unwrap();
    let mut pixels = vec![0; rt.pixel_buffer_size()];
    tick(&mut rt, 900, &mut pixels);
    snapshot(&rt, &pixels, "title");
    if mode.ends_with("interactive") {
        use std::io::BufRead;
        println!("PROBE READY");
        for line in std::io::stdin().lock().lines() {
            let line = line.unwrap();
            let args: Vec<_> = line.split_whitespace().collect();
            match args.as_slice() {
                ["click", x, y] => {
                    click(&mut rt, x.parse().unwrap(), y.parse().unwrap(), &mut pixels)
                }
                ["mouse", x, y] => rt.feed_mouse(x.parse().unwrap(), y.parse().unwrap()),
                ["button", key, down] => rt.feed_mouse_button(key.parse().unwrap(), *down == "1"),
                ["tick", count] => tick(&mut rt, count.parse().unwrap(), &mut pixels),
                ["pace", count] => paced_tick(&mut rt, count.parse().unwrap(), &mut pixels),
                ["shot", name] => snapshot(&rt, &pixels, name),
                ["profile"] => println!("{}", rt.profiler_snapshot_json()),
                ["trace"] => rt.set_string_variable("codex.trace", "1"),
                ["setvar", name, value] => rt.set_string_variable(name, value),
                ["dialog", accepted] => {
                    rt.submit_dialog_response(*accepted == "1", None);
                }
                ["quit"] => break,
                _ => println!("UNKNOWN {line}"),
            }
            println!("PROBE OK {line}");
        }
        return;
    }
    if mode == "config" {
        click(&mut rt, 270, 590, &mut pixels);
        snapshot(&rt, &pixels, "config");
        click(&mut rt, 637, 377, &mut pixels);
        rt.feed_mouse(520, 15);
        rt.feed_mouse_button(1, true);
        tick(&mut rt, 1, &mut pixels);
        rt.feed_mouse(400, 15);
        tick(&mut rt, 2, &mut pixels);
        rt.feed_mouse_button(1, false);
        tick(&mut rt, 100, &mut pixels);
        snapshot(&rt, &pixels, "config-after-drag");
        rt.feed_mouse_button(2, true);
        tick(&mut rt, 1, &mut pixels);
        rt.feed_mouse_button(2, false);
        tick(&mut rt, 150, &mut pixels);
        snapshot(&rt, &pixels, "back-title");
        click(&mut rt, 270, 410, &mut pixels);
        tick(&mut rt, 500, &mut pixels);
        snapshot(&rt, &pixels, "start-after-config");
    } else {
        click(&mut rt, 270, 410, &mut pixels);
        tick(&mut rt, 600, &mut pixels);
        snapshot(&rt, &pixels, "story");
        for n in 0..10 {
            click(&mut rt, 640, 400, &mut pixels);
            snapshot(&rt, &pixels, &format!("page-{n}"));
        }
        let key = if mode == "skip" { 16 } else { 17 };
        rt.set_string_variable("codex.trace", "1");
        eprintln!("PROBE SKIP key={key}");
        rt.feed_key_down(key);
        tick(&mut rt, 15, &mut pixels);
        rt.feed_key_up(key);
        tick(&mut rt, 100, &mut pixels);
        snapshot(&rt, &pixels, "ctrl-release");
        for n in 0..5 {
            click(&mut rt, 640, 400, &mut pixels);
            snapshot(&rt, &pixels, &format!("after-ctrl-{n}"));
        }
    }
    eprintln!("PROBE FINISHED exit={}", rt.is_exit_requested());
}
