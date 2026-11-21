#[doc(hidden)]
pub fn emit(level: &str, message: String) {
    use art3m1s_log::{Level, Record};

    if let Some(logger) = art3m1s_log::global() {
        let level = match level {
            "E" => Level::Error,
            "W" => Level::Warn,
            "D" => Level::Debug,
            _ => Level::Info,
        };
        logger.log(Record::new(level, "art3m1s", message));
    }
}

macro_rules! core_info {
    ($($arg:tt)*) => { $crate::__core_log("I", format!($($arg)*)); };
}
macro_rules! core_warn {
    ($($arg:tt)*) => { $crate::__core_log("W", format!($($arg)*)); };
}
pub(crate) use {core_info, core_warn};
