//! `--ddci-capture` (diag-log only): rolling TS buffers per DDCI device and
//! direction. An MMI menu or enquiry writes the last 60 s plus the next 60 s as
//! `ca<slot>-<UTC time>-{in,out}.ts` and a JSON sidecar. Bounds: docs/logging-setup.md.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fs::{self, File};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::thread;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use ms_ca_drivers::ddci::CiWriter;
use ms_tuner::{PidSet, SignalStatus, TsBuf, TuneParams, Tuner, TunerError};

/// Length of the rolling window and of the capture that follows a trigger.
const WINDOW_MS: u64 = 60_000;
/// Hard bound on each ring (per device and direction); a capture holds at most as much again.
pub const RING_BYTES: usize = 160 << 20;
/// Minimum time between two accepted triggers for one slot.
const MIN_INTERVAL_MS: u64 = 600_000;
/// Messages queued for the writer thread.
const QUEUE_MESSAGES: usize = 4096;
/// Bytes queued for the writer; a full queue drops data and counts it.
const QUEUE_BYTES: usize = 2 * RING_BYTES;
/// File suffix per direction: 0 is the CAM input, 1 the CAM output.
const DIR_NAMES: [&str; 2] = ["in", "out"];

#[derive(Clone)]
struct Chunk {
    mono_ms: u64,
    data: Arc<[u8]>,
}

#[derive(Default)]
struct Ring {
    chunks: VecDeque<Chunk>,
    bytes: usize,
}

impl Ring {
    /// Appends `chunk`, then drops the oldest chunks past the window or the byte bound.
    fn push(&mut self, chunk: Chunk) {
        self.bytes += chunk.data.len();
        let now = chunk.mono_ms;
        self.chunks.push_back(chunk);
        while let Some(front) = self.chunks.front() {
            let old = now.saturating_sub(front.mono_ms) > WINDOW_MS;
            if !old && self.bytes <= RING_BYTES {
                break;
            }
            let len = front.data.len();
            self.chunks.pop_front();
            self.bytes -= len;
        }
    }
}

struct Capture {
    id: u64,
    dev: usize,
    end_ms: u64,
    post_bytes: [usize; 2],
    truncated: [bool; 2],
    drops: [u64; 2],
    short_writes_at_start: u64,
}

#[derive(Default)]
struct TapState {
    rings: BTreeMap<(usize, usize), Ring>,
    captures: Vec<Capture>,
    last_accepted: BTreeMap<usize, u64>,
    short_writes: BTreeMap<usize, u64>,
    next_id: u64,
}

enum Msg {
    Start {
        id: u64,
        base: String,
        slot: usize,
        kind: &'static str,
        pid_map: Vec<(u16, u16)>,
        trigger_wall_ms: i64,
        trigger_mono_ms: u64,
    },
    Chunks {
        id: u64,
        dir: usize,
        bytes: usize,
        chunks: Vec<Chunk>,
    },
    Finish {
        id: u64,
        drops: [u64; 2],
        truncated: [bool; 2],
        short_writes: u64,
    },
}

/// The shared capture state: rings fed from the TS path, triggers and ticks from the control thread.
pub struct CaptureTap {
    dir: PathBuf,
    max_bytes: u64,
    origin: Instant,
    state: Mutex<TapState>,
    tx: mpsc::SyncSender<Msg>,
    queued: Arc<AtomicUsize>,
}

impl CaptureTap {
    /// Creates `dir` and starts the writer thread; `None` when either fails.
    pub fn new(dir: &str, max_mb: u64) -> Option<Arc<Self>> {
        let dir = PathBuf::from(dir);
        if let Err(error) = fs::create_dir_all(&dir) {
            ms_log::log!("ddci_capture: cannot create {}: {}", dir.display(), error);
            return None;
        }
        let (tx, rx) = mpsc::sync_channel(QUEUE_MESSAGES);
        let queued = Arc::new(AtomicUsize::new(0));
        let writer_queued = Arc::clone(&queued);
        let writer_dir = dir.clone();
        let spawned = thread::Builder::new()
            .name("ddci-capture".into())
            .spawn(move || writer_loop(rx, &writer_queued, &writer_dir));
        if let Err(error) = spawned {
            ms_log::log!("ddci_capture: cannot start the writer thread: {}", error);
            return None;
        }
        Some(Arc::new(Self {
            dir,
            max_bytes: max_mb.saturating_mul(1 << 20),
            origin: Instant::now(),
            state: Mutex::new(TapState::default()),
            tx,
            queued,
        }))
    }

