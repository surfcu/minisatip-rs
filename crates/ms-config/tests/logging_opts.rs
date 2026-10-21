//! One test for the `opts.cpp` log statements ported into `ms-config`.
//!
//! It lives in its own binary because [`ms_log::init`] may only run once per
//! process. The option parser is driven through [`ms_config::parse_args`],
//! including an argv that produces a getopt warning.

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
fn dvbapi_options_log_like_cpp() {
    ms_log::set_thread_name("main");

    let lines = Arc::new(Mutex::new(Vec::new()));
    ms_log::init(ms_log::Config {
        log: ms_log::LOG_GENERAL,
        debug: 0,
        slog: 0,
        file_line: false,
        backend: Box::new(Capture(Arc::clone(&lines))),
    });

    // The unknown option produces a getopt warning; `-o` still runs.
    let outcome = ms_config::parse_args(&[
        "--no-such-option".to_owned(),
        "-o".to_owned(),
        "~/tmp/sock".to_owned(),
    ]);
    assert!(!outcome.warnings.is_empty());
    assert!(outcome.error.is_none());
    assert_eq!(outcome.options.dvbapi_host, "/tmp/sock");
    assert_eq!(outcome.options.pids_all_no_dec, 1);

    let outcome = ms_config::parse_args(&["-o".to_owned(), "1.2.3.4:9001,5".to_owned()]);
    assert!(outcome.error.is_none());
    assert_eq!(outcome.options.dvbapi_host, "1.2.3.4");
    assert_eq!(outcome.options.dvbapi_port, 9001);
    assert_eq!(outcome.options.dvbapi_offset, 5);

    let prefix = "[05/03 14:07:09.042 main]: ";
    assert_eq!(
        *lines.lock().unwrap(),
        vec![
            // opts.cpp:56
            format!("{prefix}Using DVBAPI socket at /tmp/sock"),
            // opts.cpp:63
            format!("{prefix}Not filtering out encrypted packets from pids=all streams"),
            // opts.cpp:58
            format!("{prefix}Using DVBAPI server at 1.2.3.4:9001, offset 5"),
        ]
    );
}
