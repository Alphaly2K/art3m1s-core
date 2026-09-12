//! Reusable logging boundary shared by Art3m1s core, render, and engine hosts.
//!
//! This crate intentionally has no FFI, GPU, interpreter, or platform
//! dependency. Host integrations install a [`Sink`]; the [`Facade`] bridges the
//! standard `log` crate into that sink.
#![forbid(unsafe_code)]

use std::cell::Cell;
use std::fmt;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, OnceLock, RwLock};

/// Stable Art3m1s log level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum Level {
    Error = 0,
    Warn = 1,
    Info = 2,
    Debug = 3,
    Trace = 4,
}

impl Level {
    /// Stable one-character code used by the existing Flutter callback.
    pub const fn ffi_code(self) -> &'static str {
        match self {
            Self::Error => "E",
            Self::Warn => "W",
            Self::Info => "I",
            Self::Debug => "D",
            Self::Trace => "T",
        }
    }

    fn from_log_level(level: log::Level) -> Self {
        match level {
            log::Level::Error => Self::Error,
            log::Level::Warn => Self::Warn,
            log::Level::Info => Self::Info,
            log::Level::Debug => Self::Debug,
            log::Level::Trace => Self::Trace,
        }
    }

    fn from_u8(value: u8) -> Self {
        match value {
            0 => Self::Error,
            1 => Self::Warn,
            2 => Self::Info,
            3 => Self::Debug,
            _ => Self::Trace,
        }
    }

    const fn to_u8(self) -> u8 {
        self as u8
    }
}

/// Fully formatted record delivered to a [`Sink`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    level: Level,
    target: String,
    message: String,
    file: Option<String>,
    line: Option<u32>,
}

impl Record {
    pub fn new(level: Level, target: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            level,
            target: target.into(),
            message: message.into(),
            file: None,
            line: None,
        }
    }

    pub fn with_source(mut self, file: Option<impl Into<String>>, line: Option<u32>) -> Self {
        self.file = file.map(Into::into);
        self.line = line;
        self
    }

    pub fn level(&self) -> Level {
        self.level
    }

    pub fn target(&self) -> &str {
        &self.target
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn file(&self) -> Option<&str> {
        self.file.as_deref()
    }

    pub fn line(&self) -> Option<u32> {
        self.line
    }
}

/// Destination for formatted log records.
pub trait Sink: Send + Sync + 'static {
    fn log(&self, record: &Record);

    fn flush(&self) {}
}

/// Optional record filter evaluated before a sink is called.
pub trait Filter: Send + Sync + 'static {
    fn enabled(&self, _level: Level, _target: &str) -> bool {
        true
    }

    /// Return true to suppress the record.
    fn suppress(&self, _record: &Record) -> bool {
        false
    }
}

/// Cloneable logging handle with replaceable sink and filter.
#[derive(Clone)]
pub struct Logger {
    inner: Arc<LoggerInner>,
}

struct LoggerInner {
    min_level: AtomicU8,
    sink: RwLock<Option<Arc<dyn Sink>>>,
    filter: RwLock<Option<Arc<dyn Filter>>>,
}

impl fmt::Debug for Logger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Logger")
            .field("min_level", &self.level())
            .field("has_sink", &self.inner.sink.read().unwrap().is_some())
            .field("has_filter", &self.inner.filter.read().unwrap().is_some())
            .finish()
    }
}

impl Default for Logger {
    fn default() -> Self {
        Self::new()
    }
}

impl Logger {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(LoggerInner {
                min_level: AtomicU8::new(Level::Info.to_u8()),
                sink: RwLock::new(None),
                filter: RwLock::new(None),
            }),
        }
    }

    pub fn with_sink(sink: Arc<dyn Sink>) -> Self {
        let logger = Self::new();
        logger.set_sink(Some(sink));
        logger
    }

    pub fn level(&self) -> Level {
        Level::from_u8(self.inner.min_level.load(Ordering::Relaxed))
    }

    pub fn set_level(&self, level: Level) {
        self.inner.min_level.store(level.to_u8(), Ordering::Relaxed);
    }

    pub fn set_sink(&self, sink: Option<Arc<dyn Sink>>) -> Option<Arc<dyn Sink>> {
        std::mem::replace(&mut *self.inner.sink.write().unwrap(), sink)
    }

    pub fn set_filter(&self, filter: Option<Arc<dyn Filter>>) -> Option<Arc<dyn Filter>> {
        std::mem::replace(&mut *self.inner.filter.write().unwrap(), filter)
    }

    pub fn enabled(&self, level: Level, target: &str) -> bool {
        if level > self.level() {
            return false;
        }

        if IN_FILTER.with(Cell::get) {
            return true;
        }

        let filter = self.inner.filter.read().unwrap().clone();
        let Some(filter) = filter else {
            return true;
        };

        let _guard = FilterGuard::new();
        filter.enabled(level, target)
    }

    pub fn log(&self, record: Record) {
        if !self.enabled(record.level, &record.target) || self.filter_suppresses(&record) {
            return;
        }
        let sink = self.inner.sink.read().unwrap().clone();
        if let Some(sink) = sink {
            sink.log(&record);
        }
    }

    pub fn flush(&self) {
        let sink = self.inner.sink.read().unwrap().clone();
        if let Some(sink) = sink {
            sink.flush();
        }
    }

    fn filter_suppresses(&self, record: &Record) -> bool {
        if IN_FILTER.with(Cell::get) {
            return false;
        }

        let filter = self.inner.filter.read().unwrap().clone();
        let Some(filter) = filter else {
            return false;
        };

        let _guard = FilterGuard::new();
        filter.suppress(record)
    }
}

