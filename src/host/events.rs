//! Callback-free host event transport and state.
//!
//! Event queues and host state live behind an opaque [`HostEvents`] handle.
//! Logs and media/UI commands originate from process-wide core code, so they
//! are routed to the currently enabled handle; no native-to-Dart callback is
//! retained.

use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex, Weak};

pub const EVENT_KIND_LOG: u32 = 1;
pub const EVENT_KIND_MEDIA: u32 = 2;
pub const EVENT_KIND_UI: u32 = 3;

const EVENT_VERSION: u32 = 1;
const EVENT_HEADER_SIZE: usize = 24;
const MAX_EVENTS: usize = 16_384;
const MAX_EVENT_PAYLOAD: usize = 4 * 1024 * 1024;

#[derive(Debug)]
struct HostEvent {
    kind: u32,
    aux: u32,
    sequence: u64,
    payload: Vec<u8>,
}

struct EventQueue {
    events: VecDeque<HostEvent>,
    next_sequence: u64,
    dropped: u64,
}

impl EventQueue {
    fn new() -> Self {
        Self {
            events: VecDeque::new(),
            next_sequence: 1,
            dropped: 0,
        }
    }
}

#[derive(Debug, Default)]
struct FontLists {
    normal: Option<Vec<String>>,
    monospace: Option<Vec<String>>,
    vertical: Option<Vec<String>>,
    monospace_vertical: Option<Vec<String>>,
}

struct HostEventsInner {
    enabled: AtomicBool,
    window_state: AtomicI32,
    translation_enabled: AtomicBool,
    queue: Mutex<EventQueue>,
    fonts: Mutex<FontLists>,
    replacements: Mutex<Option<HashMap<String, String>>>,
}

impl HostEventsInner {
    fn new() -> Self {
        Self {
            enabled: AtomicBool::new(false),
            window_state: AtomicI32::new(0),
            translation_enabled: AtomicBool::new(false),
            queue: Mutex::new(EventQueue::new()),
            fonts: Mutex::new(FontLists::default()),
            replacements: Mutex::new(None),
        }
    }

    fn push(&self, kind: u32, aux: u32, payload: Vec<u8>) {
        if !self.enabled.load(Ordering::Relaxed) || payload.len() > MAX_EVENT_PAYLOAD {
            return;
        }
        let mut queue = self.queue.lock().unwrap();
        while queue.events.len() >= MAX_EVENTS {
            let dropped = queue
                .events
                .iter()
                .position(|event| event.kind == EVENT_KIND_LOG)
                .and_then(|index| queue.events.remove(index))
                .or_else(|| queue.events.pop_front());
            if dropped.is_some() {
                queue.dropped = queue.dropped.saturating_add(1);
            } else {
                break;
            }
        }
        let sequence = queue.next_sequence;
        queue.next_sequence = queue.next_sequence.saturating_add(1);
        queue.events.push_back(HostEvent {
            kind,
            aux,
            sequence,
            payload,
        });
    }

    fn set_font_list(&self, monospace: bool, vertical: bool, names: Vec<String>) {
        let mut lists = self.fonts.lock().unwrap();
        match (monospace, vertical) {
            (false, false) => lists.normal = Some(names),
            (true, false) => lists.monospace = Some(names),
            (false, true) => lists.vertical = Some(names),
            (true, true) => lists.monospace_vertical = Some(names),
        }
    }
}

#[derive(Clone)]
pub struct HostEvents {
    inner: Arc<HostEventsInner>,
}

