use std::sync::{Arc, Mutex};
use std::time::Duration;

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
    pub fn monotonic_ms(&self) -> u64 {
        self.time.map_or_else(
            || fallback_unix_time().as_millis() as u64,
            |time| time.monotonic_ms,
        )
    }

    pub fn random(&mut self) -> u64 {
        let seed = self
            .random_state
            .unwrap_or_else(|| fallback_unix_time().as_nanos() as u64);
        let value = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.random_state = Some(value);
        value
    }
}

pub(crate) fn fallback_unix_time() -> Duration {
    #[cfg(feature = "system-clock")]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
    }
    #[cfg(not(feature = "system-clock"))]
    Duration::ZERO
}

#[derive(Clone, Copy)]
pub(crate) struct DateTimeParts {
    pub year: i64,
    pub month: i64,
    pub day: i64,
    pub hour: i64,
    pub minute: i64,
    pub second: i64,
}

impl DateTimeParts {
    pub fn from_unix(seconds: i64) -> Self {
        let days = seconds.div_euclid(86_400);
        let secs_of_day = seconds.rem_euclid(86_400);
        let (year, month, day) = civil_from_days(days);
        Self {
            year,
            month,
            day,
            hour: secs_of_day / 3600,
            minute: secs_of_day % 3600 / 60,
            second: secs_of_day % 60,
        }
    }

    pub fn from_local_unix(seconds: i64) -> Self {
        #[cfg(all(unix, feature = "system-clock"))]
        {
            let raw = seconds as libc::time_t;
            let mut tm = std::mem::MaybeUninit::<libc::tm>::uninit();
            let ptr = unsafe { libc::localtime_r(&raw, tm.as_mut_ptr()) };
            if !ptr.is_null() {
                let tm = unsafe { tm.assume_init() };
                return Self {
                    year: i64::from(tm.tm_year) + 1900,
                    month: i64::from(tm.tm_mon) + 1,
                    day: i64::from(tm.tm_mday),
                    hour: i64::from(tm.tm_hour),
                    minute: i64::from(tm.tm_min),
                    second: i64::from(tm.tm_sec),
                };
            }
        }
        Self::from_unix(seconds)
    }
}

fn civil_from_days(days_since_epoch: i64) -> (i64, i64, i64) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = y + if month <= 2 { 1 } else { 0 };
    (year, month, day)
}

pub(crate) fn utc_components_from_seconds(seconds: i64) -> [i64; 6] {
    let p = DateTimeParts::from_unix(seconds);
    [p.year, p.month, p.day, p.hour, p.minute, p.second]
}
