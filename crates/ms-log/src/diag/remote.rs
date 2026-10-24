//! Remote syslog sink: RFC 5424 messages over TCP with octet-counted framing.
//!
//! One sender thread owns the connection. Producers only enqueue with
//! `try_send`, so a slow or unreachable receiver never stalls a logging thread.

use std::io::{self, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle, ThreadId};
use std::time::{Duration, Instant};

use crate::{LOG_LEVELS, Level};

/// Facility `daemon` in the RFC 5424 PRI field.
const FACILITY_DAEMON: u8 = 3;
/// `notice`, used for normal log lines (PRI 29).
const SEVERITY_NOTICE: u8 = 5;
/// `info`, used for event lines (PRI 30).
const SEVERITY_INFO: u8 = 6;
/// `crit`, used for panic lines (PRI 26).
const SEVERITY_CRIT: u8 = 2;
/// Structured-data enterprise id.
const SD_ID: &str = "ms@32473";
/// Reply-to/connect timeout.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Write timeout for one framed message.
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
/// Longest reconnect delay.
const MAX_BACKOFF_SECS: u64 = 30;

struct Item {
    ts: String,
    thread: String,
    module: &'static str,
    kind: &'static str,
    severity: u8,
    text: String,
}

struct Sink {
    tx: SyncSender<Item>,
    capacity: usize,
    depth: Arc<AtomicUsize>,
    inflight: Arc<AtomicUsize>,
    dropped: Arc<AtomicUsize>,
    pending: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    sender: JoinHandle<()>,
    sender_id: ThreadId,
}

static SINK: Mutex<Option<Sink>> = Mutex::new(None);

fn sink_lock() -> std::sync::MutexGuard<'static, Option<Sink>> {
    SINK.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Module name reported in the structured data, from the lowest set bit.
pub fn module_name(mask: u32) -> &'static str {
    if mask == 0 {
        return "general";
    }
    LOG_LEVELS
        .get(mask.trailing_zeros() as usize)
        .copied()
        .unwrap_or("general")
}

/// `Level` to the RFC 5424 severity used for its lines.
fn level_severity(level: Level) -> u8 {
    match level {
        Level::Log | Level::Debug | Level::Hot => SEVERITY_NOTICE,
    }
}

/// Starts a sink for `address` (`tcp://host:port` or `host:port`).
///
/// Returns `false` when the address is unusable; the caller logs the reason.
pub fn start(address: &str, hostname: &str, queue: usize) -> bool {
    let target = address.strip_prefix("tcp://").unwrap_or(address);
    if target.is_empty() || !target.contains(':') {
        return false;
    }
    stop();
    // A zero-length channel would reject every line unless the sender waits.
    let capacity = queue.max(1);
    let (tx, rx) = mpsc::sync_channel(capacity);
    let depth = Arc::new(AtomicUsize::new(0));
    let inflight = Arc::new(AtomicUsize::new(0));
    let dropped = Arc::new(AtomicUsize::new(0));
    let pending = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let hostname = hostname.to_owned();
    let thread_state = ThreadState {
        depth: Arc::clone(&depth),
        inflight: Arc::clone(&inflight),
        dropped: Arc::clone(&dropped),
        pending: Arc::clone(&pending),
        stop: Arc::clone(&stop),
        hostname,
    };
    let target = target.to_owned();
    let Ok(sender) = thread::Builder::new()
        .name("logsink".to_owned())
        .spawn(move || sender_loop(target, rx, thread_state))
    else {
        return false;
    };
    let sender_id = sender.thread().id();
    *sink_lock() = Some(Sink {
        tx,
        capacity,
        depth,
        inflight,
        dropped,
        pending,
        stop,
        sender,
        sender_id,
    });
    true
}

/// Stops the sink thread and discards queued lines.
pub fn stop() {
    let sink = sink_lock().take();
    let Some(sink) = sink else {
        return;
    };
    sink.stop.store(true, Ordering::SeqCst);
    drop(sink.tx);
    let _ = sink.sender.join();
}

/// Whether a sink is configured.
pub fn is_active() -> bool {
    sink_lock().is_some()
}

/// `(queued lines, dropped lines)`.
pub fn stats() -> (u64, u64) {
    let guard = sink_lock();
    let Some(sink) = guard.as_ref() else {
        return (0, 0);
    };
    (
        sink.depth.load(Ordering::SeqCst) as u64,
        sink.dropped.load(Ordering::SeqCst) as u64,
    )
}

/// Whether the calling thread is the sender thread.
pub fn is_sender_thread() -> bool {
    let guard = sink_lock();
    guard
        .as_ref()
        .is_some_and(|sink| sink.sender_id == thread::current().id())
}

/// Enqueues one line from the logger; never blocks.
pub(crate) fn send_log(mask: u32, level: Level, message: &str) {
    // The default build calls this for every line: do not take the sink lock.
    if !cfg!(feature = "diag-log") {
        return;
    }
    enqueue(|| Item {
        ts: crate::rfc3339_now(),
        thread: crate::thread_name(),
        module: module_name(mask),
        kind: "log",
        severity: level_severity(level),
        text: message.to_owned(),
    });
}

