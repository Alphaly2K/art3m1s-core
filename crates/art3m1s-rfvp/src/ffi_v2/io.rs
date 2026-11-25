use std::ffi::c_void;
use std::mem::size_of;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::host_runtime::{
    RfvpAssetFile, RfvpAssetSource, RfvpError, RfvpFile, RfvpFileInfo, RfvpResult, RfvpSaveStore,
};

use super::{ARGUMENT, Bytes, check_pointer, path};

const ASSET: u32 = 1;
const SAVE: u32 = 2;
const NOT_FOUND: i32 = -7;
const IO_ERROR: i32 = -8;

type Open = unsafe extern "C" fn(usize, u32, Bytes, *mut u64, *mut u64) -> i32;
type ReadAt = unsafe extern "C" fn(usize, u64, u64, *mut u8, u64, *mut u64) -> i32;
type Close = unsafe extern "C" fn(usize, u64);
type Visit = unsafe extern "C" fn(*mut c_void, Bytes, u64) -> i32;
type List = unsafe extern "C" fn(usize, u32, Bytes, Bytes, Visit, *mut c_void) -> i32;
type WriteSave = unsafe extern "C" fn(usize, Bytes, Bytes) -> i32;
type RemoveSave = unsafe extern "C" fn(usize, Bytes) -> i32;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Io {
    struct_size: u32,
    pub(super) nls: u32,
    context: usize,
    open: Option<Open>,
    read_at: Option<ReadAt>,
    close: Option<Close>,
    list: Option<List>,
    write_save: Option<WriteSave>,
    remove_save: Option<RemoveSave>,
}

impl Io {
    pub(super) unsafe fn read(info: *const Self) -> Result<Self, i32> {
        check_pointer(info)?;
        if unsafe { info.cast::<u32>().read() } < size_of::<Self>() as u32 {
            return Err(ARGUMENT);
        }
        let info = unsafe { info.read() };
        if info.open.is_none()
            || info.read_at.is_none()
            || info.close.is_none()
            || info.list.is_none()
            || info.write_save.is_none()
            || info.remove_save.is_none()
        {
            return Err(ARGUMENT);
        }
        Ok(info)
    }
}

pub(super) struct Callbacks {
    io: Io,
    gate: Arc<Mutex<()>>,
}

impl Callbacks {
    pub(super) fn new(io: Io) -> Self {
        Self {
            io,
            gate: Arc::new(Mutex::new(())),
        }
    }

    fn open(&self, domain: u32, key: &str) -> RfvpResult<CallbackFile> {
        let _guard = lock(&self.gate);
        let mut handle = 0;
        let mut len = 0;
        result(unsafe {
            (self.io.open.unwrap())(
                self.io.context,
                domain,
                bytes(key.as_bytes()),
                &mut handle,
                &mut len,
            )
        })?;
        Ok(CallbackFile {
            io: self.io,
            gate: self.gate.clone(),
            handle,
            len,
        })
    }

    fn entries(
        &self,
        domain: u32,
        root: &str,
        extension: &str,
        visit: &mut dyn FnMut(&str, RfvpFileInfo) -> RfvpResult<()>,
    ) -> RfvpResult<()> {
        let _guard = lock(&self.gate);
        let mut visitor = Visitor { visit, error: None };
        let status = unsafe {
            (self.io.list.unwrap())(
                self.io.context,
                domain,
                bytes(root.as_bytes()),
                bytes(extension.as_bytes()),
                visit_entry,
                (&mut visitor as *mut Visitor<'_>).cast(),
            )
        };
        visitor.error.map_or_else(|| result(status), Err)
    }
}

fn lock(gate: &Mutex<()>) -> MutexGuard<'_, ()> {
    gate.lock().unwrap_or_else(|poison| poison.into_inner())
}

struct Visitor<'a> {
    visit: &'a mut dyn FnMut(&str, RfvpFileInfo) -> RfvpResult<()>,
    error: Option<RfvpError>,
}

