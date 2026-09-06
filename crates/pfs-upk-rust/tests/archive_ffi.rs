//! 端到端 FFI 测试：加密 pf8 往返、手工 pf6/pf2 布局、分卷串联、范围读取、
//! 大小写不敏感查找与显式条目名编码。

use std::ffi::{c_char, CStr, CString};
use std::path::{Path, PathBuf};

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("pfs_upk_ffi_test_{}_{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn cstring(s: &str) -> CString {
    CString::new(s).unwrap()
}

unsafe fn entry_path(archive: *mut pfs_upk::PfsArchiveHandle, index: i32) -> String {
    let mut buf = [0u8; 4096];
    let n =
        unsafe { pfs_upk::pfs_entry_path(archive, index, buf.as_mut_ptr() as *mut c_char, 4096) };
    assert!(n > 0);
    unsafe { CStr::from_ptr(buf.as_mut_ptr() as *mut c_char) }
        .to_str()
        .unwrap()
        .to_string()
}

unsafe fn read_all(archive: *mut pfs_upk::PfsArchiveHandle, path: &str) -> Vec<u8> {
    let size = unsafe { pfs_upk::pfs_file_size(archive, cstring(path).as_ptr()) };
    assert!(size > 0, "file_size({path}) = {size}");
    let mut buf = vec![0u8; size as usize];
    let read = unsafe {
        pfs_upk::pfs_read(
            archive,
            cstring(path).as_ptr(),
            0,
            buf.as_mut_ptr(),
            size as u32,
        )
    };
    assert_eq!(read, size);
    buf
}

/// 手工构造一个 pf6 布局归档（未加密）：header + entries + 全零 trailer + 数据。
fn build_pf6(files: &[(&[u8], &[u8])]) -> Vec<u8> {
    let mut entries = Vec::new();
    for (name, _) in files {
        entries.push(4 + name.len() + 4 + 4 + 4);
    }
    let entries_len: usize = entries.iter().sum();
    let count = files.len();
    // index_size 从 0x07 起计：count(4) + entries + filesize_count(4) +
    // filesize_offsets(8*(n+1)) + filesize_count_offset(4)
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
        out.extend_from_slice(&[0; 4]); // reserved
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
fn encrypted_pf8_roundtrip_via_builder() {
    let temp = TempDir::new("pf8");
    let input = temp.0.join("input");
    std::fs::create_dir_all(input.join("sub")).unwrap();
    std::fs::write(input.join("hello.txt"), b"hello pf8").unwrap();
    std::fs::write(input.join("sub/Data.Bin"), b"\x01\x02\x03\x04").unwrap();
    let archive_path = temp.0.join("out.pfs");
    pf8::create_from_dir(&input, &archive_path).unwrap();

    let archive = unsafe { pfs_upk::pfs_open(cstring(archive_path.to_str().unwrap()).as_ptr()) };
    assert!(!archive.is_null());
    unsafe {
        assert_eq!(pfs_upk::pfs_entry_count(archive), 2);
        // 大小写不敏感 + 分隔符等价
        assert_eq!(
            pfs_upk::pfs_file_size(archive, cstring("HELLO.TXT").as_ptr()),
            9
        );
        assert_eq!(read_all(archive, "sub\\data.bin"), b"\x01\x02\x03\x04");
        // 范围读取：从条目内偏移 3 读 4 字节
        let mut buf = [0u8; 4];
        let read = pfs_upk::pfs_read(
            archive,
            cstring("hello.txt").as_ptr(),
            3,
            buf.as_mut_ptr(),
            4,
        );
        assert_eq!(read, 4);
        assert_eq!(&buf, b"lo p");
        // 读取越过条目末尾：截断
        let mut tail = [0u8; 8];
        let read = pfs_upk::pfs_read(
            archive,
            cstring("hello.txt").as_ptr(),
            7,
            tail.as_mut_ptr(),
            8,
        );
        assert_eq!(read, 2);
        assert_eq!(&tail[..2], b"f8");
        pfs_upk::pfs_close(archive);
    }
}

#[test]
fn manual_pf6_layout_reads_and_finds_case_insensitively() {
    let temp = TempDir::new("pf6");
    let bytes = build_pf6(&[
        (b"image/Title.PNG", &[10u8; 20][..]),
        (b"boot.iet", b"boot-content"),
    ]);
    let path = temp.0.join("root.pfs");
    std::fs::write(&path, &bytes).unwrap();

    let archive = unsafe { pfs_upk::pfs_open(cstring(path.to_str().unwrap()).as_ptr()) };
    assert!(!archive.is_null());
    unsafe {
        assert_eq!(pfs_upk::pfs_entry_count(archive), 2);
        assert_eq!(entry_path(archive, 0), "image/Title.PNG");
        assert_eq!(entry_path(archive, 1), "boot.iet");
        assert_eq!(
            pfs_upk::pfs_file_size(archive, cstring("IMAGE/title.png").as_ptr()),
            20
        );
        assert_eq!(read_all(archive, "boot.iet"), b"boot-content");
        pfs_upk::pfs_close(archive);
    }
}

#[test]
fn pf2_magic_is_accepted_as_unencrypted() {
    let temp = TempDir::new("pf2");
    let mut bytes = build_pf6(&[(b"a.txt", b"v2-format")]);
    bytes[2] = b'2'; // pf2
    let path = temp.0.join("old.pfs");
    std::fs::write(&path, &bytes).unwrap();

    let archive = unsafe { pfs_upk::pfs_open(cstring(path.to_str().unwrap()).as_ptr()) };
    assert!(!archive.is_null());
    unsafe {
        assert_eq!(read_all(archive, "a.txt"), b"v2-format");
        pfs_upk::pfs_close(archive);
    }
}

#[test]
fn split_volumes_read_across_boundary() {
    let temp = TempDir::new("split");
    let big = vec![7u8; 1000];
    let bytes = build_pf6(&[(b"big.bin", &big), (b"small.txt", b"small")]);
    // 在 big.bin 数据中间切开：基卷放 header+index+部分数据，余下进 .000。
    let cut = bytes.len() - 500;
    let base = temp.0.join("root.pfs");
    std::fs::write(&base, &bytes[..cut]).unwrap();
    std::fs::write(temp.0.join("root.pfs.000"), &bytes[cut..]).unwrap();

    let archive = unsafe { pfs_upk::pfs_open(cstring(base.to_str().unwrap()).as_ptr()) };
    assert!(!archive.is_null());
    unsafe {
        assert_eq!(read_all(archive, "big.bin"), big);
        assert_eq!(read_all(archive, "small.txt"), b"small");
        pfs_upk::pfs_close(archive);
    }
}

#[test]
fn explicit_shift_jis_encoding_decodes_entry_names() {
    let temp = TempDir::new("sjis");
    // "テスト.txt" 的 Shift_JIS 编码
    let name_sjis: &[u8] = b"\x83\x65\x83\x58\x83\x67.txt";
    let bytes = build_pf6(&[(name_sjis, b"sjis")]);
    let path = temp.0.join("sjis.pfs");
    std::fs::write(&path, &bytes).unwrap();

    let archive = unsafe {
        pfs_upk::pfs_open_with_encoding(
            cstring(path.to_str().unwrap()).as_ptr(),
            cstring("Shift_JIS").as_ptr(),
        )
    };
    assert!(!archive.is_null());
    unsafe {
        assert_eq!(entry_path(archive, 0), "テスト.txt");
        assert_eq!(read_all(archive, "テスト.txt"), b"sjis");
        pfs_upk::pfs_close(archive);
    }
}
