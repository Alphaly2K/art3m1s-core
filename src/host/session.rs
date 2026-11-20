use super::events::HostEvents;
use super::media::HostMediaCommandKind;
use crate::ffi::TextInjectResult;
use serde::Serialize;
use std::cell::RefCell;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct FontOverride {
    generation: u64,
    bytes: Option<Arc<[u8]>>,
}

struct SessionState {
    events: HostEvents,
    debug: AtomicBool,
    profile_io: AtomicBool,
    io_counters: Mutex<crate::ffi::HostFfiProfile>,
    damage_visualization: AtomicBool,
    script_debug_mode: AtomicI32,
    script_debug_level: AtomicI32,
    font: Mutex<FontOverride>,
}

/// Host output and configuration belonging to one runtime.
#[derive(Clone)]
pub struct HostSession {
    state: Arc<SessionState>,
    legacy: bool,
}

impl Default for HostSession {
    fn default() -> Self {
        Self::new()
    }
}

impl HostSession {
    pub fn new() -> Self {
        Self {
            state: Arc::new(SessionState {
                events: HostEvents::new_isolated(),
                debug: AtomicBool::new(false),
                profile_io: AtomicBool::new(false),
                io_counters: Mutex::default(),
                damage_visualization: AtomicBool::new(false),
                script_debug_mode: AtomicI32::new(0),
                script_debug_level: AtomicI32::new(0),
                font: Mutex::default(),
            }),
            legacy: false,
        }
    }

    pub(crate) fn legacy() -> Self {
        Self {
            legacy: true,
            ..Self::new()
        }
    }

    pub(crate) fn is_legacy(&self) -> bool {
        self.legacy
    }

    pub fn events(&self) -> &HostEvents {
        &self.state.events
    }

    pub fn set_debug(&self, enabled: bool) {
        if self.legacy {
            unsafe { crate::ffi::art3m1s_set_debug(i32::from(enabled)) };
            return;
        }
        self.state.debug.store(enabled, Ordering::Relaxed);
        if !enabled {
            self.state
                .damage_visualization
                .store(false, Ordering::Relaxed);
        }
    }

    pub fn debug_enabled(&self) -> bool {
        if self.legacy {
            crate::ffi::legacy_debug_enabled()
        } else {
            self.state.debug.load(Ordering::Relaxed)
        }
    }

    pub(crate) fn set_profile_io_enabled(&self, enabled: bool) {
        self.state.profile_io.store(enabled, Ordering::Relaxed);
        if !enabled {
            self.take_profile_io_counters();
        }
    }

    pub(crate) fn profile_io_enabled(&self) -> bool {
        self.state.profile_io.load(Ordering::Relaxed)
    }

    pub(crate) fn take_profile_io_counters(&self) -> crate::ffi::HostFfiProfile {
        std::mem::take(&mut *self.state.io_counters.lock().unwrap())
    }

    pub(crate) fn record_profile_io(&self, elapsed_ns: u64, bytes: u64) {
        let mut value = self.state.io_counters.lock().unwrap();
        value.calls = value.calls.saturating_add(1);
        value.elapsed_ns = value.elapsed_ns.saturating_add(elapsed_ns);
        value.bytes = value.bytes.saturating_add(bytes);
    }

    pub fn set_damage_visualization(&self, enabled: bool) {
        if self.legacy {
            unsafe { crate::ffi::art3m1s_set_damage_visualization(i32::from(enabled)) };
        } else {
            self.state
                .damage_visualization
                .store(enabled && self.debug_enabled(), Ordering::Relaxed);
        }
    }

    pub(crate) fn damage_visualization_enabled(&self) -> bool {
        if self.legacy {
            crate::ffi::damage_visualization_enabled()
        } else {
            self.debug_enabled() && self.state.damage_visualization.load(Ordering::Relaxed)
        }
    }

    pub fn set_font_override(&self, bytes: Vec<u8>) -> Result<(), String> {
        if self.legacy {
            return crate::ffi::set_font_override(bytes);
        }
        ab_glyph::FontRef::try_from_slice(&bytes)
            .map_err(|error| format!("invalid font: {error}"))?;
        let mut font = self.state.font.lock().unwrap();
        font.generation = font
            .generation
            .checked_add(1)
            .ok_or("font generation exhausted")?;
        font.bytes = Some(bytes.into());
        Ok(())
    }

    pub fn clear_font_override(&self) {
        if self.legacy {
            crate::ffi::clear_font_override();
        } else {
            self.state.font.lock().unwrap().bytes = None;
        }
    }

    pub(crate) fn font_override(&self) -> Option<(u64, Arc<[u8]>)> {
        if self.legacy {
            return crate::ffi::font_override();
        }
        let font = self.state.font.lock().unwrap();
        Some((font.generation, font.bytes.clone()?))
    }

    pub(crate) fn enabled(&self) -> bool {
        if self.legacy {
            crate::host_events::legacy_active_events().is_some_and(|events| events.enabled())
        } else {
            self.events().enabled()
        }
    }

    pub(crate) fn emit_ui(&self, kind: &str, payload: serde_json::Value) {
        if self.legacy {
            crate::ffi::emit_ui_command(kind, payload);
        } else {
            self.events().push_ui(kind, &payload.to_string());
        }
    }

    pub(crate) fn emit_media<T: Serialize>(&self, kind: HostMediaCommandKind, payload: T) {
        if self.legacy {
            let payload = serde_json::to_value(payload).unwrap_or_else(|error| {
                self.log(
                    "W",
                    &format!("serialize media command {}: {error}", kind.as_str()),
                );
                serde_json::json!({})
            });
            if let Some(events) =
                crate::host_events::legacy_active_events().filter(|events| events.enabled())
            {
                events.push_media(kind.as_str(), &payload.to_string());
            }
            return;
        }
        match serde_json::to_string(&payload) {
            Ok(payload) => self.events().push_media(kind.as_str(), &payload),
            Err(error) => self.log(
                "E",
                &format!("serialize media command {}: {error}", kind.as_str()),
            ),
        }
    }

