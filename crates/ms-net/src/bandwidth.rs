//! Process-wide bandwidth counters exposed by `/bandwidth.json`.
//!
//! C++ keeps the per-window counters in globals incremented by the adapter and
//! stream threads and folds them into the `c_*` snapshot once per second in
//! `calculate_bw` (stream.cpp:661-667, 1287-1327). The API reads the snapshot
//! through `get_json_bandwidth` (api/variables.cpp:305-324).

use std::sync::Mutex;
use std::time::Instant;

/// The values printed by `/bandwidth.json` (`c_*` in C++).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BandwidthSnapshot {
    /// Bytes written per second in KiB (`c_bw`).
    pub bw: i64,
    /// Bytes read from the adapters per second in KiB (`c_bw_dmx`).
    pub dmx: i64,
    /// Accumulated written bytes in units of 1024576 (`c_tbw`).
    pub tbw: i64,
    /// Reads in the last window, at least one (`c_reads`).
    pub reads: u32,
    /// Writes in the last window (`c_writes`).
    pub writes: u32,
    /// Failed writes in the last window (`c_failed_writes`).
    pub fwrites: u32,
    /// Microseconds per read (`c_ns_read`).
    pub ns_read: i64,
    /// Microseconds spent reading (`c_tt`).
    pub tt: i64,
    /// Bytes buffered in the last window (`c_buffered`).
    pub buffered: i64,
    /// Bytes dropped in the last window (`c_dropped`).
    pub dropped: i64,
}

#[derive(Debug, Default)]
struct Window {
    bw: i64,
    bw_dmx: i64,
    tbw: i64,
    reads: u32,
    writes: u32,
    failed_writes: u32,
    nsecs: i64,
    buffered: i64,
    dropped: i64,
}

#[derive(Debug)]
struct Inner {
    window: Window,
    snapshot: BandwidthSnapshot,
    /// `bwtt`: last time the window was folded into the snapshot.
    bwtt_ms: i64,
    origin: Instant,
}

/// Shared counters handed to every data-plane thread and the web endpoints.
#[derive(Debug)]
pub struct Bandwidth {
    inner: Mutex<Inner>,
}

impl Default for Bandwidth {
    fn default() -> Self {
        Self::new()
    }
}

