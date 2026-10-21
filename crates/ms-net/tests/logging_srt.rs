//! One test for the `srt.cpp` log statements ported into `ms-net`.
//!
//! It lives in its own binary because [`ms_log::init`] may only run once per
//! process. The `srt` feature gates the ported module.

#![cfg(feature = "srt")]

use std::sync::{Arc, Mutex};

use ms_log::{Backend, Timestamp};
use ms_net::srt::PendingPool;

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
fn accept_without_streamid_logs_the_cpp_line() {
    ms_log::set_thread_name("main");

    let lines = Arc::new(Mutex::new(Vec::new()));
    ms_log::init(ms_log::Config {
        // srt.cpp's LOG is the general module (its DEFAULT_LOG only affects
        // LOGM/DEBUGM, which the file does not use).
        log: ms_log::LOG_GENERAL,
        debug: 0,
        slog: 0,
        file_line: false,
        backend: Box::new(Capture(Arc::clone(&lines))),
    });

    let mut pool = PendingPool::new();
    // srt.cpp:103-104.
    pool.accept(None, 5);

    let prefix = "[05/03 14:07:09.042 main]: ";
    assert_eq!(
        *lines.lock().unwrap(),
        vec![format!(
            "{prefix}SRT accept: empty streamid, rejecting connection"
        )]
    );
}
