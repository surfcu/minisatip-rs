//! Time sources: `getTick()` milliseconds and `time(NULL)` for DATE_TIME.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub trait Clock: Send + Sync {
    /// Monotonic milliseconds, like `getTick()`.
    fn tick_ms(&self) -> i64;
    /// Seconds since the Unix epoch, like `time(NULL)`.
    fn unix_time(&self) -> u64;
}

/// Real time; ticks start at 0 when the clock is created.
#[derive(Debug, Clone)]
pub struct SystemClock(Instant);

impl Default for SystemClock {
    fn default() -> Self {
        Self(Instant::now())
    }
}

impl Clock for SystemClock {
    fn tick_ms(&self) -> i64 {
        self.0.elapsed().as_millis() as i64
    }

    fn unix_time(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
    }
}

/// Clock moved only by tests; clones share the same time.
/// A mutex rather than 64-bit atomics, which 32-bit MIPS does not provide.
#[derive(Debug, Clone, Default)]
pub struct ManualClock(Arc<Mutex<ManualTime>>);

#[derive(Debug, Default)]
struct ManualTime {
    tick: i64,
    unix: u64,
}

impl ManualClock {
    pub fn new(tick_ms: i64, unix: u64) -> Self {
        Self(Arc::new(Mutex::new(ManualTime {
            tick: tick_ms,
            unix,
        })))
    }

    /// Moves the tick forward by `ms`; the Unix time only moves with `set_unix`.
    pub fn advance(&self, ms: i64) {
        let mut time = self.lock();
        time.tick = time.tick.wrapping_add(ms);
    }

    pub fn set_tick(&self, ms: i64) {
        self.lock().tick = ms;
    }

    pub fn set_unix(&self, secs: u64) {
        self.lock().unix = secs;
    }

    fn lock(&self) -> MutexGuard<'_, ManualTime> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Clock for ManualClock {
    fn tick_ms(&self) -> i64 {
        self.lock().tick
    }

    fn unix_time(&self) -> u64 {
        self.lock().unix
    }
}
