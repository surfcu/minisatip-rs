//! Tests for the logging core. Expected text is derived from logging.cpp and
//! utils.cpp as cited in each test.

use super::*;
use std::sync::{Arc, Mutex};

/// Lines captured by the fake backend.
type CaptureLines = Arc<Mutex<Vec<String>>>;

/// Backend with a fixed timestamp and captured output.
struct Capture {
    stamp: Timestamp,
    stdout: CaptureLines,
    syslog: CaptureLines,
}

impl Capture {
    fn new() -> (Self, CaptureLines, CaptureLines) {
        let stdout = Arc::new(Mutex::new(Vec::new()));
        let syslog = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                // Fixed time used by the format tests.
                stamp: Timestamp::new(5, 3, 14, 7, 9, 42),
                stdout: Arc::clone(&stdout),
                syslog: Arc::clone(&syslog),
            },
            stdout,
            syslog,
        )
    }
}

impl Backend for Capture {
    fn timestamp(&self) -> Timestamp {
        self.stamp
    }

    fn syslog(&self, line: &str) {
        self.syslog.lock().unwrap().push(line.to_owned());
    }

    fn stdout(&self, line: &str) {
        self.stdout.lock().unwrap().push(line.to_owned());
    }
}

fn logger(
    log: u32,
    debug: u32,
    slog: u32,
    file_line: bool,
) -> (Logger, CaptureLines, CaptureLines) {
    let (backend, stdout, syslog) = Capture::new();
    (
        Logger::new(Config {
            log,
            debug,
            slog,
            file_line,
            backend: Box::new(backend),
        }),
        stdout,
        syslog,
    )
}

fn lines(capture: &CaptureLines) -> Vec<String> {
    capture.lock().unwrap().clone()
}

// A panic line is emitted like any other line, but repeats are never collapsed.
#[test]
fn write_panic_skips_repeat_suppression() {
    set_thread_name("main");
    let (logger, out, _) = logger(LOG_GENERAL, 0, 0, false);
    let line = "[05/03 14:07:09.042 main]: PANIC in thread main: x at f:1:1";
    logger.write_panic("PANIC in thread main: x at f:1:1");
    logger.write_panic("PANIC in thread main: x at f:1:1");
    assert_eq!(lines(&out), [line, line]);
}

// A panic raised while `Logger::write` holds the state lock must not deadlock.
#[test]
fn write_panic_while_the_state_lock_is_held_does_not_block() {
    set_thread_name("main");
    let (logger, out, _) = logger(LOG_GENERAL, 0, 0, false);
    let guard = logger.state.lock().unwrap();
    logger.write_panic("PANIC in thread main: x at f:1:1");
    drop(guard);
    assert_eq!(
        lines(&out),
        ["[05/03 14:07:09.042 main]: PANIC in thread main: x at f:1:1"]
    );
}

// With -g the panic line goes to the syslog sink without a prefix.
#[test]
fn write_panic_uses_the_syslog_sink() {
    set_thread_name("main");
    let (logger, out, syslog) = logger(LOG_GENERAL, 0, 1, false);
    logger.write_panic("PANIC in thread main: x at f:1:1");
    assert!(lines(&out).is_empty());
    assert_eq!(lines(&syslog), ["PANIC in thread main: x at f:1:1"]);
}

// logging.cpp:110-117: both failures return the literal "01/01 00:00:20",
// which has no millisecond field.
#[test]
fn fallback_timestamp_matches_the_cpp_literal() {
    assert_eq!(Timestamp::fallback().to_string(), "01/01 00:00:20");
}

// logging.cpp:64-65,115: "[%s%s]: " with stid = " " + thread name.
#[test]
fn line_has_the_cpp_prefix() {
    set_thread_name("main");
    let (logger, out, _) = logger(LOG_GENERAL, 0, 0, false);
    logger.write("src/pmt.cpp", 12, format_args!("hello"));
    assert_eq!(lines(&out), ["[05/03 14:07:09.042 main]: hello"]);
}

