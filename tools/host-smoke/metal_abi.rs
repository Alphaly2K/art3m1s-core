use art3m1s_render::artemis::FramePacket;
use std::{
    ffi::CStr,
    os::raw::c_char,
    sync::{Mutex, MutexGuard},
};

static CONNECTION: Mutex<()> = Mutex::new(());

#[repr(C)]
struct Bytes {
    data: *const u8,
    size: u64,
}

impl Bytes {
    fn empty() -> Self {
        Self {
            data: std::ptr::null(),
            size: 0,
        }
    }

    fn from_slice(bytes: &[u8]) -> Self {
        Self {
            data: bytes.as_ptr(),
            size: bytes.len() as u64,
        }
    }
}

unsafe extern "C" {
    fn metal_connect(path: *const c_char) -> i32;
    fn metal_disconnect();
    fn metal_create(out: *mut u64) -> i32;
    fn metal_destroy(rt: u64) -> i32;
    fn metal_step(rt: u64, id: u64, ms: u64, waiting: *mut u32) -> i32;
    fn metal_poll(rt: u64, buffer: *mut u8, capacity: u64, size: *mut u64) -> i32;
    fn metal_submit(
        rt: u64,
        domain: u16,
        kind: u32,
        epoch: u64,
        request: u64,
        meta: Bytes,
        blob: Bytes,
    ) -> i32;
    fn metal_acquire(rt: u64, after: u64, token: *mut u64, packet: *mut Bytes) -> i32;
    fn metal_pixels(rt: u64, token: u64, index: u32, pixels: *mut Bytes) -> i32;
    fn metal_release(rt: u64, token: u64) -> i32;
}

fn check(status: i32, operation: &str) -> Result<(), String> {
    if status == 0 {
        Ok(())
    } else {
        Err(format!("{operation} returned {status}"))
    }
}

pub struct Session {
    id: u64,
    _connection: MutexGuard<'static, ()>,
}

impl Session {
    pub fn open(path: &CStr) -> Result<Self, String> {
        let connection = CONNECTION
            .try_lock()
            .map_err(|_| "Metal probe already has a connection")?;
        check(unsafe { metal_connect(path.as_ptr()) }, "connect")?;
        let mut rt = 0;
        if let Err(error) = check(unsafe { metal_create(&mut rt) }, "create/open project") {
            unsafe { metal_disconnect() };
            return Err(error);
        }
        Ok(Self {
            id: rt,
            _connection: connection,
        })
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn step(&self, id: u64, ms: u64) -> Result<bool, String> {
        let mut waiting = 0;
        check(unsafe { metal_step(self.id, id, ms, &mut waiting) }, "step")?;
        Ok(waiting != 0)
    }

    pub fn poll<'a>(&self, buffer: &'a mut [u8]) -> Result<&'a [u8], String> {
        let mut size = 0;
        check(
            unsafe { metal_poll(self.id, buffer.as_mut_ptr(), buffer.len() as u64, &mut size) },
            "poll events",
        )?;
        Ok(&buffer[..size as usize])
    }

    pub fn reply(
        &self,
        service: (u16, u32),
        epoch: u64,
        request: u64,
        meta: &str,
        blob: &[u8],
    ) -> Result<(), String> {
        check(
            unsafe {
                metal_submit(
                    self.id,
                    service.0,
                    service.1,
                    epoch,
                    request,
                    Bytes::from_slice(meta.as_bytes()),
                    Bytes::from_slice(blob),
                )
            },
            "submit reply",
        )
    }

    pub fn frame(&self, after: u64) -> Result<Frame<'_>, String> {
        let mut frame = Frame {
            session: self,
            token: 0,
            packet: Bytes::empty(),
        };
        check(
            unsafe { metal_acquire(self.id, after, &mut frame.token, &mut frame.packet) },
            "acquire frame",
        )?;
        Ok(frame)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        unsafe {
            metal_destroy(self.id);
            metal_disconnect();
        }
    }
}

pub struct Frame<'a> {
    session: &'a Session,
    token: u64,
    packet: Bytes,
}

impl Frame<'_> {
    pub fn runtime(&self) -> u64 {
        self.session.id()
    }

    pub fn decode(&self) -> Result<FramePacket, String> {
        FramePacket::decode(self.borrow(&self.packet)?).map_err(|error| error.to_string())
    }

    pub fn pixels(&self, index: usize) -> Result<&[u8], String> {
        let mut pixels = Bytes::empty();
        let index = u32::try_from(index).map_err(|_| "texture index overflow")?;
        check(
            unsafe { metal_pixels(self.runtime(), self.token, index, &mut pixels) },
            "borrow texture",
        )?;
        self.borrow(&pixels)
    }

    fn borrow<'a>(&'a self, bytes: &Bytes) -> Result<&'a [u8], String> {
        if bytes.size == 0 {
            return Ok(&[]);
        }
        if bytes.data.is_null() || bytes.size > isize::MAX as u64 {
            return Err("invalid frame borrow".into());
        }
        // Core pins the byte storage for this token until Frame drops.
        Ok(unsafe { std::slice::from_raw_parts(bytes.data, bytes.size as usize) })
    }
}

impl Drop for Frame<'_> {
    fn drop(&mut self) {
        if self.token != 0 {
            unsafe { metal_release(self.runtime(), self.token) };
        }
    }
}
