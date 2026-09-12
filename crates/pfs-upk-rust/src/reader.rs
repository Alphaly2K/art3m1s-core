//! 面向 Rust 调用方的最小归档 API（core 的探针工具使用）。
//! 基于 pf8，语义与历史 pfs-upk 的 reader 模块对齐。

use std::io;
use std::path::Path;

use pf8::{Pf8Entry, Pf8Reader};

use crate::split::VolumeReader;

/// 打开的 PFS 归档（自动串联分卷）。
pub struct PfsArchive {
    reader: Pf8Reader,
}

impl PfsArchive {
    /// 打开归档（条目名按 UTF-8 解码）。
    pub fn open(path: &Path) -> io::Result<Self> {
        Self::open_with_encoding(path, encoding_rs::UTF_8)
    }

    /// 打开归档并指定条目名编码（如 Shift_JIS / GBK）。
    pub fn open_with_encoding(
        path: &Path,
        encoding: &'static encoding_rs::Encoding,
    ) -> io::Result<Self> {
        let volumes = VolumeReader::open(path)?;
        let reader = Pf8Reader::open_reader_with_encoding(Box::new(volumes), Some(encoding))
            .map_err(io::Error::other)?;
        Ok(Self { reader })
    }

    /// 按路径查找条目（大小写不敏感、`\` 与 `/` 等价）。
    pub fn find(&self, path: &str) -> Option<&Pf8Entry> {
        self.reader.get_entry(path)
    }

    /// 枚举归档条目，供宿主建立资源索引。
    pub fn entries(&self) -> impl Iterator<Item = &Pf8Entry> {
        self.reader.entries()
    }

    /// 从条目内偏移 `offset` 读取至多 `buf.len()` 字节，返回实际读取数。
    pub fn read_entry(
        &mut self,
        entry: &Pf8Entry,
        offset: u64,
        buf: &mut [u8],
    ) -> io::Result<usize> {
        let path = entry.path().to_string_lossy().into_owned();
        self.reader
            .read_range(&path, offset, buf)
            .map_err(io::Error::other)
    }
}
