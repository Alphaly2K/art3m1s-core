//! Core project bootstrap for the Artemis visual novel engine rewrite.
//!
//! This crate intentionally keeps the first layer thin: it wires an unpacked
//! Artemis project directory to `asb-interpreter`, while renderer code consumes
//! interpreter events through a graphics-API-neutral GPU backend boundary.

#[cfg(feature = "legacy")]
use asb_interpreter::Interpreter;
#[cfg(feature = "legacy")]
use std::path::{Component, Path, PathBuf};

macro_rules! legacy_exports {
    ($($module:ident),+ $(,)?) => {$(
        #[cfg(feature = "legacy")]
        pub mod $module;
        #[cfg(not(feature = "legacy"))]
        #[allow(dead_code, unused_imports)]
        mod $module;
    )+};
}

legacy_exports!(audio, compositor, host, render_pipeline, save, text, video);

#[cfg(not(feature = "legacy"))]
pub use host::api;
#[cfg(feature = "legacy")]
pub mod backend;
mod emote_playback;
#[cfg(feature = "legacy")]
pub mod ffi;
#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
pub use ffi::api as ffi_api;
#[cfg(feature = "legacy")]
pub use host::{events as host_events, files as host_files, media as host_media};
#[cfg(not(feature = "legacy"))]
mod core_log;
#[cfg(not(feature = "legacy"))]
pub(crate) use core_log::{core_debug, core_info, core_warn, emit as __core_log};
mod magic_path;
mod project_config;
#[cfg(feature = "legacy")]
use project_config::encoding_for_charset;
#[cfg(not(feature = "legacy"))]
use project_config::{CoreError, ProjectConfig};
#[cfg(feature = "legacy")]
pub use project_config::{CoreError, ProjectConfig, Result};
#[cfg(all(
    feature = "legacy",
    any(
        target_os = "android",
        target_os = "ios",
        all(target_os = "macos", target_arch = "aarch64")
    )
))]
mod platform;
#[cfg(feature = "legacy")]
pub mod profiler;
#[cfg(feature = "legacy")]
pub mod runtime;
mod script_handler;

#[cfg(feature = "legacy")]
pub use art3m1s_emote as emote;
#[cfg(feature = "legacy")]
pub use art3m1s_media as media;
#[cfg(feature = "legacy")]
pub use asb_interpreter as script;
#[cfg(feature = "legacy")]
pub use pfs_upk as archive;

#[cfg(feature = "legacy")]
#[derive(Debug, Clone)]
pub struct Project {
    root: PathBuf,
    config: ProjectConfig,
}

#[cfg(feature = "legacy")]
impl Project {
    /// Open a project from an in-memory `system.ini` string (no disk
    /// access needed).  The `root` path is stored for virtual-path
    /// resolution but not read from.
    pub fn open_from_data(
        root: impl Into<PathBuf>,
        ini_content: &str,
        platform: &str,
    ) -> Result<Self> {
        let root = root.into();
        let config = ProjectConfig::from_system_ini(ini_content, platform)?;
        Ok(Self { root, config })
    }

    /// Open a project from raw `system.ini` bytes.
    pub fn open_from_bytes(
        root: impl Into<PathBuf>,
        ini_content: &[u8],
        platform: &str,
    ) -> Result<Self> {
        let root = root.into();
        let config = ProjectConfig::from_system_ini_bytes(ini_content, platform)?;
        Ok(Self { root, config })
    }

