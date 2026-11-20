use crate::project_config::{CoreError, ProjectConfig, Result, encoding_for_charset};
use asb_interpreter::Interpreter;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Project {
    root: PathBuf,
    config: ProjectConfig,
}

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

/// Probe the first caption using the legacy resource view before starting a renderer.
pub(super) fn probe_caption_from_bytes_with_resources(
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

fn to_interpreter_error(error: CoreError) -> asb_interpreter::Error {
    asb_interpreter::Error::IoError(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        error.to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use asb_interpreter::InterpreterConfig;

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
