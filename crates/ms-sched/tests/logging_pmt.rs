//! Exact-text checks for `pmt.cpp` log sites ported into `ms-sched`.
//!
//! The process-wide logger can be installed once per process, so this file
//! keeps a single test.

use std::sync::{Arc, Mutex};

use ms_log::{Backend, Timestamp};

/// Fixed prefix so assertions do not depend on the clock.
const PREFIX: &str = "[05/03 14:07:09.042 logging-pmt]: ";

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
    ms_log::set_thread_name("logging-pmt");
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

fn messages(lines: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
    lines
        .lock()
        .unwrap()
        .iter()
        .map(|line| {
            line.strip_prefix(PREFIX)
                .unwrap_or_else(|| panic!("unexpected log prefix: {line:?}"))
                .to_owned()
        })
        .collect()
}

#[test]
fn pmt_add_and_del_use_cpp_text() {
    let lines = init();
    let mut sched = ms_sched::Scheduler::new();

    // pmt.cpp:1570, `returning new pmt %d for adapter %d, ...`.
    assert_eq!(sched.pmt_add(0, 5, 100), 0);
    // pmt.cpp:1590, `deleting PMT %d, name %s `.
    sched.pmt_del(0);

    let messages = messages(&lines);
    let add = "returning new pmt 0 for adapter 0, pmt pid 100, sid 5 0005";
    let del = "deleting PMT 0, name  ";
    assert!(
        messages.contains(&add.to_owned()),
        "missing {add:?} in {messages:?}"
    );
    assert!(
        messages.contains(&del.to_owned()),
        "missing {del:?} in {messages:?}"
    );
}
