//! Exact-text check for a `tables.cpp` log site ported into `ms-ca`.
//!
//! The process-wide logger can be installed once per process, so this file
//! keeps a single test.

use std::sync::{Arc, Mutex};

use ms_ca::Program;
use ms_log::{Backend, Timestamp};

/// Fixed prefix so assertions do not depend on the clock.
const PREFIX: &str = "[05/03 14:07:09.042 logging-tables]: ";

struct Capture(Arc<Mutex<Vec<String>>>);

impl Backend for Capture {
    fn timestamp(&self) -> Timestamp {
        Timestamp::new(5, 3, 14, 7, 9, 42)
    }

    fn syslog(&self, line: &str) {
        self.0.lock().unwrap().push(line.to_owned());
    }

    fn stdout(&self, line: &str) {
        self.0.lock().unwrap().push(line.to_owned());
    }
}

fn init() -> Arc<Mutex<Vec<String>>> {
    ms_log::set_thread_name("logging-tables");
    let lines = Arc::new(Mutex::new(Vec::new()));
    ms_log::init(ms_log::Config {
        log: ms_log::LOG_TABLES,
        debug: 0,
        slog: 0,
        file_line: false,
        backend: Box::new(Capture(Arc::clone(&lines))),
    });
    lines
}

#[test]
fn match_caid_uses_cpp_text() {
    let lines = init();

    let mut program = Program::default();
    program.add_caid(0x0500, 0x0100, &[]);

    // tables.cpp:200, `%s: match caid %04X (%d/%d) with CA caid ...`.
    assert!(program.match_caid(0x0500, 0xFFFF));

    let messages: Vec<String> = lines.lock().unwrap().clone();
    let expected =
        format!("{PREFIX}match_caid: match caid 0500 (0/1) with CA caid 0500 and mask FFFF");
    assert!(
        messages.contains(&expected),
        "missing {expected:?} in {messages:?}"
    );
}
