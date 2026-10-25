//! Diagnostics integration tests: RFC 5424 framing and the remote sink.

#![cfg(feature = "diag-log")]

use std::io::{self, Read};
use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use ms_log::diag::{self, remote};
use ms_log::{ALL_LOG_MODULES, Backend, Config, LOG_DVBAPI, LOG_GENERAL, Level, Logger, Timestamp};

/// The sink is process-wide, so the tests in this file run one at a time.
static TEST_LOCK: Mutex<()> = Mutex::new(());

fn diag_lock() -> MutexGuard<'static, ()> {
    TEST_LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A logger whose local outputs are discarded.
fn logger() -> Logger {
    struct Quiet;

    impl Backend for Quiet {
        fn timestamp(&self) -> Timestamp {
            Timestamp::new(1, 1, 0, 0, 0, 0)
        }

        fn syslog(&self, _line: &str) {}

        fn stdout(&self, _line: &str) {}
    }

    Logger::new(Config {
        log: ALL_LOG_MODULES,
        debug: 0,
        slog: 0,
        file_line: false,
        backend: Box::new(Quiet),
    })
}

/// Sends one line through the real logger path into the sink.
fn push(logger: &Logger, text: &str) {
    logger.write_masked(
        LOG_GENERAL,
        Level::Log,
        "diag.rs",
        1,
        format_args!("{text}"),
    );
}

fn start(address: SocketAddr, queue: usize) -> bool {
    remote::start(&format!("tcp://{address}"), "testhost", queue)
}

/// Reads one octet-counted frame: `<length> <message>`.
fn read_frame(reader: &mut impl Read) -> io::Result<String> {
    let mut length = 0usize;
    for _ in 0..6 {
        let mut byte = [0u8; 1];
        reader.read_exact(&mut byte)?;
        if byte[0] == b' ' {
            let mut message = vec![0u8; length];
            reader.read_exact(&mut message)?;
            return String::from_utf8(message).map_err(io::Error::other);
        }
        let digit = byte[0]
            .checked_sub(b'0')
            .filter(|digit| *digit < 10)
            .ok_or_else(|| io::Error::other("non-digit in frame length"))?;
        length = length * 10 + digit as usize;
    }
    Err(io::Error::other("frame length too long"))
}

// Framing, PRI, structured data and message for a log line and an event.
#[test]
fn frames_log_and_event_messages() {
    let _guard = diag_lock();
    ms_log::set_thread_name("worker");
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut reader = stream;
        (
            read_frame(&mut reader).unwrap(),
            read_frame(&mut reader).unwrap(),
        )
    });

    assert!(start(address, 100));
    let logger = logger();
    logger.write_masked(LOG_DVBAPI, Level::Log, "diag.rs", 1, format_args!("hello"));
    diag::heartbeat(diag::Heartbeat::default());

    let (log_frame, event_frame) = server.join().unwrap();
    let pid = std::process::id();
    assert!(log_frame.starts_with("<29>1 "), "{log_frame}");
    assert!(
        log_frame.contains(&format!(
            " testhost minisatip {pid} - [ms@32473 module=\"dvbapi\" thread=\"worker\" kind=\"log\"] [worker]: hello"
        )),
        "{log_frame}"
    );
    assert!(event_frame.starts_with("<30>1 "), "{event_frame}");
    assert!(
        event_frame.contains(&format!(" testhost minisatip {pid} - ")),
        "{event_frame}"
    );
    assert!(
        event_frame.contains(
            "[ms@32473 module=\"general\" thread=\"worker\" kind=\"event\"] \
             {\"kind\":\"event\",\"event\":\"heartbeat\""
        ),
        "{event_frame}"
    );
    assert!(event_frame.contains("\"sink_dropped\":0}"), "{event_frame}");
    remote::stop();
}

// Structured-data values escape `"`, `\` and `]`; the message text stays raw.
#[test]
fn render_escapes_structured_data() {
    let view = remote::ItemView {
        ts: "2026-10-24T10:20:30.400+00:00",
        thread: "a\"b]c\\d",
        module: "dvbapi",
        kind: "log",
        severity: 5,
        text: "hello",
    };
    assert_eq!(
        remote::render(&view, "testhost", 42),
        "<29>1 2026-10-24T10:20:30.400+00:00 testhost minisatip 42 - \
         [ms@32473 module=\"dvbapi\" thread=\"a\\\"b\\]c\\\\d\" kind=\"log\"] \
         [a\"b]c\\d]: hello"
    );

    let event = remote::ItemView {
        ts: "2026-10-24T10:20:30.400+00:00",
        thread: "worker",
        module: "general",
        kind: "event",
        severity: 6,
        text: "{\"kind\":\"event\"}",
    };
    assert_eq!(
        remote::render(&event, "testhost", 42),
        "<30>1 2026-10-24T10:20:30.400+00:00 testhost minisatip 42 - \
         [ms@32473 module=\"general\" thread=\"worker\" kind=\"event\"] \
         {\"kind\":\"event\"}"
    );
}

