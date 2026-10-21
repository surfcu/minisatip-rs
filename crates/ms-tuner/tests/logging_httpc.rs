//! One test for the `httpc.cpp` log statements ported into `ms-tuner`.
//!
//! It lives in its own binary because [`ms_log::init`] may only run once per
//! process.

use std::io;
use std::sync::{Arc, Mutex};

use ms_log::{Backend, Timestamp};

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
fn unsupported_scheme_logs_the_cpp_line() {
    ms_log::set_thread_name("main");

    let lines = Arc::new(Mutex::new(Vec::new()));
    ms_log::init(ms_log::Config {
        // `LOG` is the general module; LOG_HTTP `LOGM`s stay off.
        log: ms_log::LOG_GENERAL,
        debug: 0,
        slog: 0,
        file_line: false,
        backend: Box::new(Capture(Arc::clone(&lines))),
    });

    // httpc.cpp:106-107, reached before any socket is created.
    let error = ms_tuner::satipc::fetch_satip_xml("ftp://example.test/xml").unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);

    let prefix = "[05/03 14:07:09.042 main]: ";
    assert_eq!(
        *lines.lock().unwrap(),
        vec![format!(
            "{prefix}Only http support for ftp://example.test/xml"
        )]
    );
}
