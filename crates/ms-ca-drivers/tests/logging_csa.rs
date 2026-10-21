//! Exact-text check for the ported `csa.cpp` registration log.
//!
//! [`ms_log::init`] may only run once per process, so this test lives in its
//! own binary. The line is only emitted by a libdvbcsa build without iCAM
//! support (`--features dvbcsa`, no `dvbcsa-icam`).
#![cfg(all(feature = "dvbcsa", not(feature = "dvbcsa-icam")))]

use std::sync::{Arc, Mutex};

use ms_ca_drivers::descramble::CipherRegistry;
use ms_log::{Backend, Timestamp};

struct Capture(Arc<Mutex<Vec<String>>>);

impl Backend for Capture {
    fn timestamp(&self) -> Timestamp {
        Timestamp::new(1, 1, 0, 0, 0, 0)
    }

    fn syslog(&self, line: &str) {
        self.0.lock().unwrap().push(line.to_owned());
    }
}

/// csa.cpp:125: `LOG("libdvbcsa does not support ICAM (dvbcsa_bs_key_set_ecm missing)")`.
#[test]
fn icam_unsupported_line_matches_cpp() {
    let lines = Arc::new(Mutex::new(Vec::new()));
    // `slog` suppresses the timestamp/thread prefix, so the backend receives
    // the message alone and the assertion can be byte-for-byte.
    ms_log::init(ms_log::Config {
        log: ms_log::LOG_GENERAL | ms_log::LOG_DVBCA,
        debug: 0,
        slog: 1,
        file_line: false,
        backend: Box::new(Capture(Arc::clone(&lines))),
    });

    let _ = CipherRegistry::software();

    let lines = lines.lock().unwrap();
    assert!(
        lines
            .iter()
            .any(|line| line == "libdvbcsa does not support ICAM (dvbcsa_bs_key_set_ecm missing)"),
        "missing ported line in {lines:?}"
    );
}
