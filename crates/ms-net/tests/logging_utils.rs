//! One test for the `utils.cpp` log statements ported into `ms-net`.
//!
//! It lives in its own binary because [`ms_log::init`] may only run once per
//! process.

use std::sync::{Arc, Mutex};

use ms_log::{Backend, Timestamp};

struct Capture(Arc<Mutex<Vec<String>>>);

impl Backend for Capture {
    fn timestamp(&self) -> Timestamp {
        // Fixed so the assertions do not depend on the clock.
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
fn process_file_logs_the_cpp_line() {
    ms_log::set_thread_name("main");

    let lines = Arc::new(Mutex::new(Vec::new()));
    ms_log::init(ms_log::Config {
        // `LOG` is the general module; `DEBUGM` (LOG_UTILS) stays disabled.
        log: ms_log::LOG_GENERAL,
        debug: 0,
        slog: 0,
        file_line: false,
        backend: Box::new(Capture(Arc::clone(&lines))),
    });

    let table = ms_net::api::SymbolTable::new();
    let (body, flushed) = table.process_file(b"abc");
    assert_eq!(body, b"abc".to_vec());
    assert!(!flushed);

    // utils.cpp:385, with `%p` normalised to `0x0`.
    let prefix = "[05/03 14:07:09.042 main]: ";
    assert_eq!(
        *lines.lock().unwrap(),
        vec![format!("{prefix}processing_file 0x0 len 3:")]
    );
}
