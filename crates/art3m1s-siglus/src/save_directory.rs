use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, bail};

use crate::SiglusSaveStore;

pub struct DirectorySaves {
    root: PathBuf,
}

impl DirectorySaves {
    pub fn open(root: &Path, legacy: &Path) -> Result<Self> {
        fs::create_dir_all(root).context("create Siglus save directory")?;
        let saves = Self {
            root: root.canonicalize()?,
        };
        saves.import_legacy(legacy)?;
        Ok(saves)
    }

    fn path(&self, key: &str) -> Result<PathBuf> {
        if key.contains(['/', '\\', ':', '\0'])
            || !matches!(
                Path::new(key).components().next(),
                Some(Component::Normal(_))
            )
        {
            bail!("invalid Siglus save key");
        }
        Ok(self.root.join(key))
    }

    fn import_legacy(&self, legacy: &Path) -> Result<()> {
        if self.root.join(".siglus-imported").exists() {
            return Ok(());
        }
        let legacy = match legacy.canonicalize() {
            Ok(path) if path == self.root => return Ok(()),
            Ok(path) => Some(path),
            Err(err) if err.kind() == io::ErrorKind::NotFound => None,
            Err(err) => return Err(err.into()),
        };
        if let Some(legacy) = legacy {
            for entry in fs::read_dir(legacy).context("open previous Siglus save directory")? {
                let entry = entry?;
                let Some(key) = entry.file_name().into_string().ok() else {
                    continue;
                };
                let key = key.to_ascii_lowercase();
                if entry.file_type()?.is_file()
                    && is_legacy_save(&key)
                    && !self.path(&key)?.exists()
                {
                    self.write(&key, &fs::read(entry.path())?)?;
                }
            }
        }
        self.write(".siglus-imported", b"1")
    }
}

fn is_legacy_save(key: &str) -> bool {
    if matches!(key, "global.sav" | "config.sav" | "read.sav") {
        return true;
    }
    key.split_once('.').is_some_and(|(stem, extension)| {
        !stem.is_empty()
            && stem.bytes().all(|digit| digit.is_ascii_digit())
            && matches!(extension, "sav" | "png" | "bmp")
    })
}

impl SiglusSaveStore for DirectorySaves {
    fn read(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let path = match self.path(key)?.canonicalize() {
            Ok(path) if path.starts_with(&self.root) => path,
            Ok(_) => bail!("Siglus save is outside its directory"),
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err.into()),
        };
        Ok(Some(fs::read(path)?))
    }

    fn write(&self, key: &str, bytes: &[u8]) -> Result<()> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let target = self.path(key)?;
        let (temp, mut file) = loop {
            let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
            let temp = self
                .root
                .join(format!(".siglus-{}-{sequence}.tmp", std::process::id()));
            match OpenOptions::new().write(true).create_new(true).open(&temp) {
                Ok(file) => break (temp, file),
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(err) => return Err(err.into()),
            }
        };
        let result = file.write_all(bytes).and_then(|()| file.sync_all());
        drop(file);
        let result = result.and_then(|()| fs::rename(&temp, target));
        if result.is_err() {
            let _ = fs::remove_file(temp);
        }
        result.context("replace Siglus save")
    }

    fn remove(&self, key: &str) -> Result<()> {
        match fs::remove_file(self.path(key)?) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err.into()),
        }
    }
}
