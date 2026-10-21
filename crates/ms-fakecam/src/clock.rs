//! Injectable millisecond clock, so tests and the harness stay deterministic.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

/// Source of the current time in milliseconds.
pub trait Clock: Send + Sync {
    /// Milliseconds since an arbitrary, fixed origin.
    fn now_ms(&self) -> u64;
}

/// Clock that only moves when told to; clones share the same time.
/// A mutex rather than `AtomicU64`, which 32-bit MIPS does not provide.
#[derive(Debug, Clone, Default)]
pub struct ManualClock(Arc<Mutex<u64>>);

impl ManualClock {
    /// A clock starting at `ms`.
    pub fn new(ms: u64) -> Self {
        Self(Arc::new(Mutex::new(ms)))
    }

    /// Moves the clock forward by `ms`.
    pub fn advance(&self, ms: u64) {
        let mut now = self.lock();
        *now = now.wrapping_add(ms);
    }

    /// Sets the absolute time.
    pub fn set(&self, ms: u64) {
        *self.lock() = ms;
    }

    fn lock(&self) -> MutexGuard<'_, u64> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> u64 {
        *self.lock()
    }
}

/// Wall-clock time elapsed since the clock was created.
#[derive(Debug, Clone)]
pub struct MonotonicClock(Instant);

impl Default for MonotonicClock {
    fn default() -> Self {
        Self(Instant::now())
    }
}

impl Clock for MonotonicClock {
    fn now_ms(&self) -> u64 {
        self.0.elapsed().as_millis() as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_clock_is_shared_between_clones() {
        let a = ManualClock::new(5);
        let b = a.clone();
        a.advance(10);
        assert_eq!(b.now_ms(), 15);
        b.set(3);
        assert_eq!(a.now_ms(), 3);
    }
}
