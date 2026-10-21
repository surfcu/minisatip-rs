//! One test for the `utils/fifo.cpp` log statements ported into `ms-net`.
//!
//! It lives in its own binary because [`ms_log::init`] may only run once per
//! process.

use std::sync::{Arc, Mutex};

use ms_log::{Backend, Timestamp};
use ms_net::fifo::Fifo;

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
fn push_without_space_logs_the_cpp_line() {
    ms_log::set_thread_name("main");

    let lines = Arc::new(Mutex::new(Vec::new()));
    ms_log::init(ms_log::Config {
        // fifo.cpp has no DEFAULT_LOG: `LOG` is the general module.
        log: ms_log::LOG_GENERAL,
        debug: 0,
        slog: 0,
        file_line: false,
        backend: Box::new(Capture(Arc::clone(&lines))),
    });

    // `Fifo::new` records the caller location, like the C++ `create_fifo`.
    let expected_line = line!() + 1;
    let mut fifo = Fifo::new(8);
    assert_eq!(fifo.push(&[0u8; 8]), 8);

    // fifo.cpp:63-66: "Not enough space in FIFO created at %s:%d: need %u,
    // available %u".
    assert_eq!(fifo.push(&[0u8; 1]), 0);

    let prefix = "[05/03 14:07:09.042 main]: ";
    assert_eq!(
        *lines.lock().unwrap(),
        vec![format!(
            "{prefix}Not enough space in FIFO created at {}:{}: need 1, available 0",
            file!(),
            expected_line
        )]
    );
}
