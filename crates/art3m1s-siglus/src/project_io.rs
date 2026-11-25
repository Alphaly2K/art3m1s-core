use std::ffi::{CString, c_char, c_void};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, bail, ensure};
use siglus_scene_vm::formats::{
    gameexe::GameexeDecodeOptions,
    key_toml::{KeyTomlConfig, parse_key_toml},
    keys::{GAMEEXE_KEY, SCENE_KEY},
    scene_pck::ScenePckDecodeOptions,
};
use siglus_scene_vm::resource::{resolve_game_file, resolve_game_path, with_project_files};

use crate::{
    SiglusAdapter, SiglusBootSource, SiglusGameexe, SiglusHostConfig, SiglusResourceSource,
    SiglusSaveStore,
};

const ASSET: u32 = 0;
const SAVE: u32 = 1;
const MISSING: i32 = 1;

type UserDataFn = unsafe extern "C" fn(*mut c_void);
type QueryFn = unsafe extern "C" fn(*mut c_void, u32, *const c_char, *mut u64) -> i32;
type ReadFn =
    unsafe extern "C" fn(*mut c_void, u32, *const c_char, u64, *mut u8, usize, *mut usize) -> i32;
type WriteSaveFn = unsafe extern "C" fn(*mut c_void, *const c_char, *const u8, usize) -> i32;
type RemoveSaveFn = unsafe extern "C" fn(*mut c_void, *const c_char) -> i32;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct SiglusProjectIoV1 {
    pub struct_size: u32,
    pub abi_version: u32,
    pub user_data: *mut c_void,
    pub retain: Option<UserDataFn>,
    pub release: Option<UserDataFn>,
    pub query_size: Option<QueryFn>,
    pub read_range: Option<ReadFn>,
    pub write_save: Option<WriteSaveFn>,
    pub remove_save: Option<RemoveSaveFn>,
}

impl SiglusProjectIoV1 {
    pub fn is_valid(&self) -> bool {
        self.struct_size as usize >= std::mem::size_of::<Self>()
            && self.abi_version == 1
            && self.retain.is_some()
            && self.release.is_some()
            && self.query_size.is_some()
            && self.read_range.is_some()
            && self.write_save.is_some()
            && self.remove_save.is_some()
    }
}

struct Files(SiglusProjectIoV1);

// The Host keeps this retained context alive and synchronizes its callbacks.
unsafe impl Send for Files {}
unsafe impl Sync for Files {}

impl Drop for Files {
    fn drop(&mut self) {
        unsafe { self.0.release.unwrap()(self.0.user_data) };
    }
}

impl Files {
    fn query(&self, namespace: u32, key: &str) -> Result<Option<u64>> {
        let input = CString::new(key)?;
        let mut size = 0;
        match unsafe {
            self.0.query_size.unwrap()(self.0.user_data, namespace, input.as_ptr(), &mut size)
        } {
            0 => Ok(Some(size)),
            MISSING => Ok(None),
            status => bail!("Siglus Host query {key}: {status}"),
        }
    }

    fn read_at(&self, namespace: u32, key: &str, offset: u64, output: &mut [u8]) -> Result<usize> {
        let input = CString::new(key)?;
        let mut bytes = 0;
        let status = unsafe {
            self.0.read_range.unwrap()(
                self.0.user_data,
                namespace,
                input.as_ptr(),
                offset,
                output.as_mut_ptr(),
                output.len(),
                &mut bytes,
            )
        };
        ensure!(status == 0, "Siglus Host read {key}: {status}");
        ensure!(bytes <= output.len(), "Siglus Host read exceeded capacity");
        Ok(bytes)
    }

    fn read_all(&self, namespace: u32, key: &str) -> Result<Option<Vec<u8>>> {
        let Some(size) = self.query(namespace, key)? else {
            return Ok(None);
        };
        let size = usize::try_from(size)?;
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(size)?;
        bytes.resize(size, 0);
        let mut offset = 0;
        while offset < size {
            let count = self.read_at(namespace, key, offset as u64, &mut bytes[offset..])?;
            ensure!(
                count != 0,
                "Siglus Host file ended before its reported size: {key}"
            );
            offset += count;
        }
        Ok(Some(bytes))
    }
}

impl SiglusResourceSource for Files {
    fn query_size(&self, key: &str) -> Result<Option<u64>> {
        self.query(ASSET, key)
    }

    fn read_range(&self, key: &str, offset: u64, output: &mut [u8]) -> Result<usize> {
        self.read_at(ASSET, key, offset, output)
    }
}

impl SiglusSaveStore for Files {
    fn read(&self, key: &str) -> Result<Option<Vec<u8>>> {
        self.read_all(SAVE, key)
    }

    fn write(&self, key: &str, bytes: &[u8]) -> Result<()> {
        let input = CString::new(key)?;
        let status = unsafe {
            self.0.write_save.unwrap()(
                self.0.user_data,
                input.as_ptr(),
                bytes.as_ptr(),
                bytes.len(),
            )
        };
        ensure!(status == 0, "Siglus Host write {key}: {status}");
        Ok(())
    }

