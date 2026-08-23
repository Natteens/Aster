//! Narrow wall/monotonic clock boundary for `aster.time`.

use std::sync::OnceLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[must_use]
pub extern "C" fn aster_rt_time_monotonic_milliseconds() -> i64 {
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    i64::try_from(ORIGIN.get_or_init(Instant::now).elapsed().as_millis()).unwrap_or(i64::MAX)
}

#[must_use]
pub extern "C" fn aster_rt_time_unix_milliseconds() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => i64::try_from(duration.as_millis()).unwrap_or(i64::MAX),
        Err(error) => -i64::try_from(error.duration().as_millis()).unwrap_or(i64::MAX),
    }
}
