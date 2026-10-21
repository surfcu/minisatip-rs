//! Exact text of the `stream.cpp` log lines emitted by [`SessionManager`].
//!
//! The logger is process-wide and [`ms_log::init`] may only run once, so this
//! file deliberately holds a single test.

use std::sync::{Arc, Mutex};

use ms_log::{Backend, Timestamp};
use ms_net::session::SessionManager;

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
fn stream_close_log_lines() {
    ms_log::set_thread_name("main");

    let lines = Arc::new(Mutex::new(Vec::new()));
    ms_log::init(ms_log::Config {
        log: ms_log::LOG_GENERAL,
        debug: 0,
        slog: 0,
        file_line: false,
        backend: Box::new(Capture(Arc::clone(&lines))),
    });

    let mut manager = SessionManager::new();
    let id = manager.add_stream().expect("stream added");
    assert_eq!(id, 0);
    manager.close_stream(id);

    // `close_stream` logs before and after the stream shutdown (stream.cpp:352,408).
    let prefix = "[05/03 14:07:09.042 main]: ";
    let expected = [
        format!("{prefix}closing stream 0"),
        format!("{prefix}closed stream 0"),
    ];
    assert_eq!(*lines.lock().unwrap(), expected);
}