// logging.cpp:59-62: file_line adds "%s:%d: " after the bracket.
#[test]
fn file_line_adds_the_source_location() {
    set_thread_name("main");
    let (logger, out, _) = logger(LOG_GENERAL, 0, 0, true);
    logger.write("src/pmt.cpp", 12, format_args!("hello"));
    assert_eq!(
        lines(&out),
        ["[05/03 14:07:09.042 main] src/pmt.cpp:12: hello"]
    );
}

// An unnamed thread keeps the C++ empty name in the prefix (socketworks.h:138).
#[test]
fn unnamed_thread_has_an_empty_prefix_name() {
    let (logger, out, _) = logger(LOG_GENERAL, 0, 0, false);
    let logger = Arc::new(logger);
    let writer = Arc::clone(&logger);
    std::thread::Builder::new()
        .spawn(move || writer.write("src/pmt.cpp", 1, format_args!("x")))
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(lines(&out), ["[05/03 14:07:09.042 ]: x"]);
}

// The panic hook flushes a pending repeat line before the panic line, as the
// next normal line would.
#[test]
fn write_panic_flushes_pending_repeats() {
    set_thread_name("main");
    let (logger, out, _) = logger(LOG_GENERAL, 0, 0, false);
    for _ in 0..3 {
        logger.write("src/pmt.cpp", 1, format_args!("A"));
    }
    logger.write_panic("PANIC in thread main: x at f:1:1");
    assert_eq!(
        lines(&out),
        [
            "[05/03 14:07:09.042 main]: A",
            "Message repeated 2 times",
            "[05/03 14:07:09.042 main]: PANIC in thread main: x at f:1:1",
        ]
    );
}

// logging.cpp:66-69,96-100: with slog the prefix is empty and syslog gets the
// plain message.
#[test]
fn syslog_receives_the_plain_message() {
    set_thread_name("main");
    let (logger, out, syslog) = logger(LOG_GENERAL, 0, 1, true);
    logger.write("src/pmt.cpp", 12, format_args!("hello"));
    assert!(lines(&out).is_empty());
    assert_eq!(lines(&syslog), ["hello"]);
}

// logging.cpp:38-39,54-101: the two output buffers alternate; equal messages
// only bump `times`, and the next different message emits the repeat count
// first, without a prefix.
#[test]
fn repeated_messages_are_collapsed() {
    set_thread_name("main");
    let (logger, out, _) = logger(LOG_GENERAL, 0, 0, false);
    let prefix = "[05/03 14:07:09.042 main]: ";
    for message in ["A", "A", "A"] {
        logger.write("src/pmt.cpp", 1, format_args!("{message}"));
    }
    logger.write("src/pmt.cpp", 1, format_args!("B"));
    assert_eq!(
        lines(&out),
        [
            format!("{prefix}A"),
            "Message repeated 2 times".to_owned(),
            format!("{prefix}B"),
        ]
    );
}

// logging.cpp:75,40: vsnprintf writes at most 1999 bytes including the prefix.
#[test]
fn lines_are_truncated_at_1999_bytes() {
    set_thread_name("main");
    let (logger, out, _) = logger(LOG_GENERAL, 0, 0, false);
    let prefix = "[05/03 14:07:09.042 main]: ";
    let message = "x".repeat(3000);
    logger.write("src/pmt.cpp", 1, format_args!("{message}"));
    let line = lines(&out).remove(0);
    assert_eq!(line.len(), 1999);
    assert_eq!(line, format!("{prefix}{}", "x".repeat(1999 - prefix.len())));
}

// logging.cpp:72: the cut must not split a UTF-8 character (the C++ cuts bytes).
#[test]
fn truncation_stops_at_a_char_boundary() {
    set_thread_name("main");
    let (logger, out, _) = logger(LOG_GENERAL, 0, 0, false);
    let prefix = "[05/03 14:07:09.042 main]: ";
    logger.write("src/pmt.cpp", 1, format_args!("{}", "\u{e9}".repeat(2000)));
    let line = lines(&out).remove(0);
    // 1999 - 27 is even, so the cut lands on a character boundary.
    assert_eq!(line.len(), 1999);
    assert!(line.ends_with('\u{e9}'));
    assert_eq!(&line[..prefix.len()], prefix);
}

