//! One test for the process-wide logger and the exported macros. It lives in
//! its own binary because [`ms_log::init`] may only run once per process.

use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use ms_log::{Backend, Timestamp};

/// The calling module's C++ `#define DEFAULT_LOG` (`logging.h:54,64`).
const DEFAULT_LOG: u32 = ms_log::LOG_PMT;

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

static FORMATTED: AtomicUsize = AtomicUsize::new(0);

/// Counts how often it is formatted, proving the mask is checked first.
struct Counting(&'static str);

impl fmt::Display for Counting {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        FORMATTED.fetch_add(1, Ordering::SeqCst);
        f.write_str(self.0)
    }
}

mod pmt {
    /// `LOG_PMT` is enabled in both masks by the test configuration.
    const DEFAULT_LOG: u32 = ms_log::LOG_PMT;

    pub fn normal() {
        ms_log::logm!("logm-pmt");
    }

    pub fn debug() {
        ms_log::debugm!("debugm-pmt");
    }
}

mod http {
    /// `LOG_HTTP` is disabled in both masks by the test configuration.
    const DEFAULT_LOG: u32 = ms_log::LOG_HTTP;

    pub fn normal() {
        ms_log::logm!("logm-http");
    }

    pub fn debug() {
        ms_log::debugm!("debugm-http");
    }
}

#[test]
fn global_logger_and_macros() {
    ms_log::set_thread_name("main");

    // Pre-init the const default has only LOG_GENERAL and no debug mask,
    // matching the C++ option-parsing phase. Nothing is formatted.
    ms_log::logl!(ms_log::LOG_HTTP, "{}", Counting("before-init"));
    assert_eq!(FORMATTED.load(Ordering::SeqCst), 0, "not formatted");

    let lines = Arc::new(Mutex::new(Vec::new()));
    ms_log::init(ms_log::Config {
        log: ms_log::LOG_GENERAL | ms_log::LOG_PMT,
        debug: ms_log::LOG_PMT,
        slog: 0,
        file_line: false,
        backend: Box::new(Capture(Arc::clone(&lines))),
    });

    // LOGL/DEBUGL check the mask before formatting (`logging.h:47-52`).
    ms_log::logl!(ms_log::LOG_HTTP, "{}", Counting("logl-disabled"));
    assert_eq!(FORMATTED.load(Ordering::SeqCst), 0);
    ms_log::logl!(ms_log::LOG_PMT, "{}", Counting("logl-enabled"));
    assert_eq!(FORMATTED.load(Ordering::SeqCst), 1);
    ms_log::debugl!(ms_log::LOG_HTTP, "{}", Counting("debugl-disabled"));
    assert_eq!(FORMATTED.load(Ordering::SeqCst), 1);
    ms_log::debugl!(ms_log::LOG_PMT, "{}", Counting("debugl-enabled"));
    assert_eq!(FORMATTED.load(Ordering::SeqCst), 2);

    // LOG is the general module (`logging.h:54`).
    ms_log::log!("general-log");
    // LOGM/DEBUGM resolve the call site's DEFAULT_LOG (`logging.h:54,64`).
    ms_log::logm!("top-level-logm");
    ms_log::debugm!("top-level-debugm");
    ms_log::log0!("always");
    pmt::normal();
    pmt::debug();
    // Both are disabled for http and must not format anything.
    http::normal();
    http::debug();
    assert_eq!(FORMATTED.load(Ordering::SeqCst), 2);

    let prefix = "[05/03 14:07:09.042 main]: ";
    let expected = [
        format!("{prefix}logl-enabled"),
        format!("{prefix}debugl-enabled"),
        format!("{prefix}general-log"),
        format!("{prefix}top-level-logm"),
        format!("{prefix}top-level-debugm"),
        format!("{prefix}always"),
        format!("{prefix}logm-pmt"),
        format!("{prefix}debugm-pmt"),
    ];
    assert_eq!(*lines.lock().unwrap(), expected);
}

#[test]
fn module_names() {
    assert_eq!(ms_log::module_mask("general"), ms_log::LOG_GENERAL);
    assert_eq!(ms_log::module_mask("pmt"), ms_log::LOG_PMT);
    assert_eq!(ms_log::module_mask("ca"), ms_log::LOG_DVBCA);
    assert_eq!(ms_log::module_mask("all"), ms_log::ALL_LOG_MODULES);
    assert_eq!(ms_log::module_mask("unknown"), 0);
    assert_eq!(ms_log::ALL_LOG_MODULES, (1 << ms_log::LOG_LEVELS.len()) - 1);
}
