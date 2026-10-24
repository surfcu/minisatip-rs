//! Logger capture shared by the unit tests: `init` runs once per test binary.

use std::sync::{Arc, Mutex, OnceLock};

use ms_log::Backend;

/// Lines captured for the whole unit-test binary.
pub(crate) fn captured_lines() -> &'static Arc<Mutex<Vec<String>>> {
    static LINES: OnceLock<Arc<Mutex<Vec<String>>>> = OnceLock::new();
    LINES.get_or_init(|| {
        let lines = Arc::new(Mutex::new(Vec::new()));
        ms_log::init(ms_log::Config {
            log: ms_log::ALL_LOG_MODULES | ms_log::LOG_GENERAL,
            debug: 0,
            slog: 0,
            file_line: false,
            backend: Box::new(Capture {
                lines: Arc::clone(&lines),
            }),
        });
        lines
    })
}

struct Capture {
    lines: Arc<Mutex<Vec<String>>>,
}

impl Backend for Capture {
    fn timestamp(&self) -> ms_log::Timestamp {
        ms_log::Timestamp::new(1, 1, 0, 0, 0, 0)
    }

    fn syslog(&self, _: &str) {}

    fn stdout(&self, line: &str) {
        self.lines.lock().unwrap().push(line.to_owned());
    }
}