// utils.cpp:569-622: offsets are "  %04x ", bytes " %02x", a 16-character
// ASCII column, and the message carries the trailing newline.
#[test]
fn hexdump_matches_the_cpp_layout() {
    set_thread_name("main");
    let (logger, out, _) = logger(LOG_GENERAL, 0, 0, false);
    let data: Vec<u8> = (0..20).collect();
    hexdump_through(&logger, Some("test"), &data);

    let first = "  0000  00 01 02 03 04 05 06 07 08 09 0a 0b 0c 0d 0e 0f  ................\n";
    // 12 missing bytes on the second line, each padded with "   ", then the
    // two spaces before the ASCII column.
    let second = format!("  0010  10 11 12 13{}  ....\n", " ".repeat(36));
    assert_eq!(
        lines(&out),
        [format!("[05/03 14:07:09.042 main]: test:\n{first}{second}")]
    );
}

// utils.cpp:577-580: a zero-length dump logs "desc: ZERO LENGTH".
#[test]
fn hexdump_zero_length() {
    set_thread_name("main");
    let (logger, out, _) = logger(LOG_GENERAL, 0, 0, false);
    hexdump_through(&logger, Some("test"), &[]);
    assert_eq!(
        lines(&out),
        ["[05/03 14:07:09.042 main]: test: ZERO LENGTH"]
    );
}

// utils.cpp:618-621: a NULL description prints a leading newline.
#[test]
fn hexdump_without_description_starts_with_a_newline() {
    set_thread_name("main");
    let (logger, out, _) = logger(LOG_GENERAL, 0, 0, false);
    hexdump_through(&logger, None, &[0x41]);
    let line = lines(&out).remove(0);
    let expected = format!(
        "[05/03 14:07:09.042 main]: \n  0000  41{}  A\n",
        " ".repeat(45)
    );
    assert_eq!(line, expected);
}

// utils.cpp:680-697: one line per 188-byte packet with the MPEG CRC.
#[test]
fn dump_packets_matches_the_cpp_layout() {
    set_thread_name("main");
    let (logger, out, _) = logger(LOG_GENERAL, 0, 0, false);
    let data: Vec<u8> = (0..188).map(|byte| byte as u8).collect();
    dump_through(&logger, "pkt", &data, 0);
    let line = "[05/03 14:07:09.042 main]: pkt: pid 0258 (0102) CC=3 CRC=A005F1BF  pos: 0 packet 0 : \
                [00 01 02 03] 04 05 06 07";
    assert_eq!(lines(&out), [line]);
}

#[test]
fn dump_packets_with_no_data_logs_nothing() {
    set_thread_name("main");
    let (logger, out, _) = logger(LOG_GENERAL, 0, 0, false);
    dump_through(&logger, "pkt", &[], 0);
    assert!(lines(&out).is_empty());
}

#[test]
fn crc_32_is_crc32_mpeg2() {
    // CRC-32/MPEG-2 check value for "123456789".
    assert_eq!(crc_32(b"123456789"), 0x0376_E6E7);
}

// `strerror(errno)` prints only the platform text; std adds " (os error N)".
#[test]
fn strerror_drops_the_os_error_suffix() {
    let error = io::Error::from_raw_os_error(2);
    assert_eq!(errno(&error), 2);
    assert_eq!(
        strerror(&error),
        error.to_string().strip_suffix(" (os error 2)").unwrap()
    );
    assert!(!strerror(&error).contains("(os error"));
    // errno 0 is "Success", as a short write's untouched errno prints in C++.
    assert_eq!(strerror(&io::Error::from_raw_os_error(0)), "Success");
}

// Errors without an OS code keep their Display text and report errno 0.
#[test]
fn strerror_keeps_display_text_without_an_os_code() {
    let error = io::Error::other("boom");
    assert_eq!(errno(&error), 0);
    assert_eq!(strerror(&error), "boom");
}

// minisatip.cpp:288-297 and logging.h:27-45.
#[test]
fn module_masks_match_the_cpp_names() {
    assert_eq!(module_mask("general"), LOG_GENERAL);
    assert_eq!(module_mask("pmt"), LOG_PMT);
    assert_eq!(module_mask("ca"), LOG_DVBCA);
    assert_eq!(module_mask("ddci"), LOG_DDCI);
    assert_eq!(module_mask("all"), ALL_LOG_MODULES);
    assert_eq!(module_mask("nope"), 0);
    assert_eq!(ALL_LOG_MODULES, (1 << LOG_LEVELS.len()) - 1);
}