impl HostEvents {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(HostEventsInner::new()),
        }
    }

    pub fn enabled(&self) -> bool {
        self.inner.enabled.load(Ordering::Relaxed)
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.inner.enabled.store(enabled, Ordering::Relaxed);
        if enabled {
            let mut queue = self.inner.queue.lock().unwrap();
            queue.events.clear();
            queue.next_sequence = 1;
            queue.dropped = 0;
            drop(queue);
            activate(self);
        } else {
            deactivate(self);
        }
    }

    pub fn push_log(&self, level: &str, message: &str) {
        let aux = level.as_bytes().first().copied().unwrap_or(b'I') as u32;
        self.inner
            .push(EVENT_KIND_LOG, aux, message.as_bytes().to_vec());
    }

    pub fn push_media(&self, kind: &str, payload_json: &str) {
        let envelope = serde_json::json!({
            "kind": kind,
            "payload": serde_json::from_str::<Value>(payload_json).unwrap_or(Value::Null),
        });
        self.inner
            .push(EVENT_KIND_MEDIA, 0, envelope.to_string().into_bytes());
    }

    pub fn push_ui(&self, kind: &str, payload_json: &str) {
        let envelope = serde_json::json!({
            "kind": kind,
            "payload": serde_json::from_str::<Value>(payload_json).unwrap_or(Value::Null),
        });
        self.inner
            .push(EVENT_KIND_UI, 0, envelope.to_string().into_bytes());
    }

    pub fn queued_bytes(&self) -> usize {
        let queue = self.inner.queue.lock().unwrap();
        queue
            .events
            .iter()
            .map(|event| EVENT_HEADER_SIZE.saturating_add(event.payload.len()))
            .fold(0usize, usize::saturating_add)
    }

    pub fn next_event_bytes(&self) -> usize {
        self.inner
            .queue
            .lock()
            .unwrap()
            .events
            .front()
            .map(|event| EVENT_HEADER_SIZE.saturating_add(event.payload.len()))
            .unwrap_or(0)
    }

    pub fn dropped_events(&self) -> u64 {
        self.inner.queue.lock().unwrap().dropped
    }

    pub fn query_font_list(&self, monospace: bool, vertical: bool) -> Option<Vec<String>> {
        let lists = self.inner.fonts.lock().unwrap();
        match (monospace, vertical) {
            (false, false) => lists.normal.clone(),
            (true, false) => lists.monospace.clone(),
            (false, true) => lists.vertical.clone(),
            (true, true) => lists.monospace_vertical.clone(),
        }
    }

    pub fn query_window_state(&self) -> Option<(bool, bool)> {
        if !self.enabled() {
            return None;
        }
        let flags = self.inner.window_state.load(Ordering::Relaxed);
        Some((flags & 0b01 != 0, flags & 0b10 != 0))
    }

    pub fn text_replacement(&self, source: &str) -> Option<String> {
        self.inner
            .replacements
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|replacements| replacements.get(source).cloned())
    }

    pub fn text_translation_enabled(&self) -> bool {
        self.enabled() && self.inner.translation_enabled.load(Ordering::Relaxed)
    }

    pub fn set_window_state(&self, flags: i32) {
        self.inner
            .window_state
            .store(flags & 0b11, Ordering::Relaxed);
    }

    pub fn set_text_replacements(&self, replacements: Option<HashMap<String, String>>) {
        *self.inner.replacements.lock().unwrap() = replacements;
    }

    pub fn set_text_translation_enabled(&self, enabled: bool) {
        self.inner
            .translation_enabled
            .store(enabled, Ordering::Relaxed);
    }

    pub fn set_font_list(&self, monospace: bool, vertical: bool, names: Vec<String>) {
        self.inner.set_font_list(monospace, vertical, names);
    }

    pub fn clear_host_state(&self) {
        *self.inner.fonts.lock().unwrap() = FontLists::default();
        self.inner.window_state.store(0, Ordering::Relaxed);
        *self.inner.replacements.lock().unwrap() = None;
        self.inner
            .translation_enabled
            .store(false, Ordering::Relaxed);
    }
}

impl Default for HostEvents {
    fn default() -> Self {
        Self::new()
    }
}

static ACTIVE_HOST_EVENTS: Mutex<Option<Weak<HostEventsInner>>> = Mutex::new(None);

