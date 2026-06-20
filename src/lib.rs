//! Core project bootstrap for the Artemis visual novel engine rewrite.
//!
//! This crate intentionally keeps the first layer thin: it wires an unpacked
//! Artemis project directory to `asb-interpreter`, while later renderer code can
//! consume interpreter events and map them to ANGLE-backed drawing commands.

use asb_interpreter::{Interpreter, InterpreterConfig};
use std::collections::HashMap;
use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::path::{Component, Path, PathBuf};

pub mod audio;
pub mod backend;
pub mod compositor;
pub mod ffi;
pub mod ffi_callbacks;
pub mod runtime;
pub mod save;
pub mod text;

pub use asb_interpreter as script;
pub use pfs_upk as archive;

/// Result type used by the core bootstrap layer.
pub type Result<T> = std::result::Result<T, CoreError>;

/// Errors produced before control reaches the script interpreter.
#[derive(Debug)]
pub enum CoreError {
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    MissingIniSection {
        section: String,
    },
    MissingIniKey {
        section: String,
        key: String,
    },
    InvalidIniNumber {
        section: String,
        key: String,
        value: String,
    },
    InvalidProjectPath {
        path: String,
    },
    Interpreter(asb_interpreter::Error),
}

impl Display for CoreError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(f, "failed to read {}: {}", path.display(), source)
            }
            Self::MissingIniSection { section } => {
                write!(f, "system.ini section [{}] was not found", section)
            }
            Self::MissingIniKey { section, key } => {
                write!(f, "system.ini [{}] is missing key {}", section, key)
            }
            Self::InvalidIniNumber {
                section,
                key,
                value,
            } => {
                write!(
                    f,
                    "system.ini [{}] key {} has invalid number {:?}",
                    section, key, value
                )
            }
            Self::InvalidProjectPath { path } => {
                write!(
                    f,
                    "project path {:?} must be relative and stay inside the project",
                    path
                )
            }
            Self::Interpreter(source) => Display::fmt(source, f),
        }
    }
}

impl Error for CoreError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Interpreter(source) => Some(source),
            _ => None,
        }
    }
}

impl From<asb_interpreter::Error> for CoreError {
    fn from(value: asb_interpreter::Error) -> Self {
        Self::Interpreter(value)
    }
}

/// Parsed startup configuration from one platform section of `system.ini`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectConfig {
    pub platform: String,
    pub stage_width: u32,
    pub stage_height: u32,
    pub fps: u32,
    pub charset: String,
    pub boot_script: String,
    pub frameless: bool,
    pub resizable: bool,
    pub fixed_aspect_ratio: bool,
    pub sidecut: bool,
    pub power_saving: bool,
    pub no_save: bool,
    pub savepath: Option<String>,
    pub side_picture: Option<String>,
    pub process_id: Option<String>,
    pub raw: HashMap<String, String>,
}

impl ProjectConfig {
    /// Parse a `system.ini` string and select a platform section such as
    /// `WINDOWS`, `ANDROID`, `IOS`, or `WASM`.
    pub fn from_system_ini(contents: &str, platform: &str) -> Result<Self> {
        let sections = parse_ini(contents);
        let section = platform.trim().to_ascii_uppercase();
        let values = sections
            .get(&section)
            .ok_or_else(|| CoreError::MissingIniSection {
                section: section.clone(),
            })?;

        let stage_width = required_u32(values, &section, "WIDTH")?;
        let stage_height = required_u32(values, &section, "HEIGHT")?;
        let boot_script = required_string(values, &section, "BOOT")?;

        Ok(Self {
            platform: section.clone(),
            stage_width,
            stage_height,
            fps: optional_u32(values, &section, "FPS")?.unwrap_or(60),
            charset: values
                .get("CHARSET")
                .cloned()
                .unwrap_or_else(|| "UTF-8".to_string()),
            boot_script,
            frameless: ini_bool(values.get("FRAMELESS")),
            resizable: ini_bool(values.get("RESIZABLE")),
            fixed_aspect_ratio: ini_bool(values.get("FIXED_ASPECT_RATIO")),
            sidecut: ini_bool(values.get("SIDECUT")),
            power_saving: ini_bool(values.get("POWER_SAVING")),
            no_save: ini_bool(values.get("NO_SAVE")),
            savepath: values.get("SAVEPATH").cloned(),
            side_picture: values.get("SIDE_PICTURE").cloned(),
            process_id: values.get("PREVENT_MULTIPLE_PROCESS").cloned(),
            raw: values.clone(),
        })
    }

    /// Convert the project config into the interpreter's environment config.
    pub fn to_interpreter_config(&self, project_root: Option<&Path>) -> InterpreterConfig {
        let encoding = match self.charset.to_ascii_uppercase().as_str() {
            "SHIFT_JIS" | "SHIFT-JIS" | "SJIS" => encoding_rs::SHIFT_JIS,
            _ => encoding_rs::UTF_8,
        };

        InterpreterConfig {
            encoding,
            stage_width: self.stage_width,
            stage_height: self.stage_height,
            fps: self.fps,
            frameless: self.frameless,
            resizable: self.resizable,
            fixed_aspect_ratio: self.fixed_aspect_ratio,
            sidecut: self.sidecut,
            side_picture: self.side_picture.clone(),
            power_saving: self.power_saving,
            no_save: self.no_save,
            savepath: self.savepath.clone(),
            datapath: project_root.map(|path| path.display().to_string()),
            title: None,
            process_id: self.process_id.clone(),
            env: self.raw.clone(),
            // system.ini 段名为大写（WINDOWS/ANDROID/IOS/WASM），脚本机种表用小写键。
            platform: self.platform.to_ascii_lowercase(),
        }
    }
}

