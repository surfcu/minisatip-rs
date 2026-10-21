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

// logging.cpp:46-48: stid is " " plus an empty thread name for unnamed threads.
#[test]
fn unnamed_thread_leaves_the_name_empty() {
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
