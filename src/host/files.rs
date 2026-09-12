//! Native resource and save-file host for the callback-free ABI.
//!
//! Directory and PFS resources are indexed once at mount time. The Dart layer
//! may still prepare compatibility overrides, but it submits them as data
//! instead of registering native callbacks.

use crate::archive::reader::PfsArchive;
use art3m1s_media::MediaSource;
use encoding_rs::{Encoding, GB18030, GBK, SHIFT_JIS, UTF_8};
use std::collections::HashMap;
use std::ffi::{CStr, c_char, c_int};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

#[derive(Clone)]
enum IndexedResource {
    Pfs {
        archive: usize,
        path: String,
        size: u64,
    },
    File(PathBuf),
}

struct HostFiles {
    directory: Option<PathBuf>,
    archives: Vec<PfsArchive>,
    index: HashMap<String, IndexedResource>,
    overrides: HashMap<String, Vec<u8>>,
    save_dir: Option<PathBuf>,
}

#[derive(Clone)]
pub struct HostResources {
    files: Arc<Mutex<Option<HostFiles>>>,
}

static DEFAULT_RESOURCES: OnceLock<HostResources> = OnceLock::new();

impl HostResources {
    pub fn new() -> Self {
        Self {
            files: Arc::new(Mutex::new(None)),
        }
    }

    pub fn clear(&self) {
        *self.files.lock().unwrap() = None;
    }

    pub fn is_mounted(&self) -> bool {
        self.files
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|files| files.directory.is_some() || !files.archives.is_empty())
    }

    pub fn set_save_dir(&self, path: Option<&Path>) -> Result<(), String> {
        let mut guard = self.files.lock().unwrap();
        let files = guard.get_or_insert_with(empty_files);
        files.save_dir = path.map(Path::to_path_buf);
        if let Some(path) = path {
            fs::create_dir_all(path)
                .map_err(|error| format!("create {}: {error}", path.display()))?;
        }
        Ok(())
    }

    pub fn mount_directory(&self, root: &Path) -> Result<(), String> {
        if !root.is_dir() {
            return Err(format!("not a directory: {}", root.display()));
        }
        let index = index_directory(root)?;
        let previous_save_dir = self
            .files
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|files| files.save_dir.clone());
        let mut files = empty_files();
        files.directory = Some(root.to_path_buf());
        files.index = index;
        files.save_dir = previous_save_dir;
        *self.files.lock().unwrap() = Some(files);
        Ok(())
    }

    pub fn mount_pfs(&self, path: &Path, encoding: &str) -> Result<(), String> {
        let previous_save_dir = self
            .files
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|files| files.save_dir.clone());
        let mut files = mount_pfs_files(path, encoding)?;
        files.save_dir = previous_save_dir;
        *self.files.lock().unwrap() = Some(files);
        Ok(())
    }

    pub fn set_override(&self, path: &str, bytes: Vec<u8>) -> Result<(), String> {
        let key = normalize_lookup(path).ok_or_else(|| format!("invalid override path: {path}"))?;
        let mut guard = self.files.lock().unwrap();
        let files = guard.get_or_insert_with(empty_files);
        files.overrides.insert(key, bytes);
        Ok(())
    }

    pub fn clear_overrides(&self) {
        if let Some(files) = self.files.lock().unwrap().as_mut() {
            files.overrides.clear();
        }
    }

    pub fn query_size(&self, path: &str) -> Result<Option<u64>, String> {
        let guard = self.files.lock().unwrap();
        let files = guard
            .as_ref()
            .ok_or_else(|| "file host is not mounted".to_string())?;
        Ok(query_size_with(files, path))
    }

    pub fn read_file(&self, path: &str) -> Result<Vec<u8>, String> {
        let size = self
            .query_size(path)?
            .ok_or_else(|| format!("not found: {path}"))?;
        let mut output = vec![0u8; size as usize];
        if output.is_empty() {
            return Ok(output);
        }
        let read = self.read_range(path, 0, &mut output)?;
        if read != output.len() {
            return Err(format!(
                "short read: {path} ({read} of {} bytes)",
                output.len()
            ));
        }
        Ok(output)
    }

    pub fn read_range(&self, path: &str, offset: u64, output: &mut [u8]) -> Result<usize, String> {
        let mut guard = self.files.lock().unwrap();
        let files = guard
            .as_mut()
            .ok_or_else(|| "file host is not mounted".to_string())?;
        read_range_with(files, path, offset, output)
    }

    pub fn write(&self, path: &str, data: &[u8]) -> Result<(), String> {
        let mut guard = self.files.lock().unwrap();
        let files = guard
            .as_mut()
            .ok_or_else(|| "file host is not mounted".to_string())?;
        let target = save_path(files, path).ok_or_else(|| format!("invalid save path: {path}"))?;
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("create {}: {error}", parent.display()))?;
        }
        fs::write(&target, data).map_err(|error| format!("write {}: {error}", target.display()))
    }

    pub fn delete(&self, path: &str) -> Result<(), String> {
        let mut guard = self.files.lock().unwrap();
        let files = guard
            .as_mut()
            .ok_or_else(|| "file host is not mounted".to_string())?;
        let target = save_path(files, path).ok_or_else(|| format!("invalid save path: {path}"))?;
        if target.exists() {
            fs::remove_file(&target)
                .map_err(|error| format!("delete {}: {error}", target.display()))?;
        }
        Ok(())
    }

    pub fn file_mtime(&self, path: &str) -> Option<[i64; 6]> {
        let _ = path;
        None
    }

    pub fn open_media_source(&self, path: &str) -> Result<Arc<dyn MediaSource>, String> {
        if !self.is_mounted() {
            return Err("file host is not mounted".to_string());
        }
        self.query_size(path)?
            .ok_or_else(|| format!("not found: {path}"))?;
        Ok(Arc::new(HostFileSource {
            path: path.to_string(),
            resources: self.clone(),
        }))
    }
}