// RFC 5424 NILVALUE: an unavailable timestamp or hostname is written as `-`.
#[test]
fn empty_timestamp_and_hostname_render_as_nilvalue() {
    let view = remote::ItemView {
        ts: "",
        thread: "worker",
        module: "general",
        kind: "log",
        severity: 5,
        text: "hello",
    };
    assert_eq!(
        remote::render(&view, "", 42),
        "<29>1 - - minisatip 42 - [ms@32473 module=\"general\" thread=\"worker\" kind=\"log\"] \
         [worker]: hello"
    );
}

// The sender reconnects after its connection is dropped and the listener is
// rebound on the same address.
#[test]
fn reconnects_after_the_listener_is_rebound() {
    let _guard = diag_lock();
    ms_log::set_thread_name("worker");
    let logger = logger();
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    assert!(start(address, 100));

    let (mut first, _) = listener.accept().unwrap();
    first
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    push(&logger, "before drop");
    assert!(read_frame(&mut first).unwrap().contains("before drop"));
    drop(first);
    drop(listener);

    let rebind_started = Instant::now();
    let listener = loop {
        match TcpListener::bind(address) {
            Ok(listener) => break listener,
            Err(error)
                if error.kind() == io::ErrorKind::AddrInUse
                    && rebind_started.elapsed() < Duration::from_secs(10) =>
            {
                thread::sleep(Duration::from_millis(50));
            }
            Err(error) => panic!("rebind {address}: {error}"),
        }
    };
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        read_frame(&mut stream).unwrap()
    });

    // Keep feeding distinct lines until the reconnect attempt has one to send;
    // identical lines would be suppressed locally and never reach the sink.
    let started = Instant::now();
    let mut attempt = 0;
    while !server.is_finished() && started.elapsed() < Duration::from_secs(10) {
        push(&logger, &format!("after drop {attempt}"));
        attempt += 1;
        thread::sleep(Duration::from_millis(10));
    }
    let frame = server.join().unwrap();
    assert!(frame.starts_with("<29>1 "), "{frame}");
    assert!(
        frame.contains("kind=\"log\"] [worker]: after drop"),
        "{frame}"
    );
    remote::stop();
}

// With no receiver, the queue fills to its bound and the rest is counted as
// dropped; the count is reported once the connection succeeds.
#[test]
fn reports_dropped_lines_and_bounds_the_queue() {
    let _guard = diag_lock();
    ms_log::set_thread_name("worker");
    let probe = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = probe.local_addr().unwrap();
    drop(probe);

    assert!(start(address, 2));
    let logger = logger();
    for index in 0..10 {
        push(&logger, &format!("queued {index}"));
    }
    let (queued, dropped) = remote::stats();
    assert_eq!(queued, 2);
    assert_eq!(dropped, 8);

    let listener = TcpListener::bind(address).unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut frames = Vec::new();
        for _ in 0..3 {
            frames.push(read_frame(&mut stream).unwrap());
        }
        frames
    });
    let frames = server.join().unwrap();
    assert!(frames[0].contains("] [worker]: queued 0"), "{frames:?}");
    assert!(frames[1].contains("] [worker]: queued 1"), "{frames:?}");
    assert!(
        frames[2].contains("] [logsink]: dropped 8 lines"),
        "{frames:?}"
    );
    assert!(
        frames[2].contains("module=\"general\" thread=\"logsink\" kind=\"log\""),
        "{frames:?}"
    );
    remote::stop();
}

// The sink follows local repeat suppression: suppressed lines are not sent and
// the "Message repeated N times" line is sent like any other line.
#[test]
fn sink_follows_local_repeat_suppression() {
    let _guard = diag_lock();
    ms_log::set_thread_name("worker");
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut frames = Vec::new();
        for _ in 0..3 {
            frames.push(read_frame(&mut stream).unwrap());
        }
        frames
    });

    assert!(start(address, 100));
    let logger = logger();
    for _ in 0..3 {
        push(&logger, "same");
    }
    push(&logger, "different");

    let frames = server.join().unwrap();
    assert!(frames[0].contains("] [worker]: same"), "{frames:?}");
    assert!(
        frames[1].contains("] [worker]: Message repeated 2 times"),
        "{frames:?}"
    );
    assert!(frames[2].contains("] [worker]: different"), "{frames:?}");
    remote::stop();
}

