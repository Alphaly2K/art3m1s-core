use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use rfvp::host_api::{
    RfvpAssetFile, RfvpAssetSource, RfvpError, RfvpFile, RfvpFileInfo, RfvpFileKind, RfvpResult,
    RfvpSaveStore,
};

fn io_error(error: io::Error) -> RfvpError {
    if error.kind() == io::ErrorKind::NotFound {
        RfvpError::NotFound
    } else {
        RfvpError::Io
    }
}

pub(super) struct DirectoryAssets(PathBuf);

impl DirectoryAssets {
    pub fn new(root: &Path) -> RfvpResult<Self> {
        let root = root.canonicalize().map_err(io_error)?;
        if !root.is_dir() {
            return Err(RfvpError::InvalidArgument);
        }
        Ok(Self(root))
    }

    fn resolve(&self, key: &str) -> RfvpResult<PathBuf> {
        let path = Path::new(key);
        if key.contains(['\\', ':'])
            || path
                .components()
                .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
        {
            return Err(RfvpError::InvalidArgument);
        }
        within_root(&self.0, &self.0.join(path))
    }
}

fn within_root(root: &Path, path: &Path) -> RfvpResult<PathBuf> {
    let path = path.canonicalize().map_err(io_error)?;
    if !path.starts_with(root) {
        return Err(RfvpError::InvalidArgument);
    }
    Ok(path)
}

struct AssetFile(File);

impl RfvpFile for AssetFile {
    fn len(&mut self) -> RfvpResult<u64> {
        Ok(self.0.metadata().map_err(io_error)?.len())
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> RfvpResult<usize> {
        self.0.seek(SeekFrom::Start(offset)).map_err(io_error)?;
        self.0.read(buf).map_err(io_error)
    }
}

impl RfvpAssetSource for DirectoryAssets {
    fn open(&self, path: &str) -> RfvpResult<RfvpAssetFile> {
        let path = self.resolve(path)?;
        // A directory with the same name as a pack entry is not a loose override.
        if !path.is_file() {
            return Err(RfvpError::NotFound);
        }
        Ok(Box::new(AssetFile(File::open(path).map_err(io_error)?)))
    }

    fn metadata(&self, path: &str) -> RfvpResult<RfvpFileInfo> {
        let metadata = fs::metadata(self.resolve(path)?).map_err(io_error)?;
        Ok(RfvpFileInfo {
            len: metadata.len(),
            kind: if metadata.is_file() {
                RfvpFileKind::File
            } else if metadata.is_dir() {
                RfvpFileKind::Directory
            } else {
                RfvpFileKind::Other
            },
        })
    }

    fn enumerate(
        &self,
        root: &str,
        extension: &str,
        visitor: &mut dyn FnMut(&str, RfvpFileInfo) -> RfvpResult<()>,
    ) -> RfvpResult<()> {
        let mut pending = vec![self.resolve(root)?];
        while let Some(dir) = pending.pop() {
            for entry in fs::read_dir(dir).map_err(io_error)? {
                let entry = entry.map_err(io_error)?;
                let kind = entry.file_type().map_err(io_error)?;
                let path = entry.path();
                if kind.is_dir() {
                    pending.push(path);
                } else if kind.is_file()
                    && path
                        .extension()
                        .and_then(|ext| ext.to_str())
                        .is_some_and(|ext| ext.eq_ignore_ascii_case(extension))
                {
                    let key = path
                        .strip_prefix(&self.0)
                        .map_err(|_| RfvpError::InvalidArgument)?;
                    if let Some(key) = key.to_str() {
                        let key = key.replace(std::path::MAIN_SEPARATOR, "/");
                        visitor(
                            &key,
                            RfvpFileInfo::file(entry.metadata().map_err(io_error)?.len()),
                        )?;
                    }
                }
            }
        }
        Ok(())
    }
}

pub(super) struct DirectorySaves(pub PathBuf);

fn save_key(key: &str) -> RfvpResult<&Path> {
    let path = Path::new(key);
    if key.contains(['/', '\\', ':'])
        || !matches!(path.components().next(), Some(Component::Normal(_)))
    {
        return Err(RfvpError::InvalidArgument);
    }
    Ok(path)
}

impl RfvpSaveStore for DirectorySaves {
    fn read(&self, key: &str) -> RfvpResult<Vec<u8>> {
        let key = save_key(key)?;
        let root = self.0.canonicalize().map_err(io_error)?;
        fs::read(within_root(&root, &root.join(key))?).map_err(io_error)
    }

    fn write(&self, key: &str, bytes: &[u8]) -> RfvpResult<()> {
        let key = save_key(key)?;
        fs::create_dir_all(&self.0).map_err(io_error)?;
        let root = self.0.canonicalize().map_err(io_error)?;
        atomic_replace(&root, &root.join(key), bytes).map_err(io_error)
    }

    fn remove(&self, key: &str) -> RfvpResult<()> {
        fs::remove_file(self.0.join(save_key(key)?)).map_err(io_error)
    }

    fn list(&self) -> RfvpResult<Vec<String>> {
        let entries = match fs::read_dir(&self.0) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(io_error(error)),
        };
        let mut keys = Vec::new();
        for entry in entries {
            let entry = entry.map_err(io_error)?;
            if entry.file_type().map_err(io_error)?.is_file()
                && let Ok(key) = entry.file_name().into_string()
            {
                keys.push(key);
            }
        }
        Ok(keys)
    }
}

fn atomic_replace(root: &Path, target: &Path, bytes: &[u8]) -> io::Result<()> {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let (temp, mut file) = loop {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = root.join(format!(".rfvp-{}-{n}.tmp", std::process::id()));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => break (path, file),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    };
    let result = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    let result = result.and_then(|()| fs::rename(&temp, target));
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}