pub fn default_resources() -> &'static HostResources {
    DEFAULT_RESOURCES.get_or_init(HostResources::new)
}

fn empty_files() -> HostFiles {
    HostFiles {
        directory: None,
        archives: Vec::new(),
        index: HashMap::new(),
        overrides: HashMap::new(),
        save_dir: None,
    }
}

fn normalize_lookup(path: &str) -> Option<String> {
    let mut parts = Vec::new();
    let normalized = path.replace('\\', "/");
    for raw in normalized.split('/') {
        if raw.is_empty() || raw == "." {
            continue;
        }
        if raw == ".." {
            return None;
        }
        parts.push(raw);
    }
    (!parts.is_empty()).then(|| parts.join("/").to_ascii_lowercase())
}

fn normalize_save_path(path: &str) -> Option<PathBuf> {
    let mut parts = Vec::new();
    let normalized = path.trim().replace('\\', "/");
    for raw in normalized.split('/') {
        let raw = raw.trim();
        if raw.is_empty() || raw == "." {
            continue;
        }
        if raw == ".." || raw.contains(':') {
            return None;
        }
        parts.push(raw);
    }
    (!parts.is_empty()).then(|| parts.iter().collect())
}

fn save_path(files: &HostFiles, path: &str) -> Option<PathBuf> {
    let root = files.save_dir.as_ref()?;
    Some(root.join(normalize_save_path(path)?))
}

fn sidecar_path(files: &HostFiles, path: &str) -> Option<PathBuf> {
    let root = files.directory.as_ref()?;
    Some(root.join(normalize_save_path(path)?))
}

fn encoding_for(name: &str) -> &'static Encoding {
    match name.trim().to_ascii_lowercase().as_str() {
        "shift_jis" | "shift-jis" | "sjis" => SHIFT_JIS,
        "gbk" | "gb2312" => GBK,
        "gb18030" => GB18030,
        "utf-8" | "utf8" => UTF_8,
        _ => UTF_8,
    }
}