/// Records a dump into `logger` rather than the process-wide logger.
fn hexdump_through(logger: &Logger, desc: Option<&str>, data: &[u8]) {
    super::hexdump::hexdump_into(logger, desc, data);
}

fn dump_through(logger: &Logger, message: &str, data: &[u8], offset: i64) {
    super::hexdump::dump_packets_into(logger, message, data, offset);
}

/// Serializes tests that touch the process-wide ring and sink.
static DIAG_LOCK: Mutex<()> = Mutex::new(());

fn diag_guard() -> std::sync::MutexGuard<'static, ()> {
    let guard = DIAG_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    diag::reset_for_tests();
    guard
}

// Hot and Debug are the same line for the normal outputs; Hot only opts out of
// the ring, which is off here.
#[test]
fn hot_and_debug_produce_identical_output_with_the_ring_off() {
    let _guard = diag_guard();
    set_thread_name("main");
    let (debug_logger, debug_out, _) = logger(0, LOG_GENERAL, 0, false);
    debug_logger.write_masked(
        LOG_GENERAL,
        Level::Debug,
        "src/a.rs",
        7,
        format_args!("packet {}", 1),
    );
    let (hot_logger, hot_out, _) = logger(0, LOG_GENERAL, 0, false);
    hot_logger.write_masked(
        LOG_GENERAL,
        Level::Hot,
        "src/a.rs",
        7,
        format_args!("packet {}", 1),
    );
    assert_eq!(lines(&debug_out), lines(&hot_out));
    assert_eq!(lines(&hot_out), ["[05/03 14:07:09.042 main]: packet 1"]);
    assert!(!diag::ring_active());
}

// The ring takes Debug but never Hot.
#[cfg(feature = "diag-log")]
#[test]
fn the_ring_skips_hot_lines() {
    let _guard = diag_guard();
    set_thread_name("main");
    diag::ring::start(10);
    let (logger, out, _) = logger(LOG_GENERAL, LOG_GENERAL, 0, false);
    logger.write_masked(
        LOG_GENERAL,
        Level::Hot,
        "src/a.rs",
        1,
        format_args!("hot line"),
    );
    assert_eq!(lines(&out), ["[05/03 14:07:09.042 main]: hot line"]);
    assert!(diag::ring_dump(10).is_empty());

    logger.write_masked(
        LOG_GENERAL,
        Level::Debug,
        "src/a.rs",
        2,
        format_args!("debug line"),
    );
    assert_eq!(
        diag::ring_dump(10),
        ["[05/03 14:07:09.042 main]: debug line"]
    );
}

// A line only the ring wants stays out of the outputs and does not feed the
// repeat-suppression state.
#[cfg(feature = "diag-log")]
#[test]
fn ring_only_lines_skip_outputs_and_repeat_suppression() {
    let _guard = diag_guard();
    set_thread_name("main");
    diag::ring::start(10);
    let (logger, out, _) = logger(0, 0, 0, false);
    for _ in 0..2 {
        logger.write_masked(
            LOG_GENERAL,
            Level::Debug,
            "src/a.rs",
            1,
            format_args!("ring only"),
        );
    }
    assert!(lines(&out).is_empty());
    assert_eq!(
        diag::ring_dump(10),
        [
            "[05/03 14:07:09.042 main]: ring only",
            "[05/03 14:07:09.042 main]: ring only"
        ]
    );
    assert!(!diag::ring_wants(LOG_DMX, Level::Debug));
    assert!(!diag::ring_wants(LOG_GENERAL, Level::Hot));
    assert!(diag::ring_wants(LOG_GENERAL, Level::Debug));
}

