//! Bridge from the shared logging facade into host event transport.

use std::sync::{Arc, Once};

use art3m1s_log::{Logger, Record, Sink};

struct HostEventSink;

impl Sink for HostEventSink {
    fn log(&self, record: &Record) {
        crate::ffi::log(record.level().ffi_code(), record.message());
    }
}

static INSTALL: Once = Once::new();

pub(crate) fn install_once() {
    INSTALL.call_once(|| {
        let logger = Arc::new(Logger::with_sink(Arc::new(HostEventSink)));
        let _ = art3m1s_log::install_global(logger);
    });
}