fn index_directory(root: &Path) -> Result<HashMap<String, IndexedResource>, String> {
    let mut index = HashMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let entries = fs::read_dir(&directory)
            .map_err(|error| format!("read_dir {}: {error}", directory.display()))?;
        for entry in entries {
            let entry = entry.map_err(|error| format!("read_dir entry: {error}"))?;
            let file_type = entry
                .file_type()
                .map_err(|error| format!("file_type {}: {error}", entry.path().display()))?;
            if file_type.is_dir() {
                pending.push(entry.path());
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            let entry_path = entry.path();
            let Ok(relative) = entry_path.strip_prefix(root) else {
                continue;
            };
            let relative = relative.to_string_lossy().replace('\\', "/");
            let Some(key) = normalize_lookup(&relative) else {
                continue;
            };
            index.insert(key, IndexedResource::File(entry.path()));
        }
    }
    Ok(index)
}

fn is_archive_candidate(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".pfs") {
        return true;
    }
    let Some((_, suffix)) = lower.rsplit_once(".pfs.") else {
        return false;
    };
    suffix.len() == 3 && suffix.bytes().all(|byte| byte.is_ascii_digit())
}

fn mount_pfs_files(path: &Path, encoding: &str) -> Result<HostFiles, String> {
    let parent = path
        .parent()
        .ok_or_else(|| "PFS path has no parent directory".to_string())?;
    let mut candidates = Vec::new();
    for entry in
        fs::read_dir(parent).map_err(|error| format!("read_dir {}: {error}", parent.display()))?
    {
        let entry = entry.map_err(|error| format!("read_dir entry: {error}"))?;
        if !entry
            .file_type()
            .map_err(|error| format!("file_type {}: {error}", entry.path().display()))?
            .is_file()
        {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        // macOS exposes AppleDouble sidecars such as `._root.pfs.001` beside
        // archives on non-native filesystems. They match the extension pattern
        // but are metadata, not PFS archives.
        if !name.starts_with("._") && is_archive_candidate(&name) {
            candidates.push(entry.path());
        }
    }
    candidates.sort();
    if candidates.is_empty() {
        return Err(format!("no PFS archives under {}", parent.display()));
    }

    let encoding = encoding_for(encoding);
    let mut files = empty_files();
    // Some games keep videos and other large assets beside the PFS instead
    // of inside it. Keep the parent directory as a lazy fallback after the
    // archive index rather than recursively indexing the whole volume.
    files.directory = Some(parent.to_path_buf());
    let requested = path.to_path_buf();
    for candidate in candidates {
        match PfsArchive::open_with_encoding(&candidate, encoding) {
            Ok(archive) => files.archives.push(archive),
            Err(error) if candidate == requested => {
                return Err(format!("open {}: {error}", candidate.display()));
            }
            Err(_) => {
                // Sibling `.pfs.NNN` files include split volumes and unrelated
                // files with the same extension. The old host attempted each
                // archive independently and ignored invalid candidates.
            }
        }
    }
    if files.archives.is_empty() {
        return Err(format!(
            "no readable PFS archives under {}",
            parent.display()
        ));
    }

    // Later archives are patch volumes and win over earlier archives.
    for archive_index in (0..files.archives.len()).rev() {
        for entry in files.archives[archive_index].entries() {
            let path = entry.path().to_string_lossy().replace('\\', "/");
            let Some(key) = normalize_lookup(&path) else {
                continue;
            };
            let size = u64::from(entry.size());
            if size == 0 {
                continue;
            }
            files.index.entry(key).or_insert(IndexedResource::Pfs {
                archive: archive_index,
                path,
                size,
            });
        }
    }
    Ok(files)
}

fn read_source(
    files: &mut HostFiles,
    source: IndexedResource,
    offset: u64,
    output: &mut [u8],
) -> Result<usize, String> {
    match source {
        IndexedResource::File(path) => {
            let mut file =
                File::open(&path).map_err(|error| format!("open {}: {error}", path.display()))?;
            file.seek(SeekFrom::Start(offset))
                .map_err(|error| format!("seek {}: {error}", path.display()))?;
            file.read(output)
                .map_err(|error| format!("read {}: {error}", path.display()))
        }
        IndexedResource::Pfs { archive, path, .. } => {
            let entry = files
                .archives
                .get(archive)
                .and_then(|archive| archive.find(&path))
                .cloned()
                .ok_or_else(|| format!("PFS entry disappeared: {path}"))?;
            files.archives[archive]
                .read_entry(&entry, offset, output)
                .map_err(|error| format!("read PFS {path}: {error}"))
        }
    }
}

fn query_size_with(files: &HostFiles, path: &str) -> Option<u64> {
    let key = normalize_lookup(path)?;
    if let Some(save) = save_path(files, path)
        && save.is_file()
    {
        return save.metadata().ok().map(|metadata| metadata.len());
    }
    if let Some(bytes) = files.overrides.get(&key) {
        return Some(bytes.len() as u64);
    }
    if let Some(sidecar) = sidecar_path(files, path).filter(|path| path.is_file()) {
        return sidecar.metadata().ok().map(|metadata| metadata.len());
    }
    match files.index.get(&key) {
        Some(IndexedResource::Pfs { size, .. }) => Some(*size),
        Some(IndexedResource::File(path)) => path.metadata().ok().map(|metadata| metadata.len()),
        None => None,
    }
}

fn read_range_with(
    files: &mut HostFiles,
    path: &str,
    offset: u64,
    output: &mut [u8],
) -> Result<usize, String> {
    let Some(key) = normalize_lookup(path) else {
        return Err(format!("invalid resource path: {path}"));
    };
    if let Some(save) = save_path(files, path)
        && save.is_file()
    {
        let mut file =
            File::open(&save).map_err(|error| format!("open {}: {error}", save.display()))?;
        file.seek(SeekFrom::Start(offset))
            .map_err(|error| format!("seek {}: {error}", save.display()))?;
        return file
            .read(output)
            .map_err(|error| format!("read {}: {error}", save.display()));
    }
    if let Some(bytes) = files.overrides.get(&key) {
        let offset = usize::try_from(offset).unwrap_or(usize::MAX);
        if offset >= bytes.len() {
            return Ok(0);
        }
        let count = output.len().min(bytes.len() - offset);
        output[..count].copy_from_slice(&bytes[offset..offset + count]);
        return Ok(count);
    }
    if let Some(sidecar) = sidecar_path(files, path).filter(|path| path.is_file()) {
        let mut file =
            File::open(&sidecar).map_err(|error| format!("open {}: {error}", sidecar.display()))?;
        file.seek(SeekFrom::Start(offset))
            .map_err(|error| format!("seek {}: {error}", sidecar.display()))?;
        return file
            .read(output)
            .map_err(|error| format!("read {}: {error}", sidecar.display()));
    }
    if let Some(source) = files.index.get(&key).cloned() {
        return read_source(files, source, offset, output);
    }
    Err(format!("not found: {path}"))
}

pub fn clear() {
    default_resources().clear();
}

pub fn is_mounted() -> bool {
    default_resources().is_mounted()
}

pub fn set_save_dir(path: Option<&Path>) -> Result<(), String> {
    default_resources().set_save_dir(path)
}

pub fn mount_directory(root: &Path) -> Result<(), String> {
    default_resources().mount_directory(root)
}

pub fn mount_pfs(path: &Path, encoding: &str) -> Result<(), String> {
    default_resources().mount_pfs(path, encoding)
}

pub fn set_override(path: &str, bytes: Vec<u8>) -> Result<(), String> {
    default_resources().set_override(path, bytes)
}

pub fn clear_overrides() {
    default_resources().clear_overrides();
}

pub fn query_size(path: &str) -> Result<Option<u64>, String> {
    default_resources().query_size(path)
}

pub fn read_file(path: &str) -> Result<Vec<u8>, String> {
    default_resources().read_file(path)
}

pub fn read_range(path: &str, offset: u64, output: &mut [u8]) -> Result<usize, String> {
    default_resources().read_range(path, offset, output)
}

pub fn write(path: &str, data: &[u8]) -> Result<(), String> {
    default_resources().write(path, data)
}

pub fn delete(path: &str) -> Result<(), String> {
    default_resources().delete(path)
}

pub fn file_mtime(path: &str) -> Option<[i64; 6]> {
    default_resources().file_mtime(path)
}

/// A decoder-facing random-access source over the currently mounted file host.
pub struct HostFileSource {
    path: String,
    resources: HostResources,
}

impl MediaSource for HostFileSource {
    fn len(&self) -> Result<u64, String> {
        self.resources
            .query_size(&self.path)?
            .ok_or_else(|| format!("not found: {}", self.path))
    }

    fn read_at(&self, offset: u64, output: &mut [u8]) -> Result<usize, String> {
        self.resources.read_range(&self.path, offset, output)
    }
}

/// Resolve a logical media path against the active directory/PFS mount.
pub fn open_media_source(path: &str) -> Result<Arc<dyn MediaSource>, String> {
    default_resources().open_media_source(path)
}

unsafe fn c_path<'a>(path: *const c_char) -> Result<&'a Path, String> {
    if path.is_null() {
        return Err("null path".to_string());
    }
    unsafe { CStr::from_ptr(path) }
        .to_str()
        .map(Path::new)
        .map_err(|error| format!("path is not UTF-8: {error}"))
}

unsafe fn resources_ref<'a>(resources: *mut HostResources) -> Result<&'a HostResources, String> {
    if resources.is_null() {
        return Err("null resource handle".to_string());
    }
    Ok(unsafe { &*resources })
}

pub unsafe extern "C" fn art3m1s_resources_create() -> *mut HostResources {
    Box::into_raw(Box::new(HostResources::new()))
}

pub unsafe extern "C" fn art3m1s_resources_destroy(resources: *mut HostResources) {
    if !resources.is_null() {
        drop(unsafe { Box::from_raw(resources) });
    }
}

pub unsafe extern "C" fn art3m1s_resources_clear(resources: *mut HostResources) {
    if let Ok(resources) = unsafe { resources_ref(resources) } {
        resources.clear();
    }
}

pub unsafe extern "C" fn art3m1s_resources_mount_directory(
    resources: *mut HostResources,
    path: *const c_char,
) -> c_int {
    let result = (|| {
        let resources = unsafe { resources_ref(resources) }?;
        let path = unsafe { c_path(path) }?;
        resources.mount_directory(path)
    })();
    match result {
        Ok(()) => 1,
        Err(error) => {
            crate::core_warn!("resources_mount_directory: {error}");
            0
        }
    }
}

pub unsafe extern "C" fn art3m1s_resources_mount_pfs(
    resources: *mut HostResources,
    path: *const c_char,
    encoding: *const c_char,
) -> c_int {
    let result = (|| {
        let resources = unsafe { resources_ref(resources) }?;
        let path = unsafe { c_path(path) }?;
        let encoding = if encoding.is_null() {
            "utf-8"
        } else {
            unsafe { CStr::from_ptr(encoding) }
                .to_str()
                .map_err(|error| format!("encoding is not UTF-8: {error}"))?
        };
        resources.mount_pfs(path, encoding)
    })();
    match result {
        Ok(()) => 1,
        Err(error) => {
            crate::core_warn!("resources_mount_pfs: {error}");
            0
        }
    }
}

pub unsafe extern "C" fn art3m1s_resources_set_save_dir(
    resources: *mut HostResources,
    path: *const c_char,
) -> c_int {
    let result = (|| {
        let resources = unsafe { resources_ref(resources) }?;
        if path.is_null() {
            resources.set_save_dir(None)
        } else {
            let path = unsafe { c_path(path) }?;
            resources.set_save_dir(Some(path))
        }
    })();
    match result {
        Ok(()) => 1,
        Err(error) => {
            crate::core_warn!("resources_set_save_dir: {error}");
            0
        }
    }
}

pub unsafe extern "C" fn art3m1s_resources_set_override(
    resources: *mut HostResources,
    path: *const c_char,
    data: *const u8,
    len: usize,
) -> c_int {
    if data.is_null() && len != 0 {
        return 0;
    }
    let bytes = if len == 0 {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(data, len) }.to_vec()
    };
    let result = (|| {
        let resources = unsafe { resources_ref(resources) }?;
        let path = unsafe { c_path(path) }?;
        resources.set_override(&path.to_string_lossy(), bytes)
    })();
    match result {
        Ok(()) => 1,
        Err(error) => {
            crate::core_warn!("resources_set_override: {error}");
            0
        }
    }
}