    /// Wraps a CI writer so the bytes it accepts are recorded as CAM input.
    pub fn writer(
        self: &Arc<Self>,
        dev: usize,
        inner: Box<dyn CiWriter + Send>,
    ) -> Box<dyn CiWriter + Send> {
        Box::new(TapWriter {
            tap: Arc::clone(self),
            dev,
            inner,
        })
    }

    /// Wraps a CAM output reader so the packets it appends are recorded as CAM output.
    pub fn reader(
        self: &Arc<Self>,
        dev: usize,
        inner: Box<dyn Tuner + Send>,
    ) -> Box<dyn Tuner + Send> {
        Box::new(TapReader {
            tap: Arc::clone(self),
            dev,
            inner,
        })
    }

    fn state(&self) -> MutexGuard<'_, TapState> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn now_ms(&self) -> u64 {
        u64::try_from(self.origin.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// Queues one writer message; `false` when the queue is full or closed.
    fn queue(&self, msg: Msg, bytes: usize) -> bool {
        if self.queued.fetch_add(bytes, Ordering::AcqRel) + bytes > QUEUE_BYTES {
            self.queued.fetch_sub(bytes, Ordering::AcqRel);
            return false;
        }
        if self.tx.try_send(msg).is_ok() {
            return true;
        }
        self.queued.fetch_sub(bytes, Ordering::AcqRel);
        false
    }

    /// Copies `parts` into one chunk for `dev`/`dir` (0 in, 1 out).
    fn record(&self, dev: usize, dir: usize, parts: &[&[u8]]) {
        self.record_at(dev, dir, parts, self.now_ms());
    }

    fn record_at(&self, dev: usize, dir: usize, parts: &[&[u8]], now: u64) {
        let len: usize = parts.iter().map(|p| p.len()).sum();
        if len == 0 {
            return;
        }
        // The copy happens before the lock; the lock only moves Arc handles.
        let mut bytes = Vec::with_capacity(len);
        for part in parts {
            bytes.extend_from_slice(part);
        }
        let chunk = Chunk {
            mono_ms: now,
            data: Arc::from(bytes),
        };
        let mut guard = self.state();
        let st = &mut *guard;
        st.rings.entry((dev, dir)).or_default().push(chunk.clone());
        for cap in st
            .captures
            .iter_mut()
            .filter(|c| c.dev == dev && now < c.end_ms)
        {
            if cap.post_bytes[dir] + len > RING_BYTES {
                cap.truncated[dir] = true;
                continue;
            }
            cap.post_bytes[dir] += len;
            let msg = Msg::Chunks {
                id: cap.id,
                dir,
                bytes: len,
                chunks: vec![chunk.clone()],
            };
            if !self.queue(msg, len) {
                cap.drops[dir] += len as u64;
            }
        }
    }

    /// Counts a `writev` that accepted fewer bytes than offered.
    fn note_short_write(&self, dev: usize) {
        *self.state().short_writes.entry(dev).or_default() += 1;
    }

    /// An MMI menu or enquiry from the CAM on `dev`. `pid_map` is the (input, DDCI) PID remap.
    pub fn trigger(&self, dev: usize, kind: &'static str, pid_map: Vec<(u16, u16)>) {
        let wall_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
        self.trigger_at(dev, kind, pid_map, self.now_ms(), wall_ms);
    }

    fn trigger_at(
        &self,
        dev: usize,
        kind: &'static str,
        pid_map: Vec<(u16, u16)>,
        now: u64,
        wall_ms: i64,
    ) {
        // The directory scan runs before the lock: the TS path may wait on it.
        let used = dir_bytes(&self.dir);
        let mut guard = self.state();
        let st = &mut *guard;
        if st
            .last_accepted
            .get(&dev)
            .is_some_and(|&last| now < last + MIN_INTERVAL_MS)
        {
            ms_log::log!(
                "ddci_capture: CA {} {} ignored, under 10 min since the last capture",
                dev,
                kind
            );
            return;
        }
        if st.captures.iter().any(|c| c.dev == dev) {
            return;
        }
        if used >= self.max_bytes {
            ms_log::log!(
                "ddci_capture: {} holds {} bytes, at or over --ddci-capture-max-mb; no capture for CA {}",
                self.dir.display(),
                used,
                dev
            );
            return;
        }
        let id = st.next_id;
        let base = format!("ca{dev}-{}", utc_stamp(wall_ms));
        let short_writes_at_start = st.short_writes.get(&dev).copied().unwrap_or(0);
        let started = self.queue(
            Msg::Start {
                id,
                base: base.clone(),
                slot: dev,
                kind,
                pid_map,
                trigger_wall_ms: wall_ms,
                trigger_mono_ms: now,
            },
            0,
        );
        if !started {
            ms_log::log!("ddci_capture: writer busy, no capture for CA {}", dev);
            return;
        }
        st.next_id += 1;
        let mut cap = Capture {
            id,
            dev,
            end_ms: now + WINDOW_MS,
            post_bytes: [0; 2],
            truncated: [false; 2],
            drops: [0; 2],
            short_writes_at_start,
        };
        // The pre-window goes out as one batch per direction (Arc handles, no copy).
        for dir in 0..2 {
            let Some(ring) = st.rings.get(&(dev, dir)) else {
                continue;
            };
            if ring.chunks.is_empty() {
                continue;
            }
            let bytes = ring.bytes;
            let chunks: Vec<Chunk> = ring.chunks.iter().cloned().collect();
            let msg = Msg::Chunks {
                id,
                dir,
                bytes,
                chunks,
            };
            if !self.queue(msg, bytes) {
                cap.drops[dir] += bytes as u64;
            }
        }
        st.last_accepted.insert(dev, now);
        st.captures.push(cap);
        ms_log::log!(
            "ddci_capture: CA {} {}: capturing {} for 60 s",
            dev,
            kind,
            base
        );
    }

    /// Finishes captures whose window has passed; retried on the next tick if the queue is full.
    pub fn tick(&self) {
        self.tick_at(self.now_ms());
    }

    fn tick_at(&self, now: u64) {
        let mut guard = self.state();
        let st = &mut *guard;
        let due: Vec<usize> = st
            .captures
            .iter()
            .enumerate()
            .filter(|(_, c)| now >= c.end_ms)
            .map(|(i, _)| i)
            .collect();
        for i in due.into_iter().rev() {
            let cap = &st.captures[i];
            let short_writes = st
                .short_writes
                .get(&cap.dev)
                .copied()
                .unwrap_or(0)
                .saturating_sub(cap.short_writes_at_start);
            let msg = Msg::Finish {
                id: cap.id,
                drops: cap.drops,
                truncated: cap.truncated,
                short_writes,
            };
            if self.queue(msg, 0) {
                st.captures.remove(i);
            }
        }
    }
}

/// `writev` on the CI device; the bytes it accepted are CAM input.
struct TapWriter {
    tap: Arc<CaptureTap>,
    dev: usize,
    inner: Box<dyn CiWriter + Send>,
}

impl CiWriter for TapWriter {
    fn writev(&mut self, iov: &[&[u8]]) -> isize {
        let n = self.inner.writev(iov);
        if n <= 0 {
            return n;
        }
        let total: usize = iov.iter().map(|p| p.len()).sum();
        let accepted = usize::try_from(n).unwrap_or(0).min(total);
        let mut left = accepted;
        let mut parts: Vec<&[u8]> = Vec::with_capacity(iov.len());
        for part in iov {
            if left == 0 {
                break;
            }
            let take = part.len().min(left);
            parts.push(&part[..take]);
            left -= take;
        }
        self.tap.record(self.dev, 0, &parts);
        if accepted < total {
            self.tap.note_short_write(self.dev);
        }
        n
    }

