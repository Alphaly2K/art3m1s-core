//! Artemis PFS 归档访问层（C ABI）。
//!
//! 本 crate 历史上是另一 GPL 实现的跨语言重写；现实现已整体替换为 vendored
//! pf8（MIT，见 `crates/pf8`），本包装层为 MIT 下新写的 FFI 胶合代码。
//! dylib 名（`libpfs_upk`）与导出符号集保持兼容，宿主无需改动。
//!
//! 语义要点：
//! - 条目路径在打开时按指定编码解码为 UTF-8（`pfs_open` 缺省 UTF-8，
//!   `pfs_open_with_encoding` 由宿主按游戏语言环境传入，如 Shift_JIS/GBK）。
//! - 条目查找大小写不敏感、`\` 与 `/` 等价（pf8 侧统一）。
//! - 支持分卷归档（`root.pfs` + `root.pfs.000`… 串联读取）。

mod split;

use std::ffi::{c_char, c_int, CStr};
use std::path::{Path, PathBuf};

use pf8::Pf8Reader;
use split::VolumeReader;

/// 打开的归档句柄（对宿主不透明）。
pub struct PfsArchiveHandle {
    reader: Pf8Reader,
    /// 条目路径（UTF-8），打开时解码并缓存。
    paths: Vec<String>,
}

fn open_handle(
    path: &Path,
    encoding: &'static encoding_rs::Encoding,
) -> Option<Box<PfsArchiveHandle>> {
    let volumes = VolumeReader::open(path).ok()?;
    let reader = Pf8Reader::open_reader_with_encoding(Box::new(volumes), Some(encoding)).ok()?;
    let paths = reader
        .entries()
        .map(|entry| entry.path().to_string_lossy().into_owned())
        .collect();
    Some(Box::new(PfsArchiveHandle { reader, paths }))
}

fn encoding_from_name(name: &str) -> &'static encoding_rs::Encoding {
    match name.to_ascii_lowercase().as_str() {
        "shift_jis" | "shift-jis" | "sjis" | "cp932" => encoding_rs::SHIFT_JIS,
        "gbk" | "gb2312" | "gb18030" => encoding_rs::GBK,
        "big5" => encoding_rs::BIG5,
        "euc-jp" | "eucjp" => encoding_rs::EUC_JP,
        "utf-16le" => encoding_rs::UTF_16LE,
        "utf-16be" => encoding_rs::UTF_16BE,
        _ => encoding_rs::UTF_8,
    }
}

unsafe fn read_cstr<'a>(ptr: *const c_char) -> Option<&'a str> {
    if ptr.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(ptr) }.to_str().ok()
}

/// Open an archive (with split volumes), entry names decoded as UTF-8.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pfs_open(path: *const c_char) -> *mut PfsArchiveHandle {
    let Some(path) = (unsafe { read_cstr(path) }) else {
        return std::ptr::null_mut();
    };
    open_handle(Path::new(path), encoding_rs::UTF_8)
        .map(Box::into_raw)
        .unwrap_or(std::ptr::null_mut())
}

/// Open an archive with an explicit entry-name encoding.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pfs_open_with_encoding(
    path: *const c_char,
    encoding: *const c_char,
) -> *mut PfsArchiveHandle {
    let (Some(path), Some(encoding)) = (unsafe { read_cstr(path) }, unsafe { read_cstr(encoding) })
    else {
        return std::ptr::null_mut();
    };
    open_handle(Path::new(path), encoding_from_name(encoding))
        .map(Box::into_raw)
        .unwrap_or(std::ptr::null_mut())
}

/// Get the size of a file inside the archive, or -1 if not found.
/// 条目大小按 u32 存储；本签名是历史 ABI（i32），超过 2 GiB 的条目会截断。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pfs_file_size(
    archive: *mut PfsArchiveHandle,
    path: *const c_char,
) -> c_int {
    if archive.is_null() {
        return -1;
    }
    let Some(path) = (unsafe { read_cstr(path) }) else {
        return -1;
    };
    let archive = unsafe { &*archive };
    match archive.reader.get_entry(path) {
        Some(entry) => entry.size() as i32,
        None => -1,
    }
}

/// Return the number of entries in the archive.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pfs_entry_count(archive: *mut PfsArchiveHandle) -> c_int {
    if archive.is_null() {
        return -1;
    }
    let archive = unsafe { &*archive };
    archive.paths.len() as c_int
}

/// Copy the i-th entry's path (UTF-8) into `buf` (including NUL at most
/// `buf_size` bytes). Returns bytes written (excluding NUL), or -1 if `index`
/// is out of range. Truncates when the buffer is too small.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pfs_entry_path(
    archive: *mut PfsArchiveHandle,
    index: c_int,
    buf: *mut c_char,
    buf_size: c_int,
) -> c_int {
    if archive.is_null() || buf.is_null() || index < 0 || buf_size <= 0 {
        return -1;
    }
    let archive = unsafe { &*archive };
    let Some(path) = archive.paths.get(index as usize) else {
        return -1;
    };
    let bytes = path.as_bytes();
    let limit = (buf_size as usize - 1).min(bytes.len());
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf as *mut u8, limit);
        *buf.add(limit) = 0;
    }
    limit as c_int
}

/// Get the size of the i-th entry, or -1 if out of range.
/// O(1) 直下标读取，供宿主按条目建索引；比逐条目 `pfs_file_size`（路径查找）
/// 快一个量级。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pfs_entry_size(archive: *mut PfsArchiveHandle, index: c_int) -> i64 {
    if archive.is_null() || index < 0 {
        return -1;
    }
    let archive = unsafe { &*archive };
    archive
        .reader
        .entries()
        .nth(index as usize)
        .map(|entry| entry.size() as i64)
        .unwrap_or(-1)
}

/// Read up to `buf_size` bytes from a file inside the archive, starting at
/// `offset` (relative to the file's data). Returns bytes read, or -1 on error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pfs_read(
    archive: *mut PfsArchiveHandle,
    path: *const c_char,
    offset: u64,
    buf: *mut u8,
    buf_size: u32,
) -> c_int {
    if archive.is_null() || path.is_null() || buf.is_null() || buf_size == 0 {
        return -1;
    }
    let Some(path) = (unsafe { read_cstr(path) }) else {
        return -1;
    };
    let archive = unsafe { &mut *archive };
    let out = unsafe { std::slice::from_raw_parts_mut(buf, buf_size as usize) };
    match archive.reader.read_range(path, offset, out) {
        Ok(read) => read as c_int,
        Err(_) => -1,
    }
}

/// Close an archive and free its memory.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pfs_close(archive: *mut PfsArchiveHandle) {
    if !archive.is_null() {
        unsafe { drop(Box::from_raw(archive)) };
    }
}

/// Legacy bulk unpack: extract the whole archive to `output_dir`.
/// Returns 0 on success, -1 on error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pfs_unpack(
    archive_path: *const c_char,
    output_dir: *const c_char,
) -> c_int {
    let (Some(archive_path), Some(output_dir)) = (unsafe { read_cstr(archive_path) }, unsafe {
        read_cstr(output_dir)
    }) else {
        return -1;
    };
    let Some(mut handle) = open_handle(Path::new(archive_path), encoding_rs::UTF_8) else {
        return -1;
    };
    match handle.reader.extract_all(PathBuf::from(output_dir)) {
        Ok(()) => 0,
        Err(_) => -1,
    }
}