    fn remove(&self, key: &str) -> Result<()> {
        let input = CString::new(key)?;
        let status = unsafe { self.0.remove_save.unwrap()(self.0.user_data, input.as_ptr()) };
        ensure!(
            status == 0 || status == MISSING,
            "Siglus Host remove {key}: {status}"
        );
        Ok(())
    }
}

struct Boot {
    files: Arc<Files>,
    append: String,
    keys: KeyTomlConfig,
}

impl Boot {
    fn file_key(append: &str, name: &str) -> Result<String> {
        if append.is_empty() {
            return Ok(name.to_string());
        }
        let append = append.replace('\\', "/");
        ensure!(
            !append.contains(':')
                && append
                    .split('/')
                    .all(|part| !matches!(part, "" | "." | "..")),
            "invalid Siglus append directory"
        );
        Ok(format!("{append}/{name}"))
    }
}

impl SiglusBootSource for Boot {
    fn read_gameexe(&self) -> Result<Option<SiglusGameexe>> {
        if let Some(bytes) = self
            .files
            .read_all(ASSET, &Self::file_key(&self.append, "Gameexe.dat")?)?
        {
            let mut options = GameexeDecodeOptions {
                exe_key16: self.keys.exe_key16,
                base_angou_code: self.keys.base_angou_code.clone(),
                game_angou_code: Some(
                    self.keys
                        .game_angou_code
                        .clone()
                        .unwrap_or_else(|| GAMEEXE_KEY.to_vec()),
                ),
                ..GameexeDecodeOptions::default()
            };
            if let Some(order) = &self.keys.chain_order {
                options.chain_order.clone_from(order);
            }
            return Ok(Some(SiglusGameexe::Dat(bytes, options)));
        }
        Ok(self
            .files
            .read_all(ASSET, &Self::file_key(&self.append, "Gameexe.ini")?)?
            .map(SiglusGameexe::Ini))
    }

    fn load_scene_pack(&self, append: &str) -> Result<(Vec<u8>, ScenePckDecodeOptions)> {
        let file = Self::file_key(append, "Scene.pck")?;
        let content = match self.files.read_all(ASSET, &file)? {
            None if append.is_empty() => self.files.read_all(ASSET, "Data/Scene.pck")?,
            other => other,
        };
        let bytes = content.with_context(|| format!("Siglus Host scene pack missing: {file}"))?;
        Ok((
            bytes,
            ScenePckDecodeOptions {
                exe_angou_element: self.keys.exe_key16.map(|key| key.to_vec()),
                easy_angou_code: Some(SCENE_KEY.to_vec()),
                string_encryption_override: self.keys.override_string_encryption,
            },
        ))
    }

    fn initial_append(&self) -> Result<(String, String)> {
        Ok((self.append.clone(), String::new()))
    }
}

struct DirectoryFiles(PathBuf);

impl DirectoryFiles {
    fn path(&self, key: &str) -> PathBuf {
        self.0.join(key)
    }

    fn resolve_file(&self, key: &str) -> Result<Option<PathBuf>> {
        let path = self.path(key);
        with_project_files(None, || resolve_game_file(&path))
    }
}

impl SiglusResourceSource for DirectoryFiles {
    fn query_size(&self, key: &str) -> Result<Option<u64>> {
        self.resolve_file(key)?
            .map(|path| Ok(path.metadata()?.len()))
            .transpose()
    }

    fn read_range(&self, key: &str, offset: u64, output: &mut [u8]) -> Result<usize> {
        let path = self
            .resolve_file(key)?
            .context("Siglus directory asset missing")?;
        let mut file = File::open(path)?;
        file.seek(SeekFrom::Start(offset))?;
        Ok(file.read(output)?)
    }

    fn resolve_local_directory(&self, key: &str) -> Result<Option<PathBuf>> {
        let path = self.path(key);
        Ok(with_project_files(None, || resolve_game_path(&path))?.filter(|p| p.is_dir()))
    }
}

impl SiglusAdapter {
    /// Reuse the directory bootstrap while sending file reads through the native Host.
    pub fn open_directory_host(config: SiglusHostConfig) -> Result<Self> {
        let root = config.project_dir.clone();
        let files = Arc::new(DirectoryFiles(root.clone()));
        Self::open_with(Some((root, files)), || {
            crate::SiglusHost::new_external(config)
        })
    }

    /// # Safety
    /// The table's functions and context must honor the C Host IO contract.
    pub unsafe fn open_callback_io(callbacks: SiglusProjectIoV1, append: &str) -> Result<Self> {
        ensure!(callbacks.is_valid(), "invalid Siglus Host IO table");
        let normalized = append.replace('\\', "/");
        Boot::file_key(&normalized, "Scene.pck")?;
        unsafe { callbacks.retain.unwrap()(callbacks.user_data) };
        let files = Arc::new(Files(callbacks));
        let keys = files
            .read_all(ASSET, "key.toml")?
            .map(|bytes| -> Result<_> { parse_key_toml(std::str::from_utf8(&bytes)?) })
            .transpose()?
            .unwrap_or_default();
        let boot = Arc::new(Boot {
            files: files.clone(),
            append: normalized,
            keys,
        });
        let mut config = SiglusHostConfig::new(PathBuf::from("siglus-host-project"));
        config.save_store = Some(files.clone());
        SiglusAdapter::open_with_project_files(config, boot, files)
    }
}