unsafe extern "C" fn visit_entry(context: *mut c_void, key: Bytes, len: u64) -> i32 {
    let visitor = unsafe { &mut *context.cast::<Visitor<'_>>() };
    if visitor.error.is_some() {
        return IO_ERROR;
    }
    let result = catch_unwind(AssertUnwindSafe(|| {
        let key = unsafe { path(&key) }
            .ok()
            .flatten()
            .ok_or(RfvpError::InvalidArgument)?;
        (visitor.visit)(&key, RfvpFileInfo::file(len))
    }));
    match result {
        Ok(Ok(())) => 0,
        Ok(Err(error)) => {
            visitor.error = Some(error);
            IO_ERROR
        }
        Err(_) => {
            visitor.error = Some(RfvpError::Backend);
            IO_ERROR
        }
    }
}

impl RfvpAssetSource for Callbacks {
    fn open(&self, path: &str) -> RfvpResult<RfvpAssetFile> {
        Ok(Box::new(self.open(ASSET, path)?))
    }

    fn metadata(&self, path: &str) -> RfvpResult<RfvpFileInfo> {
        self.open(ASSET, path)
            .map(|file| RfvpFileInfo::file(file.len))
    }

    fn enumerate(
        &self,
        root: &str,
        extension: &str,
        visitor: &mut dyn FnMut(&str, RfvpFileInfo) -> RfvpResult<()>,
    ) -> RfvpResult<()> {
        self.entries(ASSET, root, extension, visitor)
    }
}

impl RfvpSaveStore for Callbacks {
    fn read(&self, key: &str) -> RfvpResult<Vec<u8>> {
        self.open(SAVE, key)?.read_to_vec(usize::MAX)
    }

    fn write(&self, key: &str, data: &[u8]) -> RfvpResult<()> {
        let _guard = lock(&self.gate);
        result(unsafe {
            (self.io.write_save.unwrap())(self.io.context, bytes(key.as_bytes()), bytes(data))
        })
    }

    fn remove(&self, key: &str) -> RfvpResult<()> {
        let _guard = lock(&self.gate);
        result(unsafe { (self.io.remove_save.unwrap())(self.io.context, bytes(key.as_bytes())) })
    }

    fn list(&self) -> RfvpResult<Vec<String>> {
        let mut keys = Vec::new();
        self.entries(SAVE, ".", "", &mut |key, _| {
            keys.push(key.to_owned());
            Ok(())
        })?;
        Ok(keys)
    }
}

struct CallbackFile {
    io: Io,
    gate: Arc<Mutex<()>>,
    handle: u64,
    len: u64,
}

impl RfvpFile for CallbackFile {
    fn len(&mut self) -> RfvpResult<u64> {
        Ok(self.len)
    }

    fn read_at(&mut self, offset: u64, out: &mut [u8]) -> RfvpResult<usize> {
        let _guard = lock(&self.gate);
        let mut read = 0;
        result(unsafe {
            (self.io.read_at.unwrap())(
                self.io.context,
                self.handle,
                offset,
                out.as_mut_ptr(),
                out.len() as u64,
                &mut read,
            )
        })?;
        if read > out.len() as u64 {
            return Err(RfvpError::InvalidData);
        }
        Ok(read as usize)
    }
}

impl Drop for CallbackFile {
    fn drop(&mut self) {
        let _guard = lock(&self.gate);
        unsafe { (self.io.close.unwrap())(self.io.context, self.handle) };
    }
}

fn bytes(data: &[u8]) -> Bytes {
    Bytes {
        data: if data.is_empty() {
            std::ptr::null()
        } else {
            data.as_ptr()
        },
        len: data.len() as u64,
    }
}

fn result(status: i32) -> RfvpResult<()> {
    match status {
        0 => Ok(()),
        NOT_FOUND => Err(RfvpError::NotFound),
        ARGUMENT => Err(RfvpError::InvalidArgument),
        _ => Err(RfvpError::Io),
    }
}
