//! Standard [`EngineCallbacks`] implementation that routes everything
//! through the FFI bridge — zero direct filesystem access.

use asb_interpreter::lua_engine::EngineCallbacks;
use std::collections::HashMap;
use std::sync::Mutex;

use crate::ffi;

/// Magic path table shared between callbacks and texture source.
pub type MagicPathTable = Mutex<HashMap<String, String>>;

/// Engine callbacks that use the FFI bridge for all file access.
pub struct FfiCallbacks {
    pub input: std::sync::Arc<std::sync::Mutex<InputSnapshot>>,
    pub magic_paths: std::sync::Arc<MagicPathTable>,
    pub volumes: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, f32>>>,
}

/// Minimal input state snapshot (mirrors the winit version but without
/// winit dependency).
#[derive(Default)]
pub struct InputSnapshot {
    pub mouse_x: i32,
    pub mouse_y: i32,
    pub clicked: bool,
    pub keys_down: std::collections::HashSet<u32>,
    pub keys_down_edge: std::collections::HashSet<u32>,
    pub keys_up_edge: std::collections::HashSet<u32>,
    pub key_overrides: HashMap<u32, bool>,
}

impl InputSnapshot {
    pub fn clear_edges(&mut self) {
        self.keys_down_edge.clear();
        self.keys_up_edge.clear();
    }
    fn key_down(&self, vk: u32) -> bool {
        self.key_overrides.get(&vk).copied().unwrap_or_else(|| self.keys_down.contains(&vk))
    }
}

impl EngineCallbacks for FfiCallbacks {
    fn debug(&self, _level: i32, data: &str, _raw: bool) {
        crate::core_info!("{data}");
    }

    fn enqueue_tag(&self, _tag: String, _params: HashMap<String, String>) {}
    fn set_event_handler(&self, _handlers: HashMap<String, String>) {}

    fn set_magic_path(&self, name: &str, path: &str) {
        self.magic_paths.lock().unwrap().insert(name.to_string(), path.to_string());
    }

    fn get_script_status(&self) -> u8 { 0 }

    fn is_key_down(&self, key_id: u32) -> bool {
        self.input.lock().unwrap().key_down(key_id)
    }
    fn is_key_down_edge(&self, key_id: u32) -> bool {
        self.input.lock().unwrap().keys_down_edge.contains(&key_id)
    }
    fn is_key_up_edge(&self, key_id: u32) -> bool {
        self.input.lock().unwrap().keys_up_edge.contains(&key_id)
    }
    fn override_key(&self, from: u32, to: u32) {
        let mut s = self.input.lock().unwrap();
        if to == 0 { s.key_overrides.remove(&from); } else { s.key_overrides.insert(from, true); }
    }
    fn is_decide(&self) -> bool { self.input.lock().unwrap().clicked }
    fn get_mouse_point(&self) -> (i32, i32) {
        let s = self.input.lock().unwrap();
        (s.mouse_x, s.mouse_y)
    }
    fn get_touch_count(&self) -> u32 { 0 }
    fn get_touch_point(&self, _index: u32) -> (i32, i32) { (0, 0) }

    fn is_file_exists(&self, path: &str) -> bool {
        let resolved = resolve_magic(&self.magic_paths, path);
        ffi::query_asset_size(&resolved).is_some()
    }

    fn load_png_comments(&self, path: &str) -> Option<HashMap<String, String>> {
        let resolved = resolve_magic(&self.magic_paths, path);
        let bytes = ffi::request_asset(&resolved)?;
        let comments = parse_png_text_chunks(&bytes);
        if comments.is_empty() { None } else { Some(comments) }
    }

    fn file_operation(&self, command: &str, params: HashMap<String, String>) {
        let _ = (command, params); // delegate to frontend via FFI
    }

    fn set_master_volume(&self, volume: f32) {
        self.volumes.lock().unwrap().insert("master".to_string(), volume);
    }
    fn set_bgm_volume(&self, volume: f32) {
        self.volumes.lock().unwrap().insert("bgm".to_string(), volume);
    }
    fn set_se_volume(&self, volume: f32) {
        self.volumes.lock().unwrap().insert("se".to_string(), volume);
    }
    fn set_voice_volume(&self, volume: f32) {
        self.volumes.lock().unwrap().insert("voice".to_string(), volume);
    }

    fn include(&self, _path: &str) {}
    fn set_flick_sensitivity(&self, _sensitivity: f64) {}
    fn get_script_block(&self) -> HashMap<String, String> { HashMap::new() }
    fn get_script_stack(&self) -> Vec<HashMap<String, String>> { vec![] }
    fn get_script_wait_reason(&self) -> u8 { 0 }
}

/// Resolve `:name/rest` magic paths through the magic-path table,
/// falling back to `image/rest` when the prefix is not registered.
fn resolve_magic(table: &std::sync::Arc<MagicPathTable>, name: &str) -> String {
    if let Some(rest) = name.strip_prefix(':') {
        let (ns, tail) = rest.split_once('/').unwrap_or((rest, ""));
        let map = table.lock().unwrap();
        if let Some(prefix) = map.get(ns) {
            return format!("{prefix}/{tail}");
        }
        return format!("image/{rest}");
    }
    name.to_string()
}

/// Parse PNG tEXt chunks into `keyword → text` map.
/// Used by `load_png_comments` to extract positioning metadata.
fn parse_png_text_chunks(bytes: &[u8]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    const SIG: usize = 8;
    if bytes.len() < SIG || &bytes[..SIG] != b"\x89PNG\r\n\x1a\n" { return out; }
    let mut i = SIG;
    while i + 8 <= bytes.len() {
        let len = u32::from_be_bytes([bytes[i], bytes[i+1], bytes[i+2], bytes[i+3]]) as usize;
        let typ = &bytes[i + 4..i + 8];
        let data_start = i + 8;
        let data_end = data_start + len;
        if data_end > bytes.len() { break; }
        if typ == b"tEXt" {
            let data = &bytes[data_start..data_end];
            if let Some(nul) = data.iter().position(|&b| b == 0) {
                let keyword: String = data[..nul].iter().map(|&b| b as char).collect();
                let text: String = data[nul + 1..].iter().map(|&b| b as char).collect();
                out.insert(keyword, text);
            }
        }
        if typ == b"IEND" { break; }
        i = data_end + 4;
    }
    out
}
