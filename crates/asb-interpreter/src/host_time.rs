use std::sync::{Arc, Mutex};

/// A Host clock snapshot. Monotonic milliseconds drive script deadlines;
/// calendar time and the explicit UTC offset drive date queries.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HostTime {
    pub monotonic_ms: u64,
    pub utc_unix_ms: i64,
    pub utc_offset_minutes: i32,
}

impl HostTime {
    pub fn validate(self) -> crate::Result<()> {
        if !(-1440..=1440).contains(&self.utc_offset_minutes) {
            return Err(crate::Error::RuntimeError {
                line: 0,
                message: "UTC offset must be between -1440 and 1440 minutes".into(),
            });
        }
        Ok(())
    }

    pub(crate) fn local_unix_seconds(self, utc_unix_ms: i64) -> i64 {
        utc_unix_ms.div_euclid(1000) + i64::from(self.utc_offset_minutes) * 60
    }
}

#[derive(Debug, Default)]
pub(crate) struct ScriptEnvironment {
    pub time: Option<HostTime>,
    pub random_state: Option<u64>,
    pub message_layer_serial: u64,
}

pub(crate) type SharedEnvironment = Arc<Mutex<ScriptEnvironment>>;

impl ScriptEnvironment {
    pub fn random(&mut self) -> u64 {
        let seed = self.random_state.unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|time| time.as_nanos() as u64)
                .unwrap_or(0)
        });
        let value = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.random_state = Some(value);
        value
    }
}
