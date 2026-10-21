//! Exact text of the CA option lines ported from `src/ca.cpp`/`src/opts.cpp`.
//!
//! Lives in its own binary: [`ms_log::init`] may only run once per process.

use std::sync::{Arc, Mutex};

use ms_log::{Backend, Timestamp};

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

fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|s| (*s).to_owned()).collect()
}

#[test]
fn ca_option_lines_match_cpp() {
    ms_log::set_thread_name("opts");
    let lines = Arc::new(Mutex::new(Vec::new()));
    ms_log::init(ms_log::Config {
        log: ms_log::LOG_GENERAL | ms_log::LOG_DVBCA,
        debug: 0,
        slog: 0,
        file_line: false,
        backend: Box::new(Capture(Arc::clone(&lines))),
    });

    let outcome =
        ms_config::parse_args(&args(&["-3", "1:1234", "-C", "1", "-c", "1:**2-0B00-0B01"]));
    assert_eq!(outcome.error, None);

    let prefix = "[05/03 14:07:09.042 opts]: ";
    let expected = [
        format!("{prefix}Setting CA 1 pin to 1234"),
        format!("{prefix}Forcing CA 1 to CI"),
        format!("{prefix}Forcing CA 1 to use maximum 2 CAPMTs with 3 PMTs each"),
        format!("{prefix}Forcing CA 1 to use CAID 0B00"),
        format!("{prefix}Forcing CA 1 to use CAID 0B01"),
    ];
    assert_eq!(*lines.lock().unwrap(), expected);
}