impl Bandwidth {
    /// Creates a counter set whose clock starts now.
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                window: Window::default(),
                snapshot: BandwidthSnapshot::default(),
                bwtt_ms: 0,
                origin: Instant::now(),
            }),
        }
    }

    /// Milliseconds since this counter set was created (`getTick`).
    pub fn now_ms(&self) -> i64 {
        self.inner
            .lock()
            .map(|inner| inner.origin.elapsed().as_millis() as i64)
            .unwrap_or(0)
    }

    /// Records one tuner read (`bw_dmx += rlen`, `nsecs`, `reads`;
    /// stream.cpp:1172,1193-1194).
    pub fn record_read(&self, bytes: usize, elapsed_us: i64) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.window.bw_dmx += bytes as i64;
            inner.window.nsecs += elapsed_us;
            inner.window.reads = inner.window.reads.wrapping_add(1);
        }
    }

    /// Records a successful output flush (`bw += rv, writes++`; stream.cpp:825-826).
    pub fn record_write(&self, bytes: usize) {
        self.record_writes(bytes as u64, 1);
    }

    /// Records a batch of successful flushes.
    pub fn record_writes(&self, bytes: u64, writes: u32) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.window.bw += bytes as i64;
            inner.window.writes = inner.window.writes.wrapping_add(writes);
        }
    }

    /// Records a failed write (`failed_writes++`; stream.cpp:828).
    pub fn record_failed_write(&self) {
        self.record_failed_writes(1);
    }

    /// Records a batch of failed writes.
    pub fn record_failed_writes(&self, count: u32) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.window.failed_writes = inner.window.failed_writes.wrapping_add(count);
        }
    }

    /// Adds bytes copied into a FIFO in this window (`buffered_bytes += copied`;
    /// socketworks.cpp:1505-1506). `calculate_bw` copies the accumulator into
    /// `c_buffered` and resets it (stream.cpp:1308,1325).
    pub fn record_buffered(&self, bytes: u64) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.window.buffered += bytes as i64;
        }
    }

    /// Adds to the window's dropped-byte counter (`dropped_bytes`).
    pub fn record_dropped(&self, bytes: u64) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.window.dropped += bytes as i64;
        }
    }

    /// Folds the window into the snapshot once per second
    /// (`calculate_bw`, stream.cpp:1291-1327).
    pub fn calculate(&self) {
        if let Ok(mut inner) = self.inner.lock() {
            let now = inner.origin.elapsed().as_millis() as i64;
            if inner.bwtt_ms > now {
                inner.bwtt_ms = now;
            }
            if now - inner.bwtt_ms <= 1000 {
                return;
            }
            inner.bwtt_ms = now;
            let window = &mut inner.window;
            window.tbw += window.bw;
            if window.reads == 0 {
                window.reads = 1;
            }
            let busy = window.bw > 2000 || window.bw_dmx > 2000;
            let next = busy.then(|| BandwidthSnapshot {
                bw: window.bw / 1024,
                dmx: window.bw_dmx / 1024,
                tbw: window.tbw / 1_024_576,
                reads: window.reads,
                writes: window.writes,
                fwrites: window.failed_writes,
                ns_read: window.nsecs / i64::from(window.reads),
                tt: window.nsecs / 1000,
                buffered: window.buffered,
                dropped: window.dropped,
            });
            *window = Window::default();
            if let Some(snapshot) = next {
                inner.snapshot = snapshot;
            }
        }
    }

    /// Latest folded snapshot.
    pub fn snapshot(&self) -> BandwidthSnapshot {
        self.inner
            .lock()
            .map(|inner| inner.snapshot)
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;
    use std::time::Duration;

    #[test]
    fn counters_fold_after_one_second() {
        let bandwidth = Bandwidth::new();
        bandwidth.record_read(8192, 500);
        bandwidth.record_write(4096);
        sleep(Duration::from_millis(1100));
        bandwidth.calculate();
        let snapshot = bandwidth.snapshot();
        // calculate_bw divides by 1024 (stream.cpp:1300-1301).
        assert_eq!(snapshot.dmx, 8);
        assert_eq!(snapshot.bw, 4);
        assert_eq!(snapshot.reads, 1);
        assert_eq!(snapshot.writes, 1);
        assert_eq!(snapshot.ns_read, 500);
        assert_eq!(snapshot.tt, 0);
    }

    #[test]
    fn idle_window_keeps_previous_snapshot_and_forces_reads() {
        let bandwidth = Bandwidth::new();
        bandwidth.record_read(4096, 100);
        sleep(Duration::from_millis(1100));
        bandwidth.calculate();
        assert_eq!(bandwidth.snapshot().dmx, 4);
        // No traffic in the next window: the snapshot is not updated but
        // `tbw` still accumulates (stream.cpp:1296-1310).
        sleep(Duration::from_millis(1100));
        bandwidth.calculate();
        assert_eq!(bandwidth.snapshot().dmx, 4);
        // A later busy window folds again.
        bandwidth.record_read(2048, 200);
        sleep(Duration::from_millis(1100));
        bandwidth.calculate();
        assert_eq!(bandwidth.snapshot().dmx, 2);
        assert_eq!(bandwidth.snapshot().reads, 1);
    }

    #[test]
    fn buffered_counts_fifo_copies_and_resets_each_window() {
        // `buffered_bytes += copied` per window, copied into `c_buffered` and
        // cleared by `calculate_bw` (socketworks.cpp:1505-1506;
        // stream.cpp:1308,1325).
        let bandwidth = Bandwidth::new();
        bandwidth.record_read(8192, 0);
        bandwidth.record_buffered(4096);
        bandwidth.record_buffered(1024);
        sleep(Duration::from_millis(1100));
        bandwidth.calculate();
        assert_eq!(bandwidth.snapshot().buffered, 5120);

        // No new FIFO copies: the accumulator stays at zero even though the
        // snapshot is refreshed by the busier read counter.
        bandwidth.record_read(8192, 0);
        sleep(Duration::from_millis(1100));
        bandwidth.calculate();
        assert_eq!(bandwidth.snapshot().buffered, 0);
    }

    #[test]
    fn small_windows_are_reset_without_updating_snapshot() {
        let bandwidth = Bandwidth::new();
        bandwidth.record_write(100);
        sleep(Duration::from_millis(1100));
        bandwidth.calculate();
        // `bw` is not above 2000 and `bw_dmx` is zero, so `c_*` keeps its
        // previous value, but the window was cleared (stream.cpp:1298-1326).
        assert_eq!(bandwidth.snapshot().bw, 0);
        assert_eq!(bandwidth.snapshot().tbw, 0);
    }
}
