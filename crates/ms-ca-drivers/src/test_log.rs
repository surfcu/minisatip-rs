//! Logger capture shared by the unit tests: `init` runs once per test binary.

use std::sync::{Arc, Mutex, OnceLock};

use ms_log::Backend;

/// Lines for `event` emitted by the current test thread. Parallel tests append
/// to the same capture, so a test must not rely on a position in the list.
#[cfg(feature = "diag-log")]
pub(crate) fn events(event: &str) -> Vec<String> {
    let thread = std::thread::current().name().unwrap_or_default().to_owned();
    let event = format!("\"event\":\"{event}\"");
    let thread = format!("\"thread\":\"{thread}\"");
    captured_lines()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .filter(|line| line.contains(&event) && line.contains(&thread))
        .cloned()
        .collect()
}

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
        self.lines
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(line.to_owned());
    }
}