/// An unpacked Artemis project directory.
#[derive(Debug, Clone)]
pub struct Project {
    root: PathBuf,
    config: ProjectConfig,
}

impl Project {
    /// Open a project from an in-memory `system.ini` string (no disk
    /// access needed).  The `root` path is stored for virtual-path
    /// resolution but not read from.
    pub fn open_from_data(root: impl Into<PathBuf>, ini_content: &str, platform: &str) -> Result<Self> {
        let root = root.into();
        let config = ProjectConfig::from_system_ini(ini_content, platform)?;
        Ok(Self { root, config })
    }

    /// Open an unpacked project directory and parse its `system.ini`.
    pub fn open(root: impl Into<PathBuf>, platform: &str) -> Result<Self> {
        let root = root.into();
        let ini_path = root.join("system.ini");
        let ini = std::fs::read_to_string(&ini_path).map_err(|source| CoreError::Io {
            path: ini_path,
            source,
        })?;
        let config = ProjectConfig::from_system_ini(&ini, platform)?;
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
    pub fn create_interpreter(&self) -> Interpreter {
        let root = self.root.clone();
        let mut interpreter =
            Interpreter::new(self.config.to_interpreter_config(Some(&self.root)));

        if crate::ffi::file_reader_registered() {
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
                let path =
                    resolve_project_path(&root, name).map_err(to_interpreter_error)?;
                std::fs::read(&path).map_err(asb_interpreter::Error::from)
            }));
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

/// Load a font file through the FFI bridge and return a `&'static` slice
/// suitable for [`crate::text::GlyphTextRenderer::set_font`].
pub fn load_font_ffi(path: &str) -> std::result::Result<&'static [u8], String> {
    let bytes = crate::ffi::request_file(path)?;
    Ok(Box::leak(bytes.into_boxed_slice()))
}

// ═══════════════════════════════════════════════════════════════════
// 私有辅助
// ═══════════════════════════════════════════════════════════════════

fn parse_ini(contents: &str) -> HashMap<String, HashMap<String, String>> {
    let mut sections = HashMap::new();
    let mut current: Option<String> = None;

    for raw_line in contents.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }

        if let Some(section) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            let name = section.trim().to_ascii_uppercase();
            sections.entry(name.clone()).or_insert_with(HashMap::new);
            current = Some(name);
            continue;
        }

        let Some(section) = &current else {
            continue;
        };

        let Some((key, value)) = line.split_once('=') else {
            continue;
        };

        sections
            .entry(section.clone())
            .or_insert_with(HashMap::new)
            .insert(key.trim().to_ascii_uppercase(), value.trim().to_string());
    }

    sections
}

fn required_string(values: &HashMap<String, String>, section: &str, key: &str) -> Result<String> {
    values
        .get(key)
        .filter(|value| !value.is_empty())
        .cloned()
        .ok_or_else(|| CoreError::MissingIniKey {
            section: section.to_string(),
            key: key.to_string(),
        })
}

fn required_u32(values: &HashMap<String, String>, section: &str, key: &str) -> Result<u32> {
    let value = required_string(values, section, key)?;
    parse_u32(section, key, &value)
}

fn optional_u32(values: &HashMap<String, String>, section: &str, key: &str) -> Result<Option<u32>> {
    values
        .get(key)
        .map(|value| parse_u32(section, key, value))
        .transpose()
}

fn parse_u32(section: &str, key: &str, value: &str) -> Result<u32> {
    value
        .trim()
        .parse()
        .map_err(|_| CoreError::InvalidIniNumber {
            section: section.to_string(),
            key: key.to_string(),
            value: value.to_string(),
        })
}

fn ini_bool(value: Option<&String>) -> bool {
    value
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "on" | "yes"
            )
        })
        .unwrap_or(false)
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
    use asb_interpreter::{CallbackResult, Event, ExecutionResult};
    use std::sync::{Arc, Mutex};

    fn sample_project_root() -> PathBuf {
        PathBuf::from("/Users/alphaly/lfpm/hamidashi")
    }

    #[test]
    fn reads_sample_windows_config() {
        let project = Project::open(sample_project_root(), "WINDOWS").unwrap();
        let config = project.config();

        assert_eq!(config.stage_width, 1280);
        assert_eq!(config.stage_height, 720);
        assert_eq!(config.fps, 60);
        assert_eq!(config.charset, "UTF-8");
        assert_eq!(config.boot_script, "system/first.iet");
        assert!(config.resizable);
        assert!(config.fixed_aspect_ratio);
        assert!(!config.frameless);
    }

    #[test]
    fn starts_sample_boot_script_at_top_and_reaches_wait() {
        let project = Project::open(sample_project_root(), "WINDOWS").unwrap();
        let mut interpreter = project.create_interpreter();
        let events = Arc::new(Mutex::new(Vec::new()));
        let captured_events = Arc::clone(&events);

        interpreter.set_callback(move |event| {
            captured_events.lock().unwrap().push(event.clone());
            match event {
                Event::Wait { .. } => CallbackResult::Pause,
                _ => CallbackResult::Continue,
            }
        });

        project.start_boot(&mut interpreter).unwrap();
        let result = interpreter.run().unwrap();

        assert!(matches!(result, ExecutionResult::Wait(Event::Wait { .. })));
        assert_eq!(interpreter.current_script(), Some("system/first.iet"));

        let events = events.lock().unwrap();
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Event::Wait { .. })),
            "expected sample boot to hit [wt]"
        );
    }

    #[test]
    fn loads_sample_ast_through_project_file_loader() {
        let project = Project::open(sample_project_root(), "WINDOWS").unwrap();
        let mut interpreter = project.create_interpreter();

        interpreter
            .load_external_script("script/gamestart.ast")
            .unwrap();

        assert!(interpreter.get_script("script/gamestart.ast").is_some());
    }
}
