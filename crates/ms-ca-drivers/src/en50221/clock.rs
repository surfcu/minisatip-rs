//! Time sources: `getTick()` milliseconds and `time(NULL)` for DATE_TIME.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
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
#[derive(Debug, Clone, Default)]
pub struct ManualClock {
    tick: Arc<AtomicI64>,
    unix: Arc<AtomicU64>,
}

impl ManualClock {
    pub fn new(tick_ms: i64, unix: u64) -> Self {
        Self {
            tick: Arc::new(AtomicI64::new(tick_ms)),
            unix: Arc::new(AtomicU64::new(unix)),
        }
    }

    /// Moves the tick forward by `ms`; the Unix time only moves with `set_unix`.
    pub fn advance(&self, ms: i64) {
        self.tick.fetch_add(ms, Ordering::SeqCst);
    }

    pub fn set_tick(&self, ms: i64) {
        self.tick.store(ms, Ordering::SeqCst);
    }

    pub fn set_unix(&self, secs: u64) {
        self.unix.store(secs, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn tick_ms(&self) -> i64 {
        self.tick.load(Ordering::SeqCst)
    }

    fn unix_time(&self) -> u64 {
        self.unix.load(Ordering::SeqCst)
    }
}