    pub(crate) fn log(&self, level: &str, message: &str) {
        if self.legacy {
            crate::ffi::log(level, message);
        } else {
            self.events().push_log(level, message);
        }
    }

    pub(crate) fn query_font_list(&self, monospace: bool, vertical: bool) -> Vec<String> {
        if self.legacy {
            crate::ffi::query_font_list(monospace, vertical)
        } else {
            self.events()
                .query_font_list(monospace, vertical)
                .unwrap_or_default()
        }
    }

    pub(crate) fn query_window_state(&self) -> (bool, bool) {
        if self.legacy {
            crate::ffi::query_window_state()
        } else {
            self.events().query_window_state().unwrap_or_default()
        }
    }

    pub(crate) fn text_injection(&self, text: &str) -> TextInjectResult {
        if self.legacy {
            return crate::ffi::request_text_injection(text);
        }
        if let Some(replacement) = self.events().text_replacement(text) {
            TextInjectResult::Replaced(replacement)
        } else if self.events().text_translation_enabled() {
            TextInjectResult::Pending
        } else {
            TextInjectResult::Unchanged
        }
    }

    pub(crate) fn set_script_debug_config(&self, mode: Option<i32>, level: Option<i32>) {
        if self.legacy {
            crate::ffi::set_script_debug_config(mode, level);
            return;
        }
        if let Some(mode) = mode {
            self.state.script_debug_mode.store(mode, Ordering::Relaxed);
        }
        if let Some(level) = level {
            self.state
                .script_debug_level
                .store(level, Ordering::Relaxed);
        }
    }

    pub(crate) fn script_debug_mode(&self) -> i32 {
        if self.legacy {
            crate::ffi::script_debug_mode()
        } else {
            self.state.script_debug_mode.load(Ordering::Relaxed)
        }
    }

    pub(crate) fn script_debug_level(&self) -> i32 {
        if self.legacy {
            crate::ffi::script_debug_level()
        } else {
            self.state.script_debug_level.load(Ordering::Relaxed)
        }
    }

    pub(crate) fn script_debug_print_allowed(&self, level: i32) -> bool {
        self.script_debug_mode() != 0 && level <= self.script_debug_level()
    }

    pub(crate) fn reset_project(&self) {
        if !self.legacy {
            self.events().clear_pending();
            self.set_script_debug_config(Some(0), Some(0));
        }
    }

    pub(crate) fn enter(&self) -> SessionScope {
        let current = (!self.legacy).then(|| self.clone());
        SessionScope {
            previous: CURRENT_SESSION.with(|slot| slot.replace(current)),
            _owner_thread: PhantomData,
        }
    }
}

thread_local! {
    static CURRENT_SESSION: RefCell<Option<HostSession>> = const { RefCell::new(None) };
}

pub(crate) fn current_session() -> Option<HostSession> {
    CURRENT_SESSION.with(|slot| slot.borrow().clone())
}

// Logging from shared backends has no runtime parameter. The scope selects its
// destination only; all settings and queues remain owned by the session.
pub(crate) struct SessionScope {
    previous: Option<HostSession>,
    _owner_thread: PhantomData<Rc<()>>,
}

impl Drop for SessionScope {
    fn drop(&mut self) {
        CURRENT_SESSION.with(|slot| slot.replace(self.previous.take()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_scopes_restore_diagnostics_and_keep_font_state_local() {
        let a = HostSession::new();
        let b = HostSession::new();
        a.set_debug(true);
        a.set_damage_visualization(true);
        a.set_profile_io_enabled(true);
        b.set_damage_visualization(true);
        // Font parsing has separate validation; these byte markers identify
        // the two renderer-facing resource snapshots.
        a.state.font.lock().unwrap().bytes = Some(Arc::from([1_u8, 2]));
        b.state.font.lock().unwrap().bytes = Some(Arc::from([3_u8, 4]));
        assert_ne!(a.font_override(), b.font_override());
        let previous = current_session();
        {
            let _scope = a.enter();
            assert!(crate::ffi::debug_enabled());
            {
                let legacy = HostSession::legacy();
                let _scope = legacy.enter();
                assert!(current_session().is_none());
            }
            assert!(crate::ffi::debug_enabled());
            let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _scope = b.enter();
                assert!(!crate::ffi::debug_enabled());
                assert!(!current_session().unwrap().profile_io_enabled());
                panic!("unwind session scope");
            }));
            assert!(failed.is_err());
            assert!(crate::ffi::debug_enabled());
            assert!(current_session().unwrap().profile_io_enabled());
            std::thread::spawn({
                let b = b.clone();
                move || {
                    let _scope = b.enter();
                    crate::ffi::log("I", "worker B");
                }
            })
            .join()
            .unwrap();
            crate::ffi::log("I", "owner A");
        }
        assert_eq!(current_session().is_some(), previous.is_some());
        assert!(a.damage_visualization_enabled());
        assert!(!b.damage_visualization_enabled());
        a.clear_font_override();
        assert!(a.font_override().is_none());
        assert_eq!(&*b.font_override().unwrap().1, &[3, 4]);
        for (host, expected) in [(&a, "owner A"), (&b, "worker B")] {
            let mut bytes = vec![0; host.events().queued_bytes()];
            let (written, count) = host.events().poll_events(&mut bytes);
            assert_eq!(count, 1);
            assert_eq!(&bytes[24..written], expected.as_bytes());
        }
    }
}
