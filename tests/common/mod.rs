use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use asb_interpreter::lua_engine::EngineCallbacks;

pub const FIXTURES_DIR_ENV: &str = "ART3M1S_FIXTURES_DIR";

/// Resolve a copyrighted, local-only compatibility fixture without baking a
/// developer machine path into the test suite.
pub fn fixture(name: &str) -> PathBuf {
    let override_env = format!(
        "ART3M1S_FIXTURE_{}_DIR",
        name.chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() {
                    ch.to_ascii_uppercase()
                } else {
                    '_'
                }
            })
            .collect::<String>()
    );

    if let Some(path) = std::env::var_os(&override_env) {
        return PathBuf::from(path);
    }

    let base = std::env::var_os(FIXTURES_DIR_ENV).unwrap_or_else(|| {
        panic!(
            "external fixture {name:?} is not configured; set {override_env} or \
             {FIXTURES_DIR_ENV} (with a {name}/ child directory)"
        )
    });
    PathBuf::from(base).join(name)
}

pub fn project_fixture(name: &str) -> PathBuf {
    let root = fixture(name);
    assert_project_root(name, &root);
    root
}

fn assert_project_root(name: &str, root: &Path) {
    assert!(
        root.join("system.ini").is_file(),
        "fixture {name:?} at {} is not an unpacked project root (system.ini missing)",
        root.display()
    );
}

pub struct ProjectCallbacks {
    root: PathBuf,
}

impl ProjectCallbacks {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

impl EngineCallbacks for ProjectCallbacks {
    fn debug(&self, _level: i32, data: &str, _raw: bool) {
        eprintln!("[lua] {data}");
    }

    fn enqueue_tag(&self, _tag: String, _params: HashMap<String, String>) {}
    fn set_event_handler(&self, _handlers: HashMap<String, String>) {}
    fn get_script_status(&self) -> u8 {
        0
    }
    fn is_key_down(&self, _key: u32) -> bool {
        false
    }
    fn is_key_down_edge(&self, _key: u32) -> bool {
        false
    }
    fn is_key_up_edge(&self, _key: u32) -> bool {
        false
    }
    fn is_decide(&self) -> bool {
        false
    }
    fn get_mouse_point(&self) -> (i32, i32) {
        (0, 0)
    }
    fn get_touch_count(&self) -> u32 {
        0
    }
    fn get_touch_point(&self, _index: u32) -> (i32, i32) {
        (0, 0)
    }
    fn is_file_exists(&self, path: &str) -> bool {
        art3m1s_core::resolve_project_path(&self.root, path)
            .map(|path| path.exists())
            .unwrap_or(false)
    }
    fn file_operation(&self, _command: &str, _params: HashMap<String, String>) {}
    fn include(&self, _path: &str) {}
    fn override_key(&self, _from: u32, _to: u32) {}
    fn set_flick_sensitivity(&self, _sensitivity: f64) {}
    fn get_script_block(&self) -> HashMap<String, String> {
        HashMap::new()
    }
    fn get_script_stack(&self) -> Vec<HashMap<String, String>> {
        Vec::new()
    }
    fn get_script_wait_reason(&self) -> u8 {
        0
    }
}