    /// Open an unpacked project directory and parse its `system.ini`.
    pub fn open(root: impl Into<PathBuf>, platform: &str) -> Result<Self> {
        let root = root.into();
        let ini_path = root.join("system.ini");
        let ini = std::fs::read(&ini_path).map_err(|source| CoreError::Io {
            path: ini_path,
            source,
        })?;
        let config = ProjectConfig::from_system_ini_bytes(&ini, platform)?;
        Ok(Self { root, config })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn config(&self) -> &ProjectConfig {
        &self.config
    }

    /// Resolve a script/resource path used inside Artemis scripts.
    pub fn resolve_path(&self, virtual_path: &str) -> Result<PathBuf> {
        resolve_project_path(&self.root, virtual_path)
    }

    /// Read a project file by its virtual path.
    pub fn read_file(&self, virtual_path: &str) -> Result<Vec<u8>> {
        let path = self.resolve_path(virtual_path)?;
        std::fs::read(&path).map_err(|source| CoreError::Io { path, source })
    }

    /// Create an interpreter configured for this project and install a file
    /// loader that can resolve `.iet`, `.ast`, and `.asb` paths from scripts.
    ///
    /// If the FFI file reader has been registered (Flutter frontend in
    /// control), all script loading is routed through the callback.
    /// Otherwise, files are read directly from disk (standalone mode).
    /// An optional `tag.ini` is read through the same source and decoded with
    /// the project's CHARSET before any script is parsed.
    pub fn create_interpreter(&self) -> Interpreter {
        self.create_interpreter_with_resources(None)
    }

    pub fn create_interpreter_with_resources(
        &self,
        resources: Option<&crate::host_files::HostResources>,
    ) -> Interpreter {
        let root = self.root.clone();
        let mut interpreter = Interpreter::new(self.config.to_interpreter_config(Some(&self.root)));

        if let Some(resources) = resources.cloned() {
            interpreter.set_file_loader(Box::new(move |name| {
                resources.read_file(name).map_err(|m| {
                    asb_interpreter::Error::IoError(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        m,
                    ))
                })
            }));
        } else if crate::ffi::file_reader_registered() {
            interpreter.set_file_loader(Box::new(move |name| {
                let bytes = crate::ffi::request_file(name).map_err(|m| {
                    asb_interpreter::Error::IoError(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        m,
                    ))
                })?;
                Ok(bytes)
            }));
        } else {
            interpreter.set_file_loader(Box::new(move |name| {
                let path = resolve_project_path(&root, name).map_err(to_interpreter_error)?;
                std::fs::read(&path).map_err(asb_interpreter::Error::from)
            }));
        }

        let tag_ini = if let Some(resources) = resources {
            resources.read_file("tag.ini").ok()
        } else if crate::ffi::file_reader_registered() {
            crate::ffi::request_file("tag.ini").ok()
        } else {
            self.read_file("tag.ini").ok()
        };
        if let Some(bytes) = tag_ini {
            let (text, _, _) = encoding_for_charset(&self.config.charset).decode(&bytes);
            interpreter.load_tag_ini(&text);
        }

        interpreter
    }

    /// Load and start the configured BOOT script.
    ///
    /// Artemis projects commonly use `*top` as the first label. If it is not
    /// present, this falls back to the labels supported by `Interpreter::boot`.
    pub fn start_boot(&self, interpreter: &mut Interpreter) -> Result<()> {
        let boot = self.config.boot_script.as_str();
        interpreter.load_external_script(boot)?;

        if let Some(script) = interpreter.get_script(boot) {
            for label in ["top", "main", "start", "_start"] {
                if script.get_label_line(label).is_some() {
                    interpreter.start(boot, label)?;
                    return Ok(());
                }
            }
        }

        interpreter.boot(boot)?;
        Ok(())
    }
}

/// Headless caption 探测：只把 boot 跑到解释器发出第一个 `[caption]`（`Event::Caption`）
/// 就停下并返回其文本。**不建 GL / compositor / 渲染**——因为 caption 是解释器发出的
/// 事件，跑解释器本身即可拿到，用于导入游戏时近乎瞬时地取得真实标题（再拿去查 VNDB）。
///
/// 文件加载复用 [`Project::create_interpreter`] 装好的 FFI 文件加载器，故宿主须在调用前
/// 把文件供给（目录/pfs）指向该游戏。任何失败（system.ini 非法、boot 读不到、boot 在
/// 发 caption 前就阻塞/跑完/出错）都返回 `None`，调用方回退到手动/目录名。
#[cfg(feature = "legacy")]
fn caption_probe_blocks_progress(event: &asb_interpreter::Event) -> bool {
    use asb_interpreter::{Event, WaitReason};
    match event {
        Event::ShowDialog { .. }
        | Event::YesNo { .. }
        | Event::Exit
        | Event::GoTitle
        | Event::Reset
        | Event::ScenarioText { .. }
        | Event::Text { .. } => true,
        // [wt]/[wait time=…] 是加载序列里的让帧，探测需要越过它们才能到达
        // [caption]。[stop]/[@]/按键等待则是真正的阻塞点，继续 Continue 会把
        // `[stop]` 后的 `[jump label="top"]` 空转成导入卡死。
        Event::Wait { reason } => matches!(
            reason,
            WaitReason::Stop { .. } | WaitReason::KeyWait { .. } | WaitReason::Generic
        ),
        _ => false,
    }
}

