//! Diagnostics: remote syslog sink, flight recorder and structured events.
//!
//! Everything here is compiled in every build. Without the `diag-log` feature
//! the public entry points return immediately, so callers need no `cfg`.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use crate::{LOG_DMX, Level};

pub mod remote;
pub mod ring;

/// Module whose lines the ring skips by default (packet dumps are too hot).
const DEFAULT_RING_EXCLUSION: u32 = LOG_DMX;

/// Seconds between heartbeats.
const HEARTBEAT_PERIOD: Duration = Duration::from_secs(60);

/// Longest string kept in an event field.
const MAX_STRING_BYTES: usize = 256;

/// Longest list kept in an event field.
const MAX_LIST_ITEMS: usize = 16;

/// Everything `diag::start` needs from the binary.
pub struct StartConfig {
    /// `--log-remote` address, when remote logging is configured.
    pub remote: Option<String>,
    /// `--log-remote-queue` capacity.
    pub remote_queue: usize,
    /// `--log-ring` capacity; 0 disables the flight recorder.
    pub ring: usize,
    /// Modules whose lines never enter the ring.
    pub ring_exclude: u32,
    /// Hostname reported in RFC 5424 messages.
    pub hostname: String,
    /// Active stream count for the heartbeat (status-page count).
    pub streams: fn() -> u64,
}

/// Processor statistics shown in the heartbeat.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProcessStats {
    /// Resident set size in kilobytes.
    pub rss_kb: u64,
    /// Number of threads.
    pub threads: u64,
    /// Number of open sockets.
    pub sockets: u64,
}

/// One heartbeat event.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Heartbeat {
    /// Whole seconds since the process started.
    pub uptime_s: u64,
    /// Resident set size in kilobytes.
    pub rss_kb: u64,
    /// Number of threads.
    pub threads: u64,
    /// Number of open sockets.
    pub sockets: u64,
    /// Active streams.
    pub streams: u64,
    /// Lines waiting in the remote queue.
    pub sink_queue: u64,
    /// Lines dropped by the remote queue.
    pub sink_dropped: u64,
}

/// Field values appended after the fixed event keys.
#[derive(Debug, Default)]
pub struct Fields {
    json: String,
}

impl Fields {
    /// An empty field list.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends a string field, capped at 256 bytes.
    pub fn string(&mut self, key: &str, value: &str) {
        self.json.push(',');
        self.json.push_str(&quote(key));
        self.json.push(':');
        self.json.push_str(&quote(cap(value)));
    }

    /// Appends an unsigned integer field.
    pub fn int(&mut self, key: &str, value: u64) {
        self.json.push(',');
        self.json.push_str(&quote(key));
        self.json.push(':');
        self.json.push_str(&value.to_string());
    }

    /// Appends an array of strings, keeping the first 16 entries.
    pub fn list(&mut self, key: &str, values: &[&str]) {
        self.json.push(',');
        self.json.push_str(&quote(key));
        self.json.push_str(":[");
        for (index, value) in values.iter().take(MAX_LIST_ITEMS).enumerate() {
            if index > 0 {
                self.json.push(',');
            }
            self.json.push_str(&quote(cap(value)));
        }
        self.json.push(']');
    }
}

/// Serializes one event. `ts`, `thread` and every field string are capped.
pub fn event_json(name: &str, ts: &str, thread: &str, fields: &Fields) -> String {
    format!(
        "{{\"kind\":\"event\",\"event\":{},\"ts\":{},\"thread\":{}{}}}",
        quote(cap(name)),
        quote(cap(ts)),
        quote(cap(thread)),
        fields.json
    )
}

/// Serializes the heartbeat event with caller-supplied time and thread.
pub fn heartbeat_json(heartbeat: &Heartbeat, ts: &str, thread: &str) -> String {
    let mut fields = Fields::new();
    fields.int("uptime_s", heartbeat.uptime_s);
    fields.int("rss_kb", heartbeat.rss_kb);
    fields.int("threads", heartbeat.threads);
    fields.int("sockets", heartbeat.sockets);
    fields.int("streams", heartbeat.streams);
    fields.int("sink_queue", heartbeat.sink_queue);
    fields.int("sink_dropped", heartbeat.sink_dropped);
    event_json("heartbeat", ts, thread, &fields)
}

/// Emits a heartbeat to the local outputs and the remote sink.
pub fn heartbeat(heartbeat: Heartbeat) {
    if !cfg!(feature = "diag-log") {
        return;
    }
    let json = heartbeat_json(&heartbeat, &crate::rfc3339_now(), &crate::thread_name());
    crate::write_local(&json);
    remote::send_event(&json);
}

