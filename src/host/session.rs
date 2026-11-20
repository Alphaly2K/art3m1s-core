use super::events::HostEvents;
use super::media::HostMediaCommandKind;
use serde::Serialize;
use std::cell::RefCell;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TextInjectResult {
    Unchanged,
    Replaced(String),
    Pending,
}

#[derive(Default)]
struct FontOverride {
    generation: u64,
    bytes: Option<Arc<[u8]>>,
}

static LEGACY_FONT_OVERRIDE: Mutex<FontOverride> = Mutex::new(FontOverride {
    generation: 0,
    bytes: None,
});

fn install_font_override(slot: &Mutex<FontOverride>, bytes: Vec<u8>) -> Result<(), String> {
    ab_glyph::FontRef::try_from_slice(&bytes).map_err(|error| format!("invalid font: {error}"))?;
    let mut font = slot.lock().unwrap();
    font.generation = font
        .generation
        .checked_add(1)
        .ok_or("font generation exhausted")?;
    font.bytes = Some(bytes.into());
    Ok(())
}

pub(crate) unsafe extern "C" fn art3m1s_set_font_override(data: *const u8, len: i32) -> i32 {
    if data.is_null() || len <= 0 {
        return 0;
    }
    let bytes = unsafe { std::slice::from_raw_parts(data, len as usize) }.to_vec();
    match install_font_override(&LEGACY_FONT_OVERRIDE, bytes) {
        Ok(()) => 1,
        Err(error) => {
            crate::core_warn!("art3m1s_set_font_override: {error}");
            0
        }
    }
}

pub(crate) unsafe extern "C" fn art3m1s_clear_font_override() {
    LEGACY_FONT_OVERRIDE.lock().unwrap().bytes = None;
}

#[derive(Default)]
struct ScriptDebugConfig {
    mode: AtomicI32,
    level: AtomicI32,
}

static LEGACY_SCRIPT_DEBUG: ScriptDebugConfig = ScriptDebugConfig {
    mode: AtomicI32::new(0),
    level: AtomicI32::new(0),
};

struct SessionState {
    events: HostEvents,
    debug: AtomicBool,
    profile_io: AtomicBool,
    io_counters: Mutex<crate::ffi::HostFfiProfile>,
    damage_visualization: AtomicBool,
    script_debug: ScriptDebugConfig,
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
                script_debug: ScriptDebugConfig::default(),
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

    fn font_slot(&self) -> &Mutex<FontOverride> {
        if self.legacy {
            &LEGACY_FONT_OVERRIDE
        } else {
            &self.state.font
        }
    }

    /// Install an sfnt font for subsequent text rendered by this session.
    pub fn set_font_override(&self, bytes: Vec<u8>) -> Result<(), String> {
        install_font_override(self.font_slot(), bytes)
    }

    pub fn clear_font_override(&self) {
        self.font_slot().lock().unwrap().bytes = None;
    }

    pub(crate) fn font_override(&self) -> Option<(u64, Arc<[u8]>)> {
        let font = self.font_slot().lock().unwrap();
        Some((font.generation, font.bytes.clone()?))
    }

    fn with_events<T>(&self, operation: impl FnOnce(&HostEvents) -> T) -> Option<T> {
        if self.legacy {
            super::events::legacy_active_events()
                .as_ref()
                .map(operation)
        } else {
            Some(operation(self.events()))
        }
    }

    pub(crate) fn enabled(&self) -> bool {
        self.with_events(HostEvents::enabled).unwrap_or(false)
    }

    pub(crate) fn emit_ui(&self, kind: &str, payload: serde_json::Value) {
        self.with_events(|events| {
            if events.enabled() {
                events.push_ui(kind, &payload.to_string());
            }
        });
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
            self.with_events(|events| {
                if events.enabled() {
                    events.push_media(kind.as_str(), &payload.to_string());
                }
            });
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
        self.with_events(|events| events.query_font_list(monospace, vertical))
            .flatten()
            .unwrap_or_default()
    }

    pub(crate) fn query_window_state(&self) -> (bool, bool) {
        self.with_events(HostEvents::query_window_state)
            .flatten()
            .unwrap_or_default()
    }

    pub(crate) fn text_injection(&self, text: &str) -> TextInjectResult {
        self.with_events(|events| {
            if let Some(replacement) = events.text_replacement(text) {
                TextInjectResult::Replaced(replacement)
            } else if events.text_translation_enabled() {
                TextInjectResult::Pending
            } else {
                TextInjectResult::Unchanged
            }
        })
        .unwrap_or(TextInjectResult::Unchanged)
    }

    fn script_debug(&self) -> &ScriptDebugConfig {
        if self.legacy {
            &LEGACY_SCRIPT_DEBUG
        } else {
            &self.state.script_debug
        }
    }

    pub(crate) fn set_script_debug_config(&self, mode: Option<i32>, level: Option<i32>) {
        let config = self.script_debug();
        if let Some(mode) = mode {
            config.mode.store(mode, Ordering::Relaxed);
        }
        if let Some(level) = level {
            config.level.store(level, Ordering::Relaxed);
        }
    }

    pub(crate) fn script_debug_mode(&self) -> i32 {
        self.script_debug().mode.load(Ordering::Relaxed)
    }

    pub(crate) fn script_debug_level(&self) -> i32 {
        self.script_debug().level.load(Ordering::Relaxed)
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
    fn font_override_rejects_invalid_bytes_without_storing() {
        let legacy = HostSession::legacy();
        let before = legacy.font_override().map(|(generation, _)| generation);
        let invalid = [0, 1, 2, 3];
        assert_eq!(unsafe { art3m1s_set_font_override(invalid.as_ptr(), 4) }, 0);
        assert_eq!(unsafe { art3m1s_set_font_override(std::ptr::null(), 0) }, 0);
        assert_eq!(
            legacy.font_override().map(|(generation, _)| generation),
            before
        );
    }

    #[test]
    fn script_debug_config_gates_debugprint_output() {
        let legacy = HostSession::legacy();
        let second_legacy = HostSession::legacy();
        let isolated = HostSession::new();
        legacy.set_script_debug_config(Some(0), Some(0));
        assert!(!legacy.script_debug_print_allowed(0));

        legacy.set_script_debug_config(Some(1), None);
        assert!(second_legacy.script_debug_print_allowed(0));
        assert!(!legacy.script_debug_print_allowed(1));
        assert!(!isolated.script_debug_print_allowed(0));

        legacy.set_script_debug_config(None, Some(2));
        assert!(legacy.script_debug_print_allowed(2));
        assert!(!legacy.script_debug_print_allowed(3));

        isolated.set_script_debug_config(Some(2), Some(3));
        assert!(isolated.script_debug_print_allowed(3));
        isolated.reset_project();
        assert!(!isolated.script_debug_print_allowed(0));
        assert!(legacy.script_debug_print_allowed(2));
        legacy.set_script_debug_config(Some(0), Some(0));
    }

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