// The recorder keeps its capacity and overwrites the oldest line first.
#[test]
fn the_ring_keeps_the_newest_lines_up_to_capacity() {
    let _guard = diag_guard();
    diag::ring::start(2);
    assert_eq!(diag::ring::capacity(), 2);
    assert!(diag::ring::is_active());
    diag::ring::push("a");
    diag::ring::push("b");
    diag::ring::push("c");
    assert_eq!(diag::ring::dump(10), ["b", "c"]);
    assert_eq!(diag::ring::dump(1), ["c"]);
    assert_eq!(diag::ring::dump(0), Vec::<String>::new());
    diag::ring::stop();
    assert!(!diag::ring::is_active());
    assert!(diag::ring::dump(10).is_empty());
}

// Lines that may carry key material use the unrecorded path: outputs only.
#[cfg(feature = "diag-log")]
#[test]
fn unrecorded_lines_never_reach_the_ring() {
    let _guard = diag_guard();
    set_thread_name("main");
    diag::ring::start(10);
    let (logger, out, _) = logger(LOG_GENERAL, 0, 0, false);
    logger.write_unrecorded("src/a.rs", 1, format_args!("key [01 02]"));
    logger.write_masked(
        LOG_GENERAL,
        Level::Log,
        "src/a.rs",
        2,
        format_args!("normal"),
    );
    assert_eq!(
        lines(&out),
        [
            "[05/03 14:07:09.042 main]: key [01 02]",
            "[05/03 14:07:09.042 main]: normal"
        ]
    );
    assert_eq!(diag::ring_dump(10), ["[05/03 14:07:09.042 main]: normal"]);
}

// Events bypass -l masks and repeat suppression entirely.
#[test]
fn write_local_bypasses_masks_and_repeat_suppression() {
    set_thread_name("main");
    let (logger, out, _) = logger(0, 0, 0, false);
    let event = "{\"kind\":\"event\",\"event\":\"heartbeat\"}";
    logger.write_local(event);
    logger.write_local(event);
    assert_eq!(
        lines(&out),
        [
            "[05/03 14:07:09.042 main]: {\"kind\":\"event\",\"event\":\"heartbeat\"}",
            "[05/03 14:07:09.042 main]: {\"kind\":\"event\",\"event\":\"heartbeat\"}"
        ]
    );
}

// Fixed heartbeat fields render in this exact order and shape.
#[test]
fn heartbeat_json_has_the_documented_shape() {
    let heartbeat = diag::Heartbeat {
        uptime_s: 61,
        rss_kb: 2048,
        threads: 7,
        sockets: 3,
        streams: 2,
        sink_queue: 5,
        sink_dropped: 9,
    };
    assert_eq!(
        diag::heartbeat_json(&heartbeat, "2026-10-24T10:20:30.400+00:00", "heartbeat"),
        concat!(
            r#"{"kind":"event","event":"heartbeat","ts":"2026-10-24T10:20:30.400+00:00","#,
            r#""thread":"heartbeat","uptime_s":61,"rss_kb":2048,"threads":7,"#,
            r#""sockets":3,"streams":2,"sink_queue":5,"sink_dropped":9}"#
        )
    );
}

// Event strings are capped at 256 bytes, lists at 16 entries, and JSON is
// escaped.
#[test]
fn event_fields_are_capped_and_escaped() {
    let long = "x".repeat(300);
    let items = vec!["i"; 20];
    let mut fields = diag::Fields::new();
    fields.string("long", &long);
    fields.string("weird", "a\"b\\c\nd\te\u{1}");
    fields.list("list", &items);
    let json = diag::event_json("evt", "ts", "th", &fields);
    assert!(
        json.starts_with("{\"kind\":\"event\",\"event\":\"evt\",\"ts\":\"ts\",\"thread\":\"th\"")
    );
    assert!(json.contains(&format!("\"long\":\"{}\"", "x".repeat(256))));
    assert!(json.contains(r#""weird":"a\"b\\c\nd\te\u0001""#));
    assert_eq!(json.matches("\"i\"").count(), 16);
}

// The cap respects UTF-8 character boundaries.
#[test]
fn event_strings_cap_on_a_character_boundary() {
    let long = "é".repeat(200);
    let mut fields = diag::Fields::new();
    fields.string("s", &long);
    let json = diag::event_json("evt", "ts", "th", &fields);
    assert!(json.contains(&format!("\"s\":\"{}\"", "é".repeat(128))));
}