/// Starts the recorder, the remote sink and the heartbeat timer.
///
/// The config is built only when the feature is enabled, so a normal build
/// does not even call `gethostname`.
pub fn start(config: impl FnOnce() -> StartConfig) {
    if !cfg!(feature = "diag-log") {
        return;
    }
    let config = config();
    ring::start(config.ring);
    RING_EXCLUDE.store(config.ring_exclude, Ordering::SeqCst);
    if let Some(address) = config.remote.as_deref()
        && !remote::start(address, &config.hostname, config.remote_queue)
    {
        crate::write_local(&format!("log remote: unusable address {address}"));
    }
    if config.ring > 0 || remote::is_active() {
        start_heartbeat(config.streams);
    }
}

/// Stops the heartbeat, the remote sink and the recorder.
pub fn stop() {
    if !cfg!(feature = "diag-log") {
        return;
    }
    HEARTBEAT_STOP.store(true, Ordering::SeqCst);
    remote::stop();
    ring::stop();
}

/// Stops everything and clears process-wide state; only for tests.
pub fn reset_for_tests() {
    HEARTBEAT_STOP.store(true, Ordering::SeqCst);
    remote::stop();
    ring::stop();
    RING_EXCLUDE.store(DEFAULT_RING_EXCLUSION, Ordering::SeqCst);
    if let Ok(mut started) = HEARTBEAT_STARTED.lock() {
        *started = false;
    }
}

/// Whether a line from `mask` belongs in the flight recorder.
pub fn ring_wants(mask: u32, level: Level) -> bool {
    cfg!(feature = "diag-log")
        && level != Level::Hot
        && ring::is_active()
        && mask & RING_EXCLUDE.load(Ordering::SeqCst) == 0
}

/// The newest `lines` ring entries, oldest first.
pub fn ring_dump(lines: usize) -> Vec<String> {
    if !cfg!(feature = "diag-log") {
        return Vec::new();
    }
    ring::dump(lines)
}

/// The configured ring capacity, or 0 when the recorder is off.
pub fn ring_capacity() -> usize {
    if !cfg!(feature = "diag-log") {
        return 0;
    }
    ring::capacity()
}

/// Whether the recorder is collecting lines.
pub fn ring_active() -> bool {
    cfg!(feature = "diag-log") && ring::is_active()
}

/// Queues a panic line and the ring tail for the remote sink and drains it.
pub fn panic_publish(crit: &str, tail: &[String]) {
    if !cfg!(feature = "diag-log") {
        return;
    }
    remote::send_panic(crit, tail);
}

/// `(queued lines, dropped lines)` for the heartbeat.
pub fn sink_stats() -> (u64, u64) {
    if !cfg!(feature = "diag-log") {
        return (0, 0);
    }
    remote::stats()
}

static HEARTBEAT_STOP: AtomicBool = AtomicBool::new(false);
static HEARTBEAT_STARTED: Mutex<bool> = Mutex::new(false);
static STARTED_AT: OnceLock<Instant> = OnceLock::new();
static RING_EXCLUDE: AtomicU32 = AtomicU32::new(DEFAULT_RING_EXCLUSION);

fn start_heartbeat(streams: fn() -> u64) {
    let mut started = HEARTBEAT_STARTED
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if *started {
        return;
    }
    *started = true;
    HEARTBEAT_STOP.store(false, Ordering::SeqCst);
    let _ = STARTED_AT.set(Instant::now());
    let _ = thread::Builder::new()
        .name("heartbeat".to_owned())
        .spawn(move || {
            crate::set_thread_name("heartbeat");
            loop {
                for _ in 0..HEARTBEAT_PERIOD.as_secs() {
                    if HEARTBEAT_STOP.load(Ordering::SeqCst) {
                        return;
                    }
                    thread::sleep(Duration::from_secs(1));
                }
                let stats = process_stats();
                let (sink_queue, sink_dropped) = sink_stats();
                heartbeat(Heartbeat {
                    uptime_s: STARTED_AT
                        .get()
                        .map_or(0, |start| start.elapsed().as_secs()),
                    rss_kb: stats.rss_kb,
                    threads: stats.threads,
                    sockets: stats.sockets,
                    streams: streams(),
                    sink_queue,
                    sink_dropped,
                });
            }
        });
}

/// Reads RSS, thread and socket counts from `/proc`.
fn process_stats() -> ProcessStats {
    let mut stats = ProcessStats::default();
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        for line in status.lines() {
            if let Some(value) = line.strip_prefix("VmRSS:") {
                stats.rss_kb = first_number(value);
            } else if let Some(value) = line.strip_prefix("Threads:") {
                stats.threads = first_number(value);
            }
        }
    }
    if let Ok(entries) = std::fs::read_dir("/proc/self/fd") {
        stats.sockets = entries
            .flatten()
            .filter(|entry| {
                std::fs::read_link(entry.path())
                    .is_ok_and(|target| target.to_string_lossy().starts_with("socket:"))
            })
            .count() as u64;
    }
    stats
}

fn first_number(text: &str) -> u64 {
    text.split_whitespace()
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0)
}

/// Truncates a string to at most 256 bytes on a character boundary.
fn cap(value: &str) -> &str {
    let mut end = value.len().min(MAX_STRING_BYTES);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

/// Quotes and escapes a JSON string.
fn quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c < ' ' => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
