//! 分卷读取：把 `base.pfs`、`base.pfs.000`、`base.pfs.001` … 串联成一个逻辑
//! 字节流。分卷归档的头部与索引在基卷内，条目数据偏移相对串联后的整体流。

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

pub struct VolumeReader {
    volumes: Vec<File>,
    /// 各卷在逻辑流中的起始偏移（单调递增）。
    offsets: Vec<u64>,
    total: u64,
    pos: u64,
}

impl VolumeReader {
    /// 打开基卷并自动发现后续分卷（`<base>.000`、`.001`…，编号连续为止）。
    pub fn open(base: &Path) -> io::Result<Self> {
        let mut volumes = Vec::new();
        let mut offsets = Vec::new();
        let mut total = 0u64;
        let mut path = base.to_path_buf();
        loop {
            let file = File::open(&path)?;
            offsets.push(total);
            total += file.metadata()?.len();
            volumes.push(file);
            let mut next = base.as_os_str().to_owned();
            next.push(format!(".{:03}", volumes.len() - 1));
            path = PathBuf::from(next);
            if !path.is_file() {
                break;
            }
        }
        Ok(Self {
            volumes,
            offsets,
            total,
            pos: 0,
        })
    }

    fn volume_at(&self, pos: u64) -> usize {
        match self.offsets.binary_search(&pos) {
            Ok(index) => index,
            Err(0) => 0,
            Err(index) => index - 1,
        }
    }

    fn volume_end(&self, index: usize) -> u64 {
        self.offsets.get(index + 1).copied().unwrap_or(self.total)
    }
}

impl Read for VolumeReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || self.pos >= self.total {
            return Ok(0);
        }
        let index = self.volume_at(self.pos);
        let local = self.pos - self.offsets[index];
        // 单次读不跨卷；read_exact 的调用方会循环补齐。
        let limit = (self.volume_end(index) - self.pos).min(buf.len() as u64) as usize;
        let file = &mut self.volumes[index];
        file.seek(SeekFrom::Start(local))?;
        let read = file.read(&mut buf[..limit])?;
        self.pos += read as u64;
        Ok(read)
    }
}

impl Seek for VolumeReader {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let next = match pos {
            SeekFrom::Start(value) => value as i128,
            SeekFrom::Current(delta) => self.pos as i128 + delta as i128,
            SeekFrom::End(delta) => self.total as i128 + delta as i128,
        };
        if next < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek to a negative position",
            ));
        }
        self.pos = (next as u64).min(self.total);
        Ok(self.pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn chained_volumes_read_across_boundaries() {
        let dir = std::env::temp_dir().join(format!("pfs_split_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let base = dir.join("root.pfs");
        // 基卷 5 字节 + 两个分卷各 4 字节
        File::create(&base).unwrap().write_all(b"01234").unwrap();
        File::create(dir.join("root.pfs.000"))
            .unwrap()
            .write_all(b"5678")
            .unwrap();
        File::create(dir.join("root.pfs.001"))
            .unwrap()
            .write_all(b"9abc")
            .unwrap();

        let mut reader = VolumeReader::open(&base).unwrap();
        let mut all = Vec::new();
        reader.read_to_end(&mut all).unwrap();
        assert_eq!(all, b"0123456789abc");

        // 跨卷边界的精确定位读取
        reader.seek(SeekFrom::Start(3)).unwrap();
        let mut buf = [0u8; 6];
        reader.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"345678");

        // 越界位置截断到末尾
        assert_eq!(reader.seek(SeekFrom::Start(999)).unwrap(), 13);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
