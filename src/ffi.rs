//! C FFI bridge — all host ↔ core communication.
//!
//! The Flutter frontend registers callbacks at startup; afterwards every
//! filesystem operation inside the core is routed through those callbacks,
//! keeping the core entirely free of direct I/O.

use std::ffi::{c_char, c_int, c_longlong, CString};
use std::sync::OnceLock;

// ── File reader callback ────────────────────────────────────────

type FileReaderCallback = unsafe extern "C" fn(
    path: *const c_char,
    buf: *mut u8,
    buf_size: c_int,
    offset: c_longlong,
) -> c_int;

static FILE_READER: OnceLock<FileReaderCallback> = OnceLock::new();

#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_register_file_reader(cb: FileReaderCallback) {
    let _ = FILE_READER.set(cb);
}

pub fn file_reader_registered() -> bool {
    FILE_READER.get().is_some()
}

// ── Save directory ───────────────────────────────────────────────

static SAVE_DIR: OnceLock<String> = OnceLock::new();

#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_set_save_dir(dir: *const c_char) {
    if let Ok(s) = unsafe { std::ffi::CStr::from_ptr(dir).to_str() } {
        let _ = SAVE_DIR.set(s.to_string());
    }
}

pub fn save_dir() -> Option<&'static str> {
    SAVE_DIR.get().map(|s| s.as_str())
}

// ── Query helpers ────────────────────────────────────────────────

fn query_size(path: &str) -> Option<u64> {
    let cb = FILE_READER.get()?;
    let c_path = CString::new(path).ok()?;
    let size = unsafe { cb(c_path.as_ptr(), std::ptr::null_mut(), 0, -1) };
    if size < 0 { None } else { Some(size as u64) }
}

fn read_chunk(path: &str, offset: u64, buf: &mut [u8]) -> Option<usize> {
    let cb = FILE_READER.get()?;
    let c_path = CString::new(path).ok()?;
    let n = unsafe {
        cb(
            c_path.as_ptr(),
            buf.as_mut_ptr(),
            buf.len() as c_int,
            offset as c_longlong,
        )
    };
    if n < 0 { None } else { Some(n as usize) }
}

const CHUNK: usize = 65536;
const MAX_SINGLE: u64 = 16 * 1024 * 1024;

pub fn request_file(path: &str) -> Result<Vec<u8>, String> {
    let total = query_size(path).ok_or_else(|| format!("not found: {path}"))?;
    if total == 0 { return Ok(Vec::new()); }
    if total <= MAX_SINGLE {
        let mut buf = vec![0u8; total as usize];
        let n = read_chunk(path, 0, &mut buf).unwrap_or(0);
        buf.truncate(n);
        return Ok(buf);
    }
    let mut buf = Vec::with_capacity(total as usize);
    let mut off = 0u64;
    while off < total {
        let take = ((total - off) as usize).min(CHUNK);
        let mut chunk = vec![0u8; take];
        let n = read_chunk(path, off, &mut chunk).unwrap_or(0);
        if n == 0 { break; }
        buf.extend_from_slice(&chunk[..n]);
        off += n as u64;
    }
    Ok(buf)
}

pub fn request_file_range(path: &str, offset: u64, len: usize) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; len];
    let n = read_chunk(path, offset, &mut buf)?;
    buf.truncate(n);
    Some(buf)
}

pub fn request_asset(path: &str) -> Option<Vec<u8>> { request_file(path).ok() }
pub fn request_asset_range(path: &str, offset: u64, len: usize) -> Option<Vec<u8>> {
    request_file_range(path, offset, len)
}
pub fn query_asset_size(path: &str) -> Option<u64> { query_size(path) }

// ── File operations ──────────────────────────────────────────────

#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_file_exists(path: *const c_char) -> c_int {
    if path.is_null() { return 0; }
    let Ok(s) = (unsafe { std::ffi::CStr::from_ptr(path).to_str() }) else { return 0 };
    if query_size(s).is_some() { 1 } else { 0 }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_copy_file(
    src: *const c_char,
    dst: *const c_char,
) -> c_int {
    if src.is_null() || dst.is_null() { return -1; }
    let Ok(s) = (unsafe { std::ffi::CStr::from_ptr(src).to_str() }) else { return -1 };
    let Ok(_d) = (unsafe { std::ffi::CStr::from_ptr(dst).to_str() }) else { return -1 };
    let _data = match request_file(s) {
        Ok(v) => v,
        Err(_) => return -1,
    };
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_delete_file(path: *const c_char) -> c_int {
    if path.is_null() { return -1; }
    0
}