#[cfg(feature = "legacy")]
fn caption_probe_on_event(
    event: asb_interpreter::Event,
    caption: &std::sync::Mutex<Option<String>>,
) -> asb_interpreter::CallbackResult {
    use asb_interpreter::{CallbackResult, Event};
    match event {
        Event::Caption { data } => {
            *caption.lock().unwrap() = Some(data);
            CallbackResult::Pause
        }
        event if caption_probe_blocks_progress(&event) => CallbackResult::Pause,
        _ => CallbackResult::Continue,
    }
}

#[cfg(feature = "legacy")]
fn probe_caption_from_bytes_with_resources(
    ini_content: &[u8],
    platform: &str,
    resources: &crate::host_files::HostResources,
) -> Option<String> {
    use std::sync::{Arc, Mutex};

    let project = Project::open_from_bytes("", ini_content, platform).ok()?;
    let mut interpreter = project.create_interpreter_with_resources(Some(resources));

    let caption: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let caption_cb = Arc::clone(&caption);
    interpreter.set_callback(move |event| caption_probe_on_event(event, &caption_cb));

    project.start_boot(&mut interpreter).ok()?;
    // run()/step 会把 Continue 的 Wait 直接跳过。探测只越过加载让帧，在
    // [stop]/对话框/文本等真正阻塞点停下。
    let _ = interpreter.run();
    caption.lock().unwrap().take()
}

// ═══════════════════════════════════════════════════════════════════
// 私有辅助
// ═══════════════════════════════════════════════════════════════════

#[cfg(feature = "legacy")]
pub fn resolve_project_path(root: &Path, virtual_path: &str) -> Result<PathBuf> {
    let normalized = virtual_path.replace('\\', "/");
    let relative = Path::new(&normalized);

    if relative.is_absolute() {
        return Err(CoreError::InvalidProjectPath {
            path: virtual_path.to_string(),
        });
    }

    let mut resolved = PathBuf::from(root);
    for component in relative.components() {
        match component {
            Component::Normal(part) => resolved.push(part),
            Component::CurDir => {}
            _ => {
                return Err(CoreError::InvalidProjectPath {
                    path: virtual_path.to_string(),
                });
            }
        }
    }

    Ok(resolved)
}

#[cfg(feature = "legacy")]
fn to_interpreter_error(error: CoreError) -> asb_interpreter::Error {
    asb_interpreter::Error::IoError(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        error.to_string(),
    ))
}

#[cfg(all(test, feature = "legacy"))]
mod tests {
    use asb_interpreter::InterpreterConfig;
    use super::*;