    fn last_errno(&self) -> i32 {
        self.inner.last_errno()
    }

    fn last_error(&self) -> &str {
        self.inner.last_error()
    }

    fn fd(&self) -> i32 {
        self.inner.fd()
    }

    fn socket(&self) -> i32 {
        self.inner.socket()
    }
}

/// `read` on the CAM output; the packets it appended are CAM output.
struct TapReader {
    tap: Arc<CaptureTap>,
    dev: usize,
    inner: Box<dyn Tuner + Send>,
}

impl Tuner for TapReader {
    fn tune(&mut self, params: &TuneParams) -> Result<(), TunerError> {
        self.inner.tune(params)
    }

    fn set_pids(&mut self, pids: &PidSet) -> Result<(), TunerError> {
        self.inner.set_pids(pids)
    }

    fn set_section_pids(&mut self, pids: &PidSet) {
        self.inner.set_section_pids(pids);
    }

    fn status(&self) -> SignalStatus {
        self.inner.status()
    }

    fn read(&mut self, buf: &mut TsBuf) -> Result<usize, TunerError> {
        let before = buf.len();
        let n = self.inner.read(buf)?;
        let after = buf.len();
        if after > before {
            self.tap
                .record(self.dev, 1, &[&buf.as_bytes()[before..after]]);
        }
        Ok(n)
    }