/// Enqueues one serialized event (the JSON text is the message).
pub(crate) fn send_event(json: &str) {
    enqueue(|| Item {
        ts: crate::rfc3339_now(),
        thread: crate::thread_name(),
        module: "general",
        kind: "event",
        severity: SEVERITY_INFO,
        text: json.to_owned(),
    });
}

/// Queues the panic line and the ring tail, then waits for the queue to drain.
pub(crate) fn send_panic(crit: &str, tail: &[String]) {
    let thread = crate::thread_name();
    enqueue(|| Item {
        ts: crate::rfc3339_now(),
        thread: thread.clone(),
        module: "general",
        kind: "log",
        severity: SEVERITY_CRIT,
        text: crit.to_owned(),
    });
    for line in tail {
        enqueue(|| Item {
            ts: crate::rfc3339_now(),
            thread: thread.clone(),
            module: "general",
            kind: "log",
            severity: SEVERITY_NOTICE,
            text: line.clone(),
        });
    }
    if !is_sender_thread() {
        wait_drained(Duration::from_secs(2));
    }
}

/// Waits until the sender has emptied the queue and finished its current write.
pub fn wait_drained(timeout: Duration) {
    let started = Instant::now();
    loop {
        let busy = {
            let guard = sink_lock();
            guard.as_ref().is_some_and(|sink| {
                sink.depth.load(Ordering::SeqCst) > 0 || sink.inflight.load(Ordering::SeqCst) > 0
            })
        };
        if !busy || started.elapsed() >= timeout {
            return;
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn enqueue(make_item: impl FnOnce() -> Item) {
    let (tx, capacity, depth, dropped, pending) = {
        let guard = sink_lock();
        let Some(sink) = guard.as_ref() else {
            return;
        };
        (
            sink.tx.clone(),
            sink.capacity,
            Arc::clone(&sink.depth),
            Arc::clone(&sink.dropped),
            Arc::clone(&sink.pending),
        )
    };
    // Reject before formatting the item: a stuck sender must make drops cheap.
    if depth.load(Ordering::SeqCst) >= capacity {
        dropped.fetch_add(1, Ordering::SeqCst);
        pending.fetch_add(1, Ordering::SeqCst);
        return;
    }
    // Count before `try_send`; the sender decrements on receive.
    depth.fetch_add(1, Ordering::SeqCst);
    match tx.try_send(make_item()) {
        Ok(()) => {}
        Err(TrySendError::Full(_)) => {
            decrement(&depth);
            dropped.fetch_add(1, Ordering::SeqCst);
            pending.fetch_add(1, Ordering::SeqCst);
        }
        Err(TrySendError::Disconnected(_)) => {
            decrement(&depth);
            dropped.fetch_add(1, Ordering::SeqCst);
        }
    }
}

/// Saturating decrement; the sender and producers update `depth` concurrently.
fn decrement(depth: &AtomicUsize) {
    let _ = depth.try_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
        Some(value.saturating_sub(1))
    });
}

struct ThreadState {
    depth: Arc<AtomicUsize>,
    inflight: Arc<AtomicUsize>,
    dropped: Arc<AtomicUsize>,
    pending: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    hostname: String,
}

fn sender_loop(address: String, rx: mpsc::Receiver<Item>, state: ThreadState) {
    crate::set_thread_name("logsink");
    let pid = std::process::id();
    let mut stream: Option<TcpStream> = None;
    let mut backoff = 1u64;
    // Local output already reported the current disconnected state.
    let mut reported = false;
    loop {
        if state.stop.load(Ordering::SeqCst) {
            break;
        }
        if stream.is_none() {
            match connect(&address) {
                Ok(connection) => {
                    let _ = connection.set_write_timeout(Some(WRITE_TIMEOUT));
                    stream = Some(connection);
                    backoff = 1;
                    reported = false;
                    crate::write_local(&format!("log remote connected to {address}"));
                }
                Err(error) => {
                    // Retries keep backing off; report the state once.
                    if !reported {
                        crate::write_local(&format!(
                            "log remote disconnected: {}; reconnecting in {} s",
                            crate::strerror(&error),
                            backoff
                        ));
                        reported = true;
                    }
                    sleep_interruptible(&state.stop, backoff);
                    backoff = (backoff * 2).min(MAX_BACKOFF_SECS);
                    continue;
                }
            }
            continue;
        }
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(item) => {
                decrement(&state.depth);
                state.inflight.fetch_add(1, Ordering::SeqCst);
                let result = write_item(
                    stream.as_mut().expect("connected"),
                    &item,
                    &state.hostname,
                    pid,
                );
                state.inflight.fetch_sub(1, Ordering::SeqCst);
                match result {
                    Ok(()) => {
                        // Report drops as soon as a busy queue empties, without
                        // waiting for the next idle recv timeout.
                        if state.depth.load(Ordering::SeqCst) == 0 {
                            let failed = match stream.as_mut() {
                                Some(connection) => !flush_dropped(connection, &state, pid),
                                None => false,
                            };
                            if failed {
                                disconnected(
                                    &mut stream,
                                    &state,
                                    &mut backoff,
                                    &mut reported,
                                    "write failed",
                                );
                            }
                        }
                    }
                    Err(error) => {
                        state.dropped.fetch_add(1, Ordering::SeqCst);
                        state.pending.fetch_add(1, Ordering::SeqCst);
                        disconnected(
                            &mut stream,
                            &state,
                            &mut backoff,
                            &mut reported,
                            &crate::strerror(&error),
                        );
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                let failed = match stream.as_mut() {
                    Some(connection) => !flush_dropped(connection, &state, pid),
                    None => false,
                };
                if failed {
                    disconnected(
                        &mut stream,
                        &state,
                        &mut backoff,
                        &mut reported,
                        "write failed",
                    );
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

/// Drops the connection, reports the state once and backs off.
fn disconnected(
    stream: &mut Option<TcpStream>,
    state: &ThreadState,
    backoff: &mut u64,
    reported: &mut bool,
    reason: &str,
) {
    *stream = None;
    if !*reported {
        crate::write_local(&format!(
            "log remote disconnected: {reason}; reconnecting in {backoff} s"
        ));
        *reported = true;
    }
    sleep_interruptible(&state.stop, *backoff);
    *backoff = (*backoff * 2).min(MAX_BACKOFF_SECS);
}

/// Sends one `dropped N lines` report when the queue is empty. Returns false
/// when the write failed and the connection must be re-established.
fn flush_dropped(stream: &mut TcpStream, state: &ThreadState, pid: u32) -> bool {
    let count = state.pending.swap(0, Ordering::SeqCst);
    if count == 0 {
        return true;
    }
    let item = Item {
        ts: crate::rfc3339_now(),
        thread: "logsink".to_owned(),
        module: "general",
        kind: "log",
        severity: SEVERITY_NOTICE,
        text: format!("dropped {count} lines"),
    };
    if write_item(stream, &item, &state.hostname, pid).is_ok() {
        true
    } else {
        // Keep the count so the line is reported after reconnecting.
        state.pending.fetch_add(count, Ordering::SeqCst);
        false
    }
}

fn write_item(stream: &mut TcpStream, item: &Item, hostname: &str, pid: u32) -> io::Result<()> {
    let message = render_item(item, hostname, pid);
    let frame = format!("{} {message}", message.len());
    stream.write_all(frame.as_bytes())?;
    stream.flush()
}

/// Builds one RFC 5424 message.
pub fn render(item: &ItemView<'_>, hostname: &str, pid: u32) -> String {
    let pri = FACILITY_DAEMON * 8 + item.severity;
    let message = if item.kind == "event" {
        item.text.to_owned()
    } else {
        format!("[{}]: {}", item.thread, item.text)
    };
    format!(
        "<{pri}>1 {ts} {hostname} minisatip {pid} - [{SD_ID} module=\"{}\" thread=\"{}\" kind=\"{}\"] {message}",
        escape_sd(item.module),
        escape_sd(item.thread),
        item.kind,
        ts = nil(item.ts),
        hostname = nil(hostname),
    )
}

/// RFC 5424 NILVALUE for an empty header field.
fn nil(value: &str) -> &str {
    if value.is_empty() { "-" } else { value }
}

/// Read-only view of an [`Item`] for [`render`].
pub struct ItemView<'a> {
    /// Timestamp in RFC 3339.
    pub ts: &'a str,
    /// Thread name for the structured data.
    pub thread: &'a str,
    /// Module name for the structured data.
    pub module: &'a str,
    /// `log` or `event`.
    pub kind: &'a str,
    /// RFC 5424 severity.
    pub severity: u8,
    /// Message text.
    pub text: &'a str,
}

impl ItemView<'_> {
    fn from_item(item: &Item) -> ItemView<'_> {
        ItemView {
            ts: &item.ts,
            thread: &item.thread,
            module: item.module,
            kind: item.kind,
            severity: item.severity,
            text: &item.text,
        }
    }
}

fn render_item(item: &Item, hostname: &str, pid: u32) -> String {
    render(&ItemView::from_item(item), hostname, pid)
}

/// Escapes one structured-data parameter value (`"`, `\` and `]`).
pub fn escape_sd(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '"' | '\\' | ']' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

fn connect(address: &str) -> io::Result<TcpStream> {
    let mut last = io::Error::other("no address");
    for addr in address.to_socket_addrs()? {
        match TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) {
            Ok(stream) => return Ok(stream),
            Err(error) => last = error,
        }
    }
    Err(last)
}

fn sleep_interruptible(stop: &AtomicBool, seconds: u64) {
    let mut left = seconds;
    while left > 0 && !stop.load(Ordering::SeqCst) {
        let slice = left.min(1);
        thread::sleep(Duration::from_secs(slice));
        left -= slice;
    }
}