thread_local! {
    static IN_FILTER: Cell<bool> = const { Cell::new(false) };
}

struct FilterGuard;

impl FilterGuard {
    fn new() -> Self {
        IN_FILTER.with(|flag| flag.set(true));
        Self
    }
}

impl Drop for FilterGuard {
    fn drop(&mut self) {
        IN_FILTER.with(|flag| flag.set(false));
    }
}

static GLOBAL_LOGGER: OnceLock<Arc<Logger>> = OnceLock::new();

/// Installs the process-wide standard `log` facade.
///
/// Sink and filter replacement remain possible through the returned
/// [`Logger`]; the global facade itself is installed only once.
pub fn install_global(logger: Arc<Logger>) -> Result<(), InstallError> {
    if GLOBAL_LOGGER.get().is_some() {
        return Err(InstallError::AlreadyInstalled);
    }

    if let Err(error) = log::set_boxed_logger(Box::new(Facade {
        logger: Arc::clone(&logger),
    })) {
        return Err(InstallError::Log(error.to_string()));
    }
    log::set_max_level(log::LevelFilter::Trace);
    GLOBAL_LOGGER
        .set(logger)
        .map_err(|_| InstallError::AlreadyInstalled)?;
    Ok(())
}

pub fn global() -> Option<&'static Arc<Logger>> {
    GLOBAL_LOGGER.get()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallError {
    AlreadyInstalled,
    Log(String),
}

impl fmt::Display for InstallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyInstalled => f.write_str("Art3m1s logger is already installed"),
            Self::Log(message) => write!(f, "failed to install Art3m1s logger: {message}"),
        }
    }
}

impl std::error::Error for InstallError {}

struct Facade {
    logger: Arc<Logger>,
}

impl log::Log for Facade {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        self.logger
            .enabled(Level::from_log_level(metadata.level()), metadata.target())
    }

    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }

        let record = Record {
            level: Level::from_log_level(record.level()),
            target: record.target().to_owned(),
            message: record.args().to_string(),
            file: record.file().map(str::to_owned),
            line: record.line(),
        };
        self.logger.log(record);
    }

    fn flush(&self) {
        self.logger.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::{Filter, Level, Logger, Record, Sink};
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct Capture {
        records: Mutex<Vec<Record>>,
        flushes: Mutex<usize>,
    }

    impl Sink for Capture {
        fn log(&self, record: &Record) {
            self.records.lock().unwrap().push(record.clone());
        }

        fn flush(&self) {
            *self.flushes.lock().unwrap() += 1;
        }
    }

    struct SuppressDebug;

    impl Filter for SuppressDebug {
        fn suppress(&self, record: &Record) -> bool {
            record.level() == Level::Debug
        }
    }

    #[test]
    fn sink_receives_formatted_record() {
        let capture = Arc::new(Capture::default());
        let logger = Logger::with_sink(capture.clone());

        logger.log(Record::new(Level::Info, "render", "frame ready"));

        let records = capture.records.lock().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].level(), Level::Info);
        assert_eq!(records[0].target(), "render");
        assert_eq!(records[0].message(), "frame ready");
    }

    #[test]
    fn level_filter_drops_lower_priority_records() {
        let capture = Arc::new(Capture::default());
        let logger = Logger::with_sink(capture.clone());
        logger.set_level(Level::Info);

        logger.log(Record::new(Level::Debug, "render", "debug"));
        logger.log(Record::new(Level::Warn, "render", "warn"));

        let records = capture.records.lock().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].level(), Level::Warn);
    }

    #[test]
    fn filter_can_suppress_records() {
        let capture = Arc::new(Capture::default());
        let logger = Logger::with_sink(capture.clone());
        logger.set_level(Level::Trace);
        logger.set_filter(Some(Arc::new(SuppressDebug)));

        logger.log(Record::new(Level::Debug, "render", "debug"));
        logger.log(Record::new(Level::Info, "render", "info"));

        let records = capture.records.lock().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].level(), Level::Info);
    }

    #[test]
    fn sink_can_be_replaced_without_reinstalling_facade() {
        let first = Arc::new(Capture::default());
        let second = Arc::new(Capture::default());
        let logger = Logger::with_sink(first.clone());

        logger.log(Record::new(Level::Info, "core", "first"));
        logger.set_sink(Some(second.clone()));
        logger.log(Record::new(Level::Info, "core", "second"));

        assert_eq!(first.records.lock().unwrap().len(), 1);
        let records = second.records.lock().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].message(), "second");
    }

    #[test]
    fn flush_reaches_current_sink() {
        let capture = Arc::new(Capture::default());
        let logger = Logger::with_sink(capture.clone());

        logger.flush();

        assert_eq!(*capture.flushes.lock().unwrap(), 1);
    }
}