fn activate(events: &HostEvents) {
    *ACTIVE_HOST_EVENTS.lock().unwrap() = Some(Arc::downgrade(&events.inner));
}

fn deactivate(events: &HostEvents) {
    let mut active = ACTIVE_HOST_EVENTS.lock().unwrap();
    let is_current = active
        .as_ref()
        .and_then(Weak::upgrade)
        .is_some_and(|current| Arc::ptr_eq(&current, &events.inner));
    if is_current {
        *active = None;
    }
}

fn active_events() -> Option<HostEvents> {
    let mut active = ACTIVE_HOST_EVENTS.lock().unwrap();
    let events = active.as_ref().and_then(Weak::upgrade);
    if events.is_none() {
        *active = None;
    }
    events.map(|inner| HostEvents { inner })
}

pub fn enabled() -> bool {
    active_events().is_some_and(|events| events.enabled())
}

pub fn push_log(level: &str, message: &str) {
    if let Some(events) = active_events() {
        events.push_log(level, message);
    }
}

pub fn push_media(kind: &str, payload_json: &str) {
    if let Some(events) = active_events() {
        events.push_media(kind, payload_json);
    }
}

pub fn push_ui(kind: &str, payload_json: &str) {
    if let Some(events) = active_events() {
        events.push_ui(kind, payload_json);
    }
}

pub fn query_font_list(monospace: bool, vertical: bool) -> Option<Vec<String>> {
    active_events().and_then(|events| events.query_font_list(monospace, vertical))
}

pub fn query_window_state() -> Option<(bool, bool)> {
    active_events().and_then(|events| events.query_window_state())
}

pub fn text_replacement(source: &str) -> Option<String> {
    active_events().and_then(|events| events.text_replacement(source))
}

pub fn text_translation_enabled() -> bool {
    active_events().is_some_and(|events| events.text_translation_enabled())
}

fn parse_font_list(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect()
}

unsafe fn events_ref<'a>(events: *mut HostEvents) -> Option<&'a HostEvents> {
    if events.is_null() {
        None
    } else {
        Some(unsafe { &*events })
    }
}

pub unsafe extern "C" fn art3m1s_host_events_create() -> *mut HostEvents {
    Box::into_raw(Box::new(HostEvents::new()))
}

pub unsafe extern "C" fn art3m1s_host_events_destroy(events: *mut HostEvents) {
    if let Some(events) = unsafe { events_ref(events) } {
        deactivate(events);
    }
    if !events.is_null() {
        drop(unsafe { Box::from_raw(events) });
    }
}

pub unsafe extern "C" fn art3m1s_host_events_enable_v1(events: *mut HostEvents, enabled: i32) {
    if let Some(events) = unsafe { events_ref(events) } {
        events.set_enabled(enabled != 0);
    }
}

pub unsafe extern "C" fn art3m1s_host_events_next_v1(events: *mut HostEvents) -> usize {
    unsafe { events_ref(events) }
        .map(HostEvents::next_event_bytes)
        .unwrap_or(0)
}

