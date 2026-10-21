//! Exact-text check for the `ddci.cpp` `disable_cat_adapters` log, ported
//! into the option parser.
//!
//! The process-wide logger can be installed once per process, so this file
//! keeps a single test.

use std::sync::{Arc, Mutex};

use ms_log::{Backend, Timestamp};

/// Fixed prefix so assertions do not depend on the clock.
const PREFIX: &str = "[05/03 14:07:09.042 logging-ddci]: ";

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
    ms_log::set_thread_name("logging-ddci");
    let lines = Arc::new(Mutex::new(Vec::new()));
    ms_log::init(ms_log::Config {
        log: ms_log::LOG_GENERAL,
        debug: 0,
        slog: 0,
        file_line: false,
        backend: Box::new(Capture(Arc::clone(&lines))),
    });
    lines
}

#[test]
fn disable_cat_uses_cpp_text() {
    let lines = init();

    let args = ["--disable-cat".to_owned(), "1".to_owned()];
    let outcome = ms_config::parse_args(&args);
    assert!(outcome.error.is_none());

    let messages: Vec<String> = lines.lock().unwrap().clone();
    let expected = format!("{PREFIX}Disable passing the CAT to DDCI 1");
    assert!(
        messages.contains(&expected),
        "missing {expected:?} in {messages:?}"
    );
    assert!(outcome.options.disable_cat.contains(&1));
}