    fn standby(&mut self) {
        self.inner.standby();
    }

    fn take_wait_new_stream(&mut self) -> bool {
        self.inner.take_wait_new_stream()
    }
}

/// One capture being written by the writer thread.
struct Job {
    slot: usize,
    kind: &'static str,
    base: PathBuf,
    names: [String; 2],
    files: [Option<File>; 2],
    offset: [u64; 2],
    index: [Vec<(u64, i64, u64)>; 2],
    failed: [bool; 2],
    pid_map: Vec<(u16, u16)>,
    trigger_wall_ms: i64,
    trigger_mono_ms: u64,
}

impl Job {
    fn open(dir: &Path, base: &str, slot: usize, kind: &'static str) -> Option<Self> {
        let names = DIR_NAMES.map(|d| format!("{base}-{d}.ts"));
        let mut files: [Option<File>; 2] = [None, None];
        for (i, name) in names.iter().enumerate() {
            match File::create(dir.join(name)) {
                Ok(file) => files[i] = Some(file),
                Err(error) => {
                    ms_log::log!("ddci_capture: cannot create {}: {}", name, error);
                    return None;
                }
            }
        }
        Some(Self {
            slot,
            kind,
            base: dir.join(base),
            names,
            files,
            offset: [0; 2],
            index: [Vec::new(), Vec::new()],
            failed: [false; 2],
            pid_map: Vec::new(),
            trigger_wall_ms: 0,
            trigger_mono_ms: 0,
        })
    }

    fn write(&mut self, dir: usize, chunks: &[Chunk]) {
        for chunk in chunks {
            if self.failed[dir] {
                return;
            }
            let Some(file) = self.files[dir].as_mut() else {
                return;
            };
            if let Err(error) = file.write_all(&chunk.data) {
                self.failed[dir] = true;
                ms_log::log!(
                    "ddci_capture: write to {} failed: {}",
                    self.names[dir],
                    error
                );
                return;
            }
            // One index entry per chunk: a gap is at most one batch interval.
            let at = self.offset[dir];
            let wall = self.trigger_wall_ms + (chunk.mono_ms as i64 - self.trigger_mono_ms as i64);
            self.index[dir].push((chunk.mono_ms, wall, at));
            self.offset[dir] += chunk.data.len() as u64;
        }
    }

    fn finish(mut self, drops: [u64; 2], truncated: [bool; 2], short_writes: u64) {
        for file in self.files.iter_mut().flatten() {
            let _ = file.flush();
        }
        let bytes: u64 = self.offset.iter().sum();
        let dropped: u64 = drops.iter().sum();
        let json = self.sidecar(drops, truncated, short_writes);
        let sidecar = format!("{}.json", self.base.display());
        // Written under a temporary name and renamed, so readers never see a partial file.
        let tmp = format!("{sidecar}.tmp");
        if let Err(error) = fs::write(&tmp, json).and_then(|()| fs::rename(&tmp, &sidecar)) {
            ms_log::log!("ddci_capture: cannot write {}: {}", sidecar, error);
        }
        ms_log::log!(
            "ddci_capture: CA {} {}: wrote {} and {} ({} bytes, {} dropped)",
            self.slot,
            self.kind,
            self.names[0],
            self.names[1],
            bytes,
            dropped
        );
        ms_log::diag::events::ddci_capture(
            i64::try_from(self.slot).unwrap_or(-1),
            &format!("{} {}", self.names[0], self.names[1]),
            bytes,
            dropped,
        );
    }