pub unsafe extern "C" fn art3m1s_poll_events_v1(
    events: *mut HostEvents,
    output: *mut u8,
    capacity: usize,
    out_count: *mut u32,
) -> usize {
    if !out_count.is_null() {
        unsafe { *out_count = 0 };
    }
    let Some(events) = (unsafe { events_ref(events) }) else {
        return 0;
    };
    if output.is_null() || capacity == 0 {
        return 0;
    }

    let mut queue = events.inner.queue.lock().unwrap();
    let mut written = 0usize;
    let mut count = 0u32;
    while let Some(event) = queue.events.front() {
        let required = EVENT_HEADER_SIZE.saturating_add(event.payload.len());
        if written.saturating_add(required) > capacity {
            break;
        }
        let event = queue.events.pop_front().unwrap();
        let payload_len = event.payload.len() as u32;
        let mut header = [0u8; EVENT_HEADER_SIZE];
        header[0..4].copy_from_slice(&EVENT_VERSION.to_ne_bytes());
        header[4..8].copy_from_slice(&event.kind.to_ne_bytes());
        header[8..16].copy_from_slice(&event.sequence.to_ne_bytes());
        header[16..20].copy_from_slice(&payload_len.to_ne_bytes());
        header[20..24].copy_from_slice(&event.aux.to_ne_bytes());
        unsafe {
            std::ptr::copy_nonoverlapping(header.as_ptr(), output.add(written), header.len());
        }
        written += EVENT_HEADER_SIZE;
        unsafe {
            std::ptr::copy_nonoverlapping(
                event.payload.as_ptr(),
                output.add(written),
                event.payload.len(),
            );
        }
        written += event.payload.len();
        count = count.saturating_add(1);
    }
    if !out_count.is_null() {
        unsafe { *out_count = count };
    }
    written
}

pub unsafe extern "C" fn art3m1s_set_font_list_v1(
    events: *mut HostEvents,
    monospace: i32,
    vertical: i32,
    data: *const u8,
    len: usize,
) -> i32 {
    let Some(events) = (unsafe { events_ref(events) }) else {
        return 0;
    };
    if data.is_null() && len != 0 {
        return 0;
    }
    let bytes = if len == 0 {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(data, len) }
    };
    events.set_font_list(monospace != 0, vertical != 0, parse_font_list(bytes));
    1
}

pub unsafe extern "C" fn art3m1s_set_window_state_v1(events: *mut HostEvents, flags: i32) {
    if let Some(events) = unsafe { events_ref(events) } {
        events.set_window_state(flags);
    }
}

pub unsafe extern "C" fn art3m1s_set_text_replacements_v1(
    events: *mut HostEvents,
    data: *const u8,
    len: usize,
) -> i32 {
    let Some(events) = (unsafe { events_ref(events) }) else {
        return 0;
    };
    if data.is_null() && len != 0 {
        return 0;
    }
    if len == 0 {
        events.set_text_replacements(Some(HashMap::new()));
        return 1;
    }
    let Ok(bytes) = std::str::from_utf8(unsafe { std::slice::from_raw_parts(data, len) }) else {
        return 0;
    };
    let Ok(Value::Object(values)) = serde_json::from_str::<Value>(bytes) else {
        return 0;
    };
    let replacements = values
        .into_iter()
        .filter_map(|(source, value)| value.as_str().map(|value| (source, value.to_string())))
        .collect();
    events.set_text_replacements(Some(replacements));
    1
}

pub unsafe extern "C" fn art3m1s_set_text_translation_enabled_v1(
    events: *mut HostEvents,
    enabled: i32,
) {
    if let Some(events) = unsafe { events_ref(events) } {
        events.set_text_translation_enabled(enabled != 0);
    }
}

