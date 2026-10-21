//! One test for the `satipc.cpp`/`httpc.cpp` log statements ported into
//! `ms-tuner`.
//!
//! It lives in its own binary because [`ms_log::init`] may only run once per
//! process.

use std::sync::{Arc, Mutex};

use ms_log::{Backend, Timestamp};
use ms_tuner::satipc::Client;

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
fn set_pid_logs_the_cpp_line() {
    ms_log::set_thread_name("main");

    let lines = Arc::new(Mutex::new(Vec::new()));
    ms_log::init(ms_log::Config {
        // `LOG` is the general module; the satipc LOGM/DEBUGM masks stay off.
        log: ms_log::LOG_GENERAL,
        debug: 0,
        slog: 0,
        file_line: false,
        backend: Box::new(Capture(Arc::clone(&lines))),
    });

    // A disabled client is the C++ `!sip` case: `err` is reported as -2.
    let mut client = Client::new(7, "10.0.0.1", 554);
    assert_eq!(client.set_pid(256), 0);

    // satipc.cpp:1410.
    let prefix = "[05/03 14:07:09.042 main]: ";
    assert_eq!(
        *lines.lock().unwrap(),
        vec![format!(
            "{prefix}satipc: set_pid for adapter 7, pid 256, err -2 (lap=0)"
        )]
    );
}
