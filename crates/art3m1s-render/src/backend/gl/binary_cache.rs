use glow::HasContext;
use std::fs::{self, File, OpenOptions};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const MAX_BINARY: usize = 8 * 1024 * 1024;
const MAGIC: &[u8] = b"artemis-glsl-binary-1";
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

pub(super) struct Cache {
    directory: PathBuf,
    driver: [String; 4],
    formats: Vec<i32>,
}

impl Cache {
    pub(super) fn open(gl: &glow::Context, directory: &Path) -> Option<Self> {
        if !directory.is_absolute() {
            return None;
        }
        let formats = unsafe { gl.get_parameter_i32(glow::NUM_PROGRAM_BINARY_FORMATS) };
        if !(1..=64).contains(&formats) || fs::create_dir_all(directory).is_err() {
            return None;
        }
        let mut formats = vec![0; formats as usize];
        unsafe { gl.get_parameter_i32_slice(glow::PROGRAM_BINARY_FORMATS, &mut formats) };
        let driver = [
            glow::VENDOR,
            glow::RENDERER,
            glow::VERSION,
            glow::SHADING_LANGUAGE_VERSION,
        ]
        .map(|parameter| unsafe { gl.get_parameter_string(parameter) });
        Some(Self {
            directory: directory.into(),
            driver,
            formats,
        })
    }

    pub(super) fn load_or_compile(
        &self,
        gl: &glow::Context,
        vertex: &str,
        fragment: &str,
        compile: impl FnOnce() -> Result<glow::Program, String>,
    ) -> Result<glow::Program, String> {
        let mut key = MAGIC.to_vec();
        for field in self
            .driver
            .iter()
            .map(String::as_str)
            .chain([vertex, fragment])
        {
            key.extend_from_slice(&(field.len() as u64).to_le_bytes());
            key.extend_from_slice(field.as_bytes());
        }
        let mut hash = DefaultHasher::new();
        key.hash(&mut hash);
        let path = self.directory.join(format!("{:016x}.glbin", hash.finish()));
        if let Some(program) = self.try_load(gl, &path, &key) {
            return Ok(program);
        }
        let program = compile()?;
        if let Some(binary) = unsafe { gl.get_program_binary(program) }
            && (1..=MAX_BINARY).contains(&binary.buffer.len())
            && self.formats.contains(&(binary.format as i32))
        {
            self.store(&path, &key, binary);
        }
        Ok(program)
    }

    fn try_load(&self, gl: &glow::Context, path: &Path, key: &[u8]) -> Option<glow::Program> {
        let mut file = File::open(path).ok()?;
        let mut prefix = vec![0; key.len()];
        file.read_exact(&mut prefix).ok()?;
        if prefix != key {
            return None;
        }
        let mut format = [0; 4];
        file.read_exact(&mut format).ok()?;
        let format = u32::from_le_bytes(format);
        if !self.formats.contains(&(format as i32)) {
            return None;
        }
        let mut buffer = Vec::new();
        file.take((MAX_BINARY + 1) as u64)
            .read_to_end(&mut buffer)
            .ok()?;
        if !(1..=MAX_BINARY).contains(&buffer.len()) {
            return None;
        }
        unsafe {
            let program = gl.create_program().ok()?;
            gl.program_binary(program, &glow::ProgramBinary { format, buffer });
            let error = gl.get_error();
            if error == glow::NO_ERROR && gl.get_program_link_status(program) {
                return Some(program);
            }
            gl.delete_program(program);
        }
        None
    }

    fn store(&self, path: &Path, key: &[u8], binary: glow::ProgramBinary) {
        let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let temporary = path.with_extension(format!("{}.{}.tmp", std::process::id(), id));
        let Ok(mut file) = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        else {
            return;
        };
        let written = file
            .write_all(key)
            .and_then(|()| file.write_all(&binary.format.to_le_bytes()))
            .and_then(|()| file.write_all(&binary.buffer));
        drop(file);
        if written.and_then(|()| fs::rename(&temporary, path)).is_err() {
            let _ = fs::remove_file(temporary);
        }
    }
}
