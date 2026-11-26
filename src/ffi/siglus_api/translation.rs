use super::{STATUS_ARGUMENT, STATUS_OK, guarded, with_runtime};
use std::{collections::HashMap, mem::size_of, ptr, slice, str};

const ABSENT: i32 = 1;
const BUFFER_TOO_SMALL: i32 = -4;
const MAX_TEXT_BYTES: usize = 16 * 1024;
const MAX_ENTRIES: usize = 262_144;
const MAX_TABLE_BYTES: usize = 64 * 1024 * 1024;

#[repr(C)]
#[derive(Clone, Copy)]
struct Text {
    bytes: *const u8,
    len: usize,
}

#[repr(C)]
struct Replacement {
    source: Text,
    target: Text,
}

#[repr(C)]
pub struct SiglusTranslationApiV1 {
    version: u32,
    size: u32,
    configure: unsafe extern "C" fn(u64, u32, *const Replacement, usize) -> i32,
    poll: unsafe extern "C" fn(u64, *mut u64, *mut u8, usize, *mut usize) -> i32,
    complete: unsafe extern "C" fn(u64, u64, *const Text) -> i32,
}

static API: SiglusTranslationApiV1 = SiglusTranslationApiV1 {
    version: 1,
    size: size_of::<SiglusTranslationApiV1>() as u32,
    configure,
    poll,
    complete,
};

#[unsafe(no_mangle)]
pub extern "C" fn art3m1s_siglus_get_translation_api_v1() -> *const SiglusTranslationApiV1 {
    &API
}

unsafe fn read_text(text: Text) -> Option<String> {
    if text.len == 0 {
        return Some(String::new());
    }
    if text.bytes.is_null() || text.len > MAX_TEXT_BYTES {
        return None;
    }
    str::from_utf8(unsafe { slice::from_raw_parts(text.bytes, text.len) })
        .ok()
        .map(str::to_owned)
}

unsafe extern "C" fn configure(
    session: u64,
    online: u32,
    entries: *const Replacement,
    count: usize,
) -> i32 {
    guarded(|| {
        if online > 1
            || count > MAX_ENTRIES
            || (count != 0 && (entries.is_null() || !entries.is_aligned()))
        {
            return STATUS_ARGUMENT;
        }
        with_runtime(session, |runtime| {
            let entries = if count == 0 {
                &[]
            } else {
                unsafe { slice::from_raw_parts(entries, count) }
            };
            let mut table = HashMap::new();
            let mut bytes = 0usize;
            for entry in entries {
                bytes = bytes
                    .saturating_add(entry.source.len)
                    .saturating_add(entry.target.len);
                if bytes > MAX_TABLE_BYTES {
                    return STATUS_ARGUMENT;
                }
                let (Some(source), Some(target)) = (unsafe { read_text(entry.source) }, unsafe {
                    read_text(entry.target)
                }) else {
                    return STATUS_ARGUMENT;
                };
                table.insert(source, target);
            }
            runtime
                .adapter
                .with_host(|host| host.vm_mut().configure_text_translation(table, online != 0));
            STATUS_OK
        })
    })
}

unsafe extern "C" fn poll(
    session: u64,
    id: *mut u64,
    output: *mut u8,
    capacity: usize,
    written: *mut usize,
) -> i32 {
    guarded(|| {
        if id.is_null()
            || !id.is_aligned()
            || written.is_null()
            || !written.is_aligned()
            || (capacity != 0 && output.is_null())
        {
            return STATUS_ARGUMENT;
        }
        unsafe { written.write(0) };
        with_runtime(session, |runtime| {
            runtime.adapter.with_host(|host| {
                let vm = host.vm_mut();
                let Some((request, text)) = vm.text_translation_request() else {
                    return ABSENT;
                };
                unsafe { written.write(text.len()) };
                if capacity < text.len() {
                    return BUFFER_TOO_SMALL;
                }
                unsafe {
                    ptr::copy_nonoverlapping(text.as_ptr(), output, text.len());
                    id.write(request);
                }
                vm.consume_text_translation(request);
                STATUS_OK
            })
        })
    })
}

