//! One test for the `api/variables.cpp` log statement ported into `ms-net`.
//!
//! It lives in its own binary because [`ms_log::init`] may only run once per
//! process.

use std::sync::{Arc, Mutex};

use ms_log::{Backend, Timestamp};
use ms_net::api::CBuf;

struct Capture(Arc<Mutex<Vec<String>>>);

impl Backend for Capture {
    fn timestamp(&self) -> Timestamp {
        // Fixed so the assertion does not depend on the clock.
        Timestamp::new(5, 3, 14, 7, 9, 42)
    }

    fn syslog(&self, line: &str) {
        self.0.lock().unwrap().push(format!("syslog:{line}"));
    }

    fn stdout(&self, line: &str) {
        self.0.lock().unwrap().push(line.to_owned());
    }
}

#[test]
fn escape_json_without_room_logs_the_cpp_line() {
    ms_log::set_thread_name("main");

    let lines = Arc::new(Mutex::new(Vec::new()));
    ms_log::init(ms_log::Config {
        // variables.cpp has no DEFAULT_LOG: `LOG` is the general module.
        log: ms_log::LOG_GENERAL,
        debug: 0,
        slog: 0,
        file_line: false,
        backend: Box::new(Capture(Arc::clone(&lines))),
    });

    let mut buffer = CBuf::new(4);
    buffer.put(b"abc");
    // variables.cpp:171-176: dl is the remaining space; 1 < 2 aborts the copy.
    buffer.escape_json(b"x");
    assert_eq!(buffer.len(), 3, "escape_json must not move the pointer");

    let prefix = "[05/03 14:07:09.042 main]: ";
    assert_eq!(
        *lines.lock().unwrap(),
        vec![format!("{prefix}escape_json_string: dl 1 < 2 for x")]
    );
}
