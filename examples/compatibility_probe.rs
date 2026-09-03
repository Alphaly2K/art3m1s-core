use art3m1s_core::{
    archive::reader::PfsArchive, backend::gl::platform::GfxBackend, ffi, runtime::CoreRuntime,
};
use std::{
    ffi::{CStr, CString, c_char, c_int, c_longlong},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

static ARCHIVE: OnceLock<Mutex<PfsArchive>> = OnceLock::new();
static OUTPUT_DIR: OnceLock<PathBuf> = OnceLock::new();

unsafe extern "C" fn read(
    path: *const c_char,
    buf: *mut u8,
    len: c_int,
    offset: c_longlong,
) -> c_int {
    let path = unsafe { CStr::from_ptr(path) }.to_string_lossy();
    let mut archive = ARCHIVE.get().unwrap().lock().unwrap();
    let Some(entry) = archive.find(&path).cloned() else {
        return -1;
    };
    if buf.is_null() {
        return entry.size as c_int;
    }
    let buf = unsafe { std::slice::from_raw_parts_mut(buf, len as usize) };
    archive
        .read_entry(&entry, offset as u64, buf)
        .map(|n| n as c_int)
        .unwrap_or(-1)
}

unsafe extern "C" fn log(level: *const c_char, msg: *const c_char) {
    eprintln!(
        "[{}] {}",
        unsafe { CStr::from_ptr(level) }.to_string_lossy(),
        unsafe { CStr::from_ptr(msg) }.to_string_lossy()
    );
}

fn tick(rt: &mut CoreRuntime, count: usize, pixels: &mut Vec<u8>) {
    for _ in 0..count {
        rt.advance_and_render_into(17, pixels);
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
    let archive = PfsArchive::open(Path::new(&path)).unwrap();
    ARCHIVE.set(Mutex::new(archive)).ok().unwrap();
    unsafe {
        ffi::art3m1s_register_file_reader(read);
        ffi::art3m1s_register_log_callback(log);
        ffi::art3m1s_set_debug(1);
        let save_dir = CString::new(save_dir.to_string_lossy().as_bytes()).unwrap();
        ffi::art3m1s_set_save_dir(save_dir.as_ptr());
    }
    let ini = ffi::request_file("system.ini").unwrap();
    let mut rt = CoreRuntime::create(1280, 720, GfxBackend::Cgl).unwrap();
    rt.load_project_bytes(&ini, "WINDOWS").unwrap();
    let mut pixels = vec![0; rt.pixel_buffer_size()];
    tick(&mut rt, 900, &mut pixels);
    snapshot(&rt, &pixels, "title");
    if mode == "interactive" {
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
                ["shot", name] => snapshot(&rt, &pixels, name),
                ["trace"] => rt.set_string_variable("codex.trace", "1"),
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