pub unsafe extern "C" fn art3m1s_clear_host_state_v1(events: *mut HostEvents) {
    if let Some(events) = unsafe { events_ref(events) } {
        events.clear_host_state();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn reset() -> HostEvents {
        let events = HostEvents::new();
        events.set_enabled(true);
        events.clear_host_state();
        events
    }

    fn ptr(events: &HostEvents) -> *mut HostEvents {
        events as *const HostEvents as *mut HostEvents
    }

    #[test]
    fn event_encoding_preserves_order_and_payloads() {
        let _guard = TEST_LOCK.lock().unwrap();
        let events = reset();
        events.push_log("W", "warning");
        events.push_media("audio_stop_all", "{}");
        events.push_ui("caption", r#"{"data":"title"}"#);

        let required = events.queued_bytes();
        let mut output = vec![0u8; required];
        let mut count = 0u32;
        let written = unsafe {
            art3m1s_poll_events_v1(ptr(&events), output.as_mut_ptr(), output.len(), &mut count)
        };
        assert_eq!(written, required);
        assert_eq!(count, 3);

        let mut offset = 0usize;
        let first_sequence = u64::from_ne_bytes(output[8..16].try_into().unwrap());
        for (index, kind) in [EVENT_KIND_LOG, EVENT_KIND_MEDIA, EVENT_KIND_UI]
            .into_iter()
            .enumerate()
        {
            let event_kind = u32::from_ne_bytes(output[offset + 4..offset + 8].try_into().unwrap());
            let sequence = u64::from_ne_bytes(output[offset + 8..offset + 16].try_into().unwrap());
            let payload_len =
                u32::from_ne_bytes(output[offset + 16..offset + 20].try_into().unwrap()) as usize;
            assert_eq!(event_kind, kind);
            assert_eq!(sequence, first_sequence + index as u64);
            offset += EVENT_HEADER_SIZE + payload_len;
        }
        assert_eq!(offset, written);
    }

    #[test]
    fn font_window_and_text_state_are_available_without_callbacks() {
        let _guard = TEST_LOCK.lock().unwrap();
        let events = reset();
        let fonts = b"Font A\nFont B";
        assert_eq!(
            unsafe { art3m1s_set_font_list_v1(ptr(&events), 0, 0, fonts.as_ptr(), fonts.len()) },
            1
        );
        assert_eq!(
            events.query_font_list(false, false),
            Some(vec!["Font A".to_string(), "Font B".to_string()])
        );

        unsafe { art3m1s_set_window_state_v1(ptr(&events), 0b11) };
        assert_eq!(events.query_window_state(), Some((true, true)));

        let replacements = br#"{"hello":"translated","bad":1}"#;
        assert_eq!(
            unsafe {
                art3m1s_set_text_replacements_v1(
                    ptr(&events),
                    replacements.as_ptr(),
                    replacements.len(),
                )
            },
            1
        );
        assert_eq!(
            events.text_replacement("hello").as_deref(),
            Some("translated")
        );
        assert_eq!(events.text_replacement("bad"), None);
        unsafe { art3m1s_set_text_translation_enabled_v1(ptr(&events), 1) };
        assert!(events.text_translation_enabled());
    }

    #[test]
    fn full_buffer_polls_complete_events_only() {
        let _guard = TEST_LOCK.lock().unwrap();
        let events = reset();
        events.push_log("I", "first");
        events.push_log("I", "second");
        let one_event = EVENT_HEADER_SIZE + "first".len();
        let mut output = vec![0u8; one_event];
        let mut count = 0u32;
        let written = unsafe {
            art3m1s_poll_events_v1(ptr(&events), output.as_mut_ptr(), output.len(), &mut count)
        };
        assert_eq!(written, one_event);
        assert_eq!(count, 1);
        assert_eq!(
            events.next_event_bytes(),
            EVENT_HEADER_SIZE + "second".len()
        );
    }

    #[test]
    fn enabled_handle_routes_process_events_without_sharing_queue() {
        let _guard = TEST_LOCK.lock().unwrap();
        let first = reset();
        push_log("I", "first");
        assert_eq!(first.next_event_bytes(), EVENT_HEADER_SIZE + "first".len());

        let second = reset();
        push_log("I", "second");
        assert_eq!(first.next_event_bytes(), EVENT_HEADER_SIZE + "first".len());
        assert_eq!(
            second.next_event_bytes(),
            EVENT_HEADER_SIZE + "second".len()
        );

        second.set_enabled(false);
        push_log("I", "dropped");
        assert_eq!(
            second.next_event_bytes(),
            EVENT_HEADER_SIZE + "second".len()
        );
    }

    #[test]
    fn destroying_null_handle_is_a_noop() {
        let _guard = TEST_LOCK.lock().unwrap();
        unsafe { art3m1s_host_events_destroy(std::ptr::null_mut()) };
    }
}