    fn sidecar(&self, drops: [u64; 2], truncated: [bool; 2], short_writes: u64) -> String {
        let mut out = String::from("{\n");
        out.push_str("  \"format\": \"ddci-capture/1\",\n");
        out.push_str(&format!("  \"slot\": {},\n", self.slot));
        out.push_str(&format!("  \"kind\": \"{}\",\n", self.kind));
        out.push_str(&format!(
            "  \"trigger_wall_utc_ms\": {},\n",
            self.trigger_wall_ms
        ));
        out.push_str(&format!(
            "  \"trigger_mono_ms\": {},\n",
            self.trigger_mono_ms
        ));
        out.push_str(&format!("  \"window_ms\": {WINDOW_MS},\n"));
        out.push_str(&format!(
            "  \"files\": {{\"in\": \"{}\", \"out\": \"{}\"}},\n",
            self.names[0], self.names[1]
        ));
        out.push_str(&format!("  \"short_writes\": {short_writes},\n"));
        let pairs: Vec<String> = self
            .pid_map
            .iter()
            .map(|(input, ddci)| format!("[{input}, {ddci}]"))
            .collect();
        out.push_str(&format!("  \"pid_remap\": [{}],\n", pairs.join(", ")));
        out.push_str("  \"dirs\": {\n");
        for d in 0..2 {
            let index: Vec<String> = self.index[d]
                .iter()
                .map(|(mono, wall, at)| format!("[{mono}, {wall}, {at}]"))
                .collect();
            out.push_str(&format!(
                "    \"{}\": {{\"bytes\": {}, \"dropped_bytes\": {}, \"truncated\": {}, \"index\": [{}]}}{}\n",
                DIR_NAMES[d],
                self.offset[d],
                drops[d],
                truncated[d],
                index.join(", "),
                if d == 0 { "," } else { "" }
            ));
        }
        out.push_str("  }\n}\n");
        out
    }
}

fn writer_loop(rx: mpsc::Receiver<Msg>, queued: &AtomicUsize, dir: &Path) {
    let mut jobs: HashMap<u64, Job> = HashMap::new();
    for msg in rx {
        match msg {
            Msg::Start {
                id,
                base,
                slot,
                kind,
                pid_map,
                trigger_wall_ms,
                trigger_mono_ms,
            } => {
                if let Some(mut job) = Job::open(dir, &base, slot, kind) {
                    job.pid_map = pid_map;
                    job.trigger_wall_ms = trigger_wall_ms;
                    job.trigger_mono_ms = trigger_mono_ms;
                    jobs.insert(id, job);
                }
            }
            Msg::Chunks {
                id,
                dir: d,
                bytes,
                chunks,
            } => {
                if let Some(job) = jobs.get_mut(&id) {
                    job.write(d, &chunks);
                }
                queued.fetch_sub(bytes, Ordering::AcqRel);
            }
            Msg::Finish {
                id,
                drops,
                truncated,
                short_writes,
            } => {
                if let Some(job) = jobs.remove(&id) {
                    job.finish(drops, truncated, short_writes);
                }
            }
        }
    }
}

/// Bytes of this tool's capture files already in `dir`.
fn dir_bytes(dir: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            name.starts_with("ca") && (name.ends_with(".ts") || name.ends_with(".json"))
        })
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .sum()
}