unsafe extern "C" fn complete(session: u64, id: u64, text: *const Text) -> i32 {
    guarded(|| {
        let reply = if text.is_null() {
            None
        } else {
            if !text.is_aligned() {
                return STATUS_ARGUMENT;
            }
            let Some(reply) = (unsafe { read_text(text.read()) }) else {
                return STATUS_ARGUMENT;
            };
            Some(reply)
        };
        with_runtime(session, |runtime| {
            runtime.adapter.with_host(|host| {
                if host.vm_mut().complete_text_translation(id, reply) {
                    STATUS_OK
                } else {
                    ABSENT
                }
            })
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ffi::siglus_api::{
        LAST_ERROR, NEXT_HANDLE, STATUS_HANDLE, create_frame_runtime, runtime_destroy,
        runtime_tick,
        tests::{GAMEEXE, scene_pack_with_text},
    };
    use std::{ffi::CString, path::PathBuf, sync::atomic::Ordering};

    #[test]
    fn stage_translation_uses_the_session_thread_and_rejects_stale_replies() {
        struct Project(PathBuf);
        impl Drop for Project {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let project = Project(std::env::temp_dir().join(format!(
            "art3m1s-siglus-translation-{}-{}",
            std::process::id(),
            NEXT_HANDLE.fetch_add(1, Ordering::Relaxed),
        )));
        std::fs::create_dir_all(&project.0).unwrap();
        std::fs::write(project.0.join("Gameexe.ini"), GAMEEXE).unwrap();
        std::fs::write(
            project.0.join("Scene.pck"),
            scene_pack_with_text(Some("Source")),
        )
        .unwrap();
        let path = CString::new(project.0.to_str().unwrap()).unwrap();
        let open = || {
            let mut handle = 0;
            assert_eq!(
                unsafe { create_frame_runtime(path.as_ptr(), ptr::null(), &mut handle) },
                STATUS_OK,
                "{}",
                LAST_ERROR.with(|e| e.borrow().clone())
            );
            handle
        };
        let tick = |handle, dt| {
            assert_eq!(
                unsafe { runtime_tick(handle, dt, 0, ptr::null_mut(), 0, ptr::null_mut()) },
                STATUS_OK,
                "{}",
                LAST_ERROR.with(|e| e.borrow().clone())
            );
        };
        let message = |handle, expected: &str| {
            with_runtime(handle, |rt| {
                rt.adapter.with_host(|host| {
                    assert_eq!(host.vm_mut().ctx.ui.message_text(), Some(expected))
                });
                STATUS_OK
            })
        };
        let text = |s: &'static str| Text {
            bytes: s.as_ptr(),
            len: s.len(),
        };
        let online = open();
        let patched = open();
        let api = unsafe { &*art3m1s_siglus_get_translation_api_v1() };
        assert_eq!(
            unsafe { (api.configure)(online, 1, ptr::null(), 0) },
            STATUS_OK
        );
        assert_eq!(
            std::thread::spawn(move || unsafe { (api.configure)(online, 0, ptr::null(), 0) })
                .join()
                .unwrap(),
            STATUS_HANDLE
        );
        let replacement = Replacement {
            source: text("Source"),
            target: text("Local patch"),
        };
        assert_eq!(
            unsafe { (api.configure)(patched, 1, &replacement, 1) },
            STATUS_OK
        );
        let mut id = 0;
        let mut len = 0;
        let mut buffer = [0; 32];
        let mut status = ABSENT;
        for _ in 0..8 {
            tick(online, 16);
            status = unsafe { (api.poll)(online, &mut id, ptr::null_mut(), 0, &mut len) };
            if status != ABSENT {
                break;
            }
        }
        assert_eq!(status, BUFFER_TOO_SMALL);
        assert_eq!(len, "Source".len());
        assert_eq!(
            unsafe { (api.poll)(online, &mut id, buffer.as_mut_ptr(), buffer.len(), &mut len) },
            STATUS_OK
        );
        assert_eq!(&buffer[..len], b"Source");
        assert_eq!(
            unsafe { (api.complete)(online, id, &text("Host reply")) },
            STATUS_OK
        );
        tick(online, 16);
        assert_eq!(message(online, "Host reply"), STATUS_OK);
        for _ in 0..8 {
            tick(patched, 16);
            let has_text = with_runtime(patched, |rt| {
                rt.adapter
                    .with_host(|host| i32::from(host.vm_mut().ctx.ui.message_text().is_some()))
            });
            if has_text == 1 {
                break;
            }
        }
        assert_eq!(message(patched, "Local patch"), STATUS_OK);
        assert_eq!(
            unsafe {
                (api.poll)(
                    patched,
                    &mut id,
                    buffer.as_mut_ptr(),
                    buffer.len(),
                    &mut len,
                )
            },
            ABSENT
        );
        assert_eq!(
            unsafe { (api.complete)(online, id, &text("Old reply")) },
            ABSENT
        );
        let timeout = open();
        assert_eq!(
            unsafe { (api.configure)(timeout, 1, ptr::null(), 0) },
            STATUS_OK
        );
        let mut pending = ABSENT;
        for _ in 0..8 {
            tick(timeout, 16);
            pending = unsafe {
                (api.poll)(
                    timeout,
                    &mut id,
                    buffer.as_mut_ptr(),
                    buffer.len(),
                    &mut len,
                )
            };
            if pending != ABSENT {
                break;
            }
        }
        assert_eq!(pending, STATUS_OK);
        tick(timeout, 3_000);
        assert_eq!(message(timeout, "Source"), STATUS_OK);
        assert_eq!(
            unsafe { (api.complete)(timeout, id, &text("Late reply")) },
            ABSENT
        );
        unsafe {
            runtime_destroy(online);
            runtime_destroy(patched);
            runtime_destroy(timeout);
        }
    }
}