// A panic flushes a pending repeat line, and that line reaches the sink like
// the line it repeated.
#[test]
fn panic_flush_sends_the_pending_repeat_to_the_sink() {
    let _guard = diag_lock();
    ms_log::set_thread_name("worker");
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut frames = Vec::new();
        for _ in 0..2 {
            frames.push(read_frame(&mut stream).unwrap());
        }
        frames
    });

    assert!(start(address, 100));
    let logger = logger();
    push(&logger, "same");
    push(&logger, "same");
    logger.write_panic("PANIC in thread worker: test at diag.rs:1:1");

    let frames = server.join().unwrap();
    assert!(frames[0].contains("] [worker]: same"), "{frames:?}");
    assert!(
        frames[1].contains("] [worker]: Message repeated 1 times"),
        "{frames:?}"
    );
    remote::stop();
}

// A producer that keeps the queue busy still gets the drop report as soon as
// the queue drains, without waiting for a 100 ms idle timeout.
#[test]
fn reports_drops_when_a_busy_queue_drains() {
    let _guard = diag_lock();
    ms_log::set_thread_name("worker");
    let probe = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = probe.local_addr().unwrap();
    drop(probe);

    assert!(start(address, 4));
    let sender_logger = logger();
    for index in 0..20 {
        push(&sender_logger, &format!("queued {index}"));
    }
    assert_eq!(remote::stats(), (4, 16));

    // The probe address can linger in TIME_WAIT; retry the rebind briefly.
    let rebind_started = Instant::now();
    let listener = loop {
        match TcpListener::bind(address) {
            Ok(listener) => break listener,
            Err(error)
                if error.kind() == io::ErrorKind::AddrInUse
                    && rebind_started.elapsed() < Duration::from_secs(2) =>
            {
                thread::sleep(Duration::from_millis(50));
            }
            Err(error) => panic!("rebind {address}: {error}"),
        }
    };
    let stop = Arc::new(AtomicBool::new(false));
    let producer_stop = Arc::clone(&stop);
    let producer = thread::spawn(move || {
        let logger = logger();
        let mut index = 0;
        while !producer_stop.load(Ordering::SeqCst) {
            push(&logger, &format!("busy {index}"));
            index += 1;
            thread::sleep(Duration::from_millis(5));
        }
    });
    let (mut stream, _) = listener.accept().unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    // The producer never leaves the queue idle longer than the recv timeout,
    // so the report can only come from the post-write check.
    let report = loop {
        let frame = read_frame(&mut stream).unwrap();
        if frame.contains("dropped ") && frame.contains(" lines") {
            break frame;
        }
    };
    stop.store(true, Ordering::SeqCst);
    producer.join().unwrap();
    remote::stop();
    assert!(
        report.contains("module=\"general\" thread=\"logsink\" kind=\"log\""),
        "{report}"
    );
    let count: u64 = report
        .rsplit_once("dropped ")
        .and_then(|(_, rest)| rest.split_whitespace().next())
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| panic!("no drop count: {report}"));
    assert!(count >= 16, "{report}");
}

// A receiver that accepts but never reads must not stall producers: 100k pushes
// finish quickly and the overflow is counted.
#[test]
fn a_stalled_receiver_never_blocks_producers() {
    let _guard = diag_lock();
    ms_log::set_thread_name("worker");
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    assert!(start(address, 100));
    // Let the sender connect before the socket buffer starts backing up.
    thread::sleep(Duration::from_millis(100));

    let logger = logger();
    let started = Instant::now();
    for index in 0..100_000 {
        // Distinct lines: the sink follows local repeat suppression, so
        // identical lines would only be enqueued once.
        push(&logger, &format!("stalled receiver line {index}"));
    }
    let elapsed = started.elapsed();
    let (queued, dropped) = remote::stats();
    assert!(
        elapsed < Duration::from_secs(1),
        "100k pushes took {elapsed:?}"
    );
    assert!(dropped > 50_000, "dropped {dropped}");
    assert!(queued <= 100, "queued {queued}");

    // Drain the pending connection so the sender is not stuck in a write when
    // the sink is stopped.
    let drain = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buffer = [0u8; 65536];
        while stream
            .read(&mut buffer)
            .map(|read| read > 0)
            .unwrap_or(false)
        {}
    });
    remote::stop();
    drain.join().unwrap();
}