/// `YYYYMMDDTHHMMSS` in UTC for a Unix time in milliseconds.
fn utc_stamp(unix_ms: i64) -> String {
    let secs = unix_ms.div_euclid(1000);
    let days = secs.div_euclid(86_400) + 719_468;
    let rem = secs.rem_euclid(86_400);
    let era = days.div_euclid(146_097);
    let doe = days - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn chunk(mono_ms: u64, len: usize) -> Chunk {
        Chunk {
            mono_ms,
            data: Arc::from(vec![0x47u8; len]),
        }
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("ms-ddci-capture-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn ring_respects_time_and_byte_bounds() {
        let mut ring = Ring::default();
        ring.push(chunk(0, 188));
        ring.push(chunk(30_000, 188));
        ring.push(chunk(61_000, 188));
        // The chunk at 0 is more than 60 s older than the newest one.
        assert_eq!(ring.chunks.len(), 2);
        assert_eq!(ring.bytes, 376);
        // The byte bound drops the oldest chunks first.
        let mut ring = Ring::default();
        for i in 0..3 {
            ring.push(chunk(i, RING_BYTES / 2));
        }
        assert!(ring.bytes <= RING_BYTES);
        assert_eq!(ring.chunks.front().map(|c| c.mono_ms), Some(1));
    }

    #[test]
    fn utc_stamp_formats_known_instants() {
        assert_eq!(utc_stamp(0), "19700101T000000");
        // 2026-11-01T13:07:33Z.
        assert_eq!(utc_stamp(1_793_538_453_000), "20261101T130733");
    }

    #[test]
    fn menu_writes_one_pair_and_sidecar_then_rate_limits() {
        let dir = temp_dir("menu");
        let tap = CaptureTap::new(dir.to_str().unwrap(), 2048).unwrap();
        tap.record_at(8, 0, &[&[0x47; 188]], 1_000);
        tap.record_at(8, 1, &[&[0x47; 188], &[0x47; 188]], 1_100);
        tap.trigger_at(8, "menu", vec![(0x100, 0x1100)], 2_000, 1_793_538_453_000);
        // A second trigger inside 10 minutes is refused while the first capture runs.
        tap.trigger_at(8, "enquiry", vec![], 3_000, 1_793_538_454_000);
        assert_eq!(tap.state().captures.len(), 1);
        tap.record_at(8, 1, &[&[0x47; 188]], 30_000);
        // Recorded after the window closes: not captured.
        tap.record_at(8, 0, &[&[0x47; 188]], 62_001);
        tap.tick_at(62_000);
        assert!(tap.state().captures.is_empty());
        let base = dir.join("ca8-20261101T130733");
        let sidecar = format!("{}.json", base.display());
        for _ in 0..200 {
            if Path::new(&sidecar).exists() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let json = fs::read_to_string(&sidecar).unwrap();
        assert!(json.contains("\"kind\": \"menu\""), "{json}");
        assert!(json.contains("[256, 4352]"), "{json}");
        assert_eq!(
            fs::metadata(format!("{}-in.ts", base.display()))
                .unwrap()
                .len(),
            188
        );
        assert_eq!(
            fs::metadata(format!("{}-out.ts", base.display()))
                .unwrap()
                .len(),
            564
        );
        // Inside 10 minutes of the accepted trigger, with no capture running: refused.
        tap.trigger_at(8, "menu", vec![], 300_000, 0);
        assert!(tap.state().captures.is_empty());
        assert_eq!(tap.state().last_accepted.get(&8), Some(&2_000));
        // Ten minutes later a new capture is accepted again.
        tap.trigger_at(8, "menu", vec![], 2_000 + MIN_INTERVAL_MS, 0);
        assert_eq!(tap.state().captures.len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn size_cap_refuses_a_new_capture() {
        let dir = temp_dir("cap");
        let tap = CaptureTap::new(dir.to_str().unwrap(), 0).unwrap();
        tap.record_at(3, 0, &[&[0x47; 188]], 1_000);
        tap.trigger_at(3, "menu", vec![], 2_000, 0);
        assert!(tap.state().captures.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_capture_without_the_option() {
        struct Full;
        impl CiWriter for Full {
            fn writev(&mut self, iov: &[&[u8]]) -> isize {
                iov.iter().map(|p| p.len() as isize).sum()
            }
        }
        struct Idle;
        impl Tuner for Idle {
            fn tune(&mut self, _: &TuneParams) -> Result<(), TunerError> {
                Ok(())
            }
            fn set_pids(&mut self, _: &PidSet) -> Result<(), TunerError> {
                Ok(())
            }
            fn status(&self) -> SignalStatus {
                SignalStatus::default()
            }
            fn read(&mut self, _: &mut TsBuf) -> Result<usize, TunerError> {
                Ok(0)
            }
        }
        // Without --ddci-capture no tap exists, so both CI ends are used unwrapped.
        let runtime = crate::ca::CaRuntime::new(&ms_config::Options::default());
        assert!(!runtime.capture_enabled());
        let (mut writer, _reader) = runtime.ci_io(3, Box::new(Full), Box::new(Idle));
        assert_eq!(writer.writev(&[&[0x47u8; 188]]), 188);
        // With the option, the same call creates the tap.
        let dir = temp_dir("option");
        let opts = ms_config::Options {
            ddci_capture: Some(dir.to_string_lossy().into_owned()),
            ..ms_config::Options::default()
        };
        let runtime = crate::ca::CaRuntime::new(&opts);
        assert!(runtime.capture_enabled());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn wrapped_writer_records_only_accepted_bytes() {
        struct Short;
        impl CiWriter for Short {
            fn writev(&mut self, iov: &[&[u8]]) -> isize {
                let total: usize = iov.iter().map(|p| p.len()).sum();
                (total - 100) as isize
            }
        }
        let dir = temp_dir("short");
        let tap = CaptureTap::new(dir.to_str().unwrap(), 2048).unwrap();
        let mut writer = tap.writer(5, Box::new(Short));
        let packet = [0x47u8; 188];
        assert_eq!(writer.writev(&[&packet, &packet]), 376 - 100);
        let guard = tap.state();
        let ring = guard.rings.get(&(5, 0)).unwrap();
        assert_eq!(ring.bytes, 276);
        assert_eq!(guard.short_writes.get(&5), Some(&1));
        drop(guard);
        let _ = fs::remove_dir_all(&dir);
    }
}