    #[test]
    fn project_loads_optional_tag_ini_with_script_charset() {
        let root = std::env::temp_dir().join(format!(
            "art3m1s-tag-ini-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        for charset in ["UTF-8", "Shift_JIS"] {
            let encoding = encoding_for_charset(charset);
            let (table, _, _) = encoding.encode("[立ち絵表示]\n0=st\n1=pos\n2=time\n");
            let (script, _, _) = encoding.encode("*top\n[&linetag allow=\"1\" prefix=\"#\"]\n#立ち絵表示 character,c,default\n[stop]\n");
            std::fs::write(root.join("tag.ini"), &table).unwrap();
            std::fs::write(root.join("boot.txt"), &script).unwrap();
            let project = Project::open_from_data(
                &root,
                &format!("[WINDOWS]\nWIDTH=1280\nHEIGHT=720\nBOOT=boot.txt\nCHARSET={charset}\n"),
                "windows",
            )
            .unwrap();
            let mut interpreter = project.create_interpreter();
            project.start_boot(&mut interpreter).unwrap();
            let picture = &interpreter.get_script("boot.txt").unwrap().instructions[0];
            assert_eq!(picture.tag, "立ち絵表示");
            assert_eq!(picture.get("st"), Some("character"));
            assert_eq!(picture.get("pos"), Some("c"));
            assert_eq!(picture.get("time"), Some("default"));

            std::fs::remove_file(root.join("tag.ini")).unwrap();
            let mut without_ini = project.create_interpreter();
            project.start_boot(&mut without_ini).unwrap();
            let picture = &without_ini.get_script("boot.txt").unwrap().instructions[0];
            assert!(picture.get("st").is_none());
            assert_eq!(picture.get("0"), Some("character"));
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    fn caption_probe_callback(
        caption: &std::sync::Arc<std::sync::Mutex<Option<String>>>,
    ) -> impl FnMut(asb_interpreter::Event) -> asb_interpreter::CallbackResult + use<> {
        let caption = std::sync::Arc::clone(caption);
        move |event| caption_probe_on_event(event, &caption)
    }

    #[test]
    fn caption_capture_mechanism_grabs_caption_before_stop() {
        use std::sync::{Arc, Mutex};

        let mut interp = Interpreter::new(InterpreterConfig::default());
        interp
            .load_script("boot", "*top\n[caption data=\"探测标题\"]\n[stop]\n")
            .unwrap();
        let caption: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        interp.set_callback(caption_probe_callback(&caption));
        interp.start("boot", "top").unwrap();
        let _ = interp.run();
        assert_eq!(caption.lock().unwrap().as_deref(), Some("探测标题"));
    }

    #[test]
    fn caption_probe_continues_past_yield_wait_to_caption() {
        use std::sync::{Arc, Mutex};

        let mut interp = Interpreter::new(InterpreterConfig::default());
        interp
            .load_script(
                "boot",
                "*top\n[wt]\n[caption data=\"加载后标题\"]\n[stop]\n",
            )
            .unwrap();
        let caption: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        interp.set_callback(caption_probe_callback(&caption));
        interp.start("boot", "top").unwrap();
        let _ = interp.run();
        assert_eq!(caption.lock().unwrap().as_deref(), Some("加载后标题"));
    }

    #[test]
    fn caption_probe_stops_at_stop_before_later_caption() {
        use std::sync::{Arc, Mutex};

        let mut interp = Interpreter::new(InterpreterConfig::default());
        interp
            .load_script("boot", "*top\n[stop]\n[caption data=\"不该执行\"]\n")
            .unwrap();
        let caption: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        interp.set_callback(caption_probe_callback(&caption));
        interp.start("boot", "top").unwrap();
        let _ = interp.run();
        assert_eq!(caption.lock().unwrap().as_deref(), None);
    }

    #[test]
    fn caption_probe_stops_at_stop_instead_of_jumping_forever() {
        use std::sync::{Arc, Mutex};

        let mut interp = Interpreter::new(InterpreterConfig::default());
        interp
            .load_script("boot", "*top\n[stop]\n[jump label=\"top\"]\n")
            .unwrap();
        let caption: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        interp.set_callback(caption_probe_callback(&caption));
        interp.start("boot", "top").unwrap();
        let _ = interp.run();
        assert_eq!(caption.lock().unwrap().as_deref(), None);
    }

    #[test]
    fn system_ini_bytes_default_to_shift_jis() {
        let (title, _, _) = encoding_rs::SHIFT_JIS.encode("タイトル");
        let mut ini = b"[WINDOWS]\nWIDTH=800\nHEIGHT=600\nBOOT=boot.iet\nTITLE=".to_vec();
        ini.extend_from_slice(&title);
        ini.extend_from_slice(b"\n");

        let config = ProjectConfig::from_system_ini_bytes(&ini, "WINDOWS").unwrap();

        assert_eq!(config.charset, "Shift_JIS");
        assert_eq!(
            config.raw.get("TITLE").map(String::as_str),
            Some("タイトル")
        );
        assert_eq!(
            config.to_interpreter_config(None).encoding.name(),
            "Shift_JIS"
        );
    }

    #[test]
    fn system_ini_bytes_respect_utf8_charset() {
        let ini =
            "[WINDOWS]\nWIDTH=800\nHEIGHT=600\nBOOT=boot.iet\nCHARSET=UTF-8\nTITLE=タイトル\n";

        let config = ProjectConfig::from_system_ini_bytes(ini.as_bytes(), "WINDOWS").unwrap();

        assert_eq!(config.charset, "UTF-8");
        assert_eq!(
            config.raw.get("TITLE").map(String::as_str),
            Some("タイトル")
        );
        assert_eq!(config.to_interpreter_config(None).encoding.name(), "UTF-8");
    }
}
