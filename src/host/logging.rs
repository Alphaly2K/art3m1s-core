//! Bridge from the shared logging facade into host event transport.

use std::sync::{Arc, Once};

use art3m1s_log::{Logger, Record, Sink};

struct HostEventSink;

impl Sink for HostEventSink {
    fn log(&self, record: &Record) {
        #[cfg(feature = "rfvp-engine")]
        if art3m1s_rfvp::host_runtime::is_replay_worker_thread() {
            // A player call can be waiting on this worker with its handle locked.
            crate::ffi::rfvp_api::enqueue_log(record.level().ffi_code(), record.message());
            return;
        }
        crate::ffi::log(record.level().ffi_code(), record.message());
        #[cfg(feature = "rfvp-engine")]
        crate::ffi::rfvp_api::dispatch_log(record.level().ffi_code(), record.message());
        #[cfg(feature = "siglus-engine")]
        crate::ffi::siglus_api::dispatch_log(record);
    }
}

static INSTALL: Once = Once::new();

pub(crate) fn install_once() {
    INSTALL.call_once(|| {
        let logger = Arc::new(Logger::with_sink(Arc::new(HostEventSink)));
        let _ = art3m1s_log::install_global(logger);
    });
}