pub unsafe extern "C" fn art3m1s_resources_clear_overrides(resources: *mut HostResources) {
    if let Ok(resources) = unsafe { resources_ref(resources) } {
        resources.clear_overrides();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal unencrypted PF6 fixture, enough to exercise PFS mounting.
    fn build_pf6(files: &[(&[u8], &[u8])]) -> Vec<u8> {
        let mut entries = Vec::new();
        for (name, _) in files {
            entries.push(4 + name.len() + 4 + 4 + 4);
        }
        let entries_len: usize = entries.iter().sum();
        let count = files.len();
        let index_size = (4 + entries_len + 4 + 8 * (count + 1) + 4) as u32;
        let data_start = 7 + index_size as usize;

        let mut out = Vec::new();
        out.extend_from_slice(b"pf6");
        out.extend_from_slice(&index_size.to_le_bytes());
        out.extend_from_slice(&(count as u32).to_le_bytes());
        let mut offset = data_start as u32;
        for (name, data) in files {
            out.extend_from_slice(&(name.len() as u32).to_le_bytes());
            out.extend_from_slice(name);
            out.extend_from_slice(&[0; 4]);
            out.extend_from_slice(&offset.to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            offset += data.len() as u32;
        }
        out.extend_from_slice(&((count + 1) as u32).to_le_bytes());
        out.extend_from_slice(&vec![0u8; 8 * (count + 1)]);
        out.extend_from_slice(&(7u32 + 4 + entries_len as u32).to_le_bytes());
        for (_, data) in files {
            out.extend_from_slice(data);
        }
        out
    }

    #[test]
    fn directory_mount_supports_overrides_and_save_precedence() {
        let root = std::env::temp_dir().join(format!("art3m1s-host-files-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("data")).unwrap();
        fs::write(root.join("data/value.txt"), b"resource").unwrap();

        let resources = HostResources::new();
        resources.mount_directory(&root).unwrap();
        assert_eq!(resources.read_file("DATA\\VALUE.TXT").unwrap(), b"resource");

        resources
            .set_override("data/value.txt", b"override".to_vec())
            .unwrap();
        assert_eq!(resources.read_file("data/value.txt").unwrap(), b"override");

        let save = root.join("save");
        resources.set_save_dir(Some(&save)).unwrap();
        resources.write("data/value.txt", b"save").unwrap();
        assert_eq!(resources.read_file("data/value.txt").unwrap(), b"save");
        resources.delete("data/value.txt").unwrap();
        assert_eq!(resources.read_file("data/value.txt").unwrap(), b"override");

        resources.clear();
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn pfs_mount_ignores_invalid_sibling_candidates() {
        let root = std::env::temp_dir().join(format!("art3m1s-host-pfs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        let archive = root.join("game.pfs");
        fs::write(
            &archive,
            build_pf6(&[(b"system.ini", b"[WINDOWS]\r\nWIDTH=1280\r\n")]),
        )
        .unwrap();
        fs::write(root.join("._game.pfs.001"), b"AppleDouble metadata").unwrap();
        fs::write(root.join("game.pfs.001"), b"not a PFS archive").unwrap();

        let resources = HostResources::new();
        resources.mount_pfs(&archive, "utf-8").unwrap();
        assert_eq!(
            resources.read_file("system.ini").unwrap(),
            b"[WINDOWS]\r\nWIDTH=1280\r\n"
        );

        resources.clear();
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn pfs_mount_reads_sidecar_files_after_archive_entries() {
        let root =
            std::env::temp_dir().join(format!("art3m1s-host-sidecar-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("movie")).unwrap();
        fs::write(root.join("movie/logo.dat"), b"sidecar logo").unwrap();
        fs::write(root.join("movie/opening.dat"), b"sidecar opening").unwrap();

        let archive = root.join("game.pfs");
        fs::write(
            &archive,
            build_pf6(&[
                (b"system.ini", b"[WINDOWS]\r\nWIDTH=1280\r\n"),
                (b"movie/logo.dat", b"archive logo"),
            ]),
        )
        .unwrap();

        let resources = HostResources::new();
        resources.mount_pfs(&archive, "utf-8").unwrap();
        assert_eq!(
            resources.read_file("system.ini").unwrap(),
            b"[WINDOWS]\r\nWIDTH=1280\r\n"
        );
        // Loose files beside the archive override archive entries.
        assert_eq!(
            resources.read_file("movie/logo.dat").unwrap(),
            b"sidecar logo"
        );
        assert_eq!(
            resources.read_file("movie/opening.dat").unwrap(),
            b"sidecar opening"
        );

        resources.clear();
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn pfs_patch_volumes_override_lower_volumes() {
        let root =
            std::env::temp_dir().join(format!("art3m1s-host-pfs-order-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        let archive = root.join("game.pfs");
        fs::write(
            &archive,
            build_pf6(&[(b"system.ini", b"base"), (b"only-base.txt", b"base only")]),
        )
        .unwrap();
        fs::write(
            root.join("game.pfs.001"),
            build_pf6(&[
                (b"system.ini", b"patch 001"),
                (b"only-001.txt", b"001 only"),
            ]),
        )
        .unwrap();
        fs::write(
            root.join("game.pfs.002"),
            build_pf6(&[
                (b"system.ini", b"patch 002"),
                (b"only-002.txt", b"002 only"),
            ]),
        )
        .unwrap();

        let resources = HostResources::new();
        resources.mount_pfs(&archive, "utf-8").unwrap();
        assert_eq!(resources.read_file("system.ini").unwrap(), b"patch 002");
        assert_eq!(resources.read_file("only-base.txt").unwrap(), b"base only");
        assert_eq!(resources.read_file("only-001.txt").unwrap(), b"001 only");
        assert_eq!(resources.read_file("only-002.txt").unwrap(), b"002 only");

        resources.clear();
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn media_source_reads_mounted_resources() {
        let root =
            std::env::temp_dir().join(format!("art3m1s-host-media-source-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("movie")).unwrap();
        fs::write(root.join("movie/opening.bin"), b"0123456789").unwrap();

        let resources = HostResources::new();
        resources.mount_directory(&root).unwrap();
        let source = resources.open_media_source(r"movie\opening.bin").unwrap();
        assert_eq!(source.len().unwrap(), 10);
        let mut output = [0u8; 4];
        assert_eq!(source.read_at(3, &mut output).unwrap(), 4);
        assert_eq!(&output, b"3456");

        resources.clear();
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn resource_handles_keep_mounts_isolated() {
        let root_a =
            std::env::temp_dir().join(format!("art3m1s-resource-a-{}", std::process::id()));
        let root_b =
            std::env::temp_dir().join(format!("art3m1s-resource-b-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root_a);
        let _ = fs::remove_dir_all(&root_b);
        fs::create_dir_all(&root_a).unwrap();
        fs::create_dir_all(&root_b).unwrap();
        fs::write(root_a.join("value.txt"), b"a").unwrap();
        fs::write(root_b.join("value.txt"), b"b").unwrap();

        let resources_a = HostResources::new();
        let resources_b = HostResources::new();
        resources_a.mount_directory(&root_a).unwrap();
        resources_b.mount_directory(&root_b).unwrap();

        assert_eq!(resources_a.read_file("value.txt").unwrap(), b"a");
        assert_eq!(resources_b.read_file("value.txt").unwrap(), b"b");

        resources_a.clear();
        assert!(resources_a.query_size("value.txt").is_err());
        assert_eq!(resources_b.read_file("value.txt").unwrap(), b"b");

        let _ = fs::remove_dir_all(&root_a);
        let _ = fs::remove_dir_all(&root_b);
    }
}
