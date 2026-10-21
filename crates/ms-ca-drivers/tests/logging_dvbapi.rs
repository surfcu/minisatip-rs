//! Exact-text check for the `dvbapi.cpp` per-message log ported into
//! `DvbapiClient::on_data` (`dvbapi_reply`).
//!
//! The process-wide logger can be installed once per process, so this file
//! keeps a single test.

use std::sync::{Arc, Mutex};

use ms_ca_drivers::dvbapi::{DvbapiClient, DvbapiConfig};
use ms_log::{Backend, Timestamp};

/// Fixed prefix so assertions do not depend on the clock.
const PREFIX: &str = "[05/03 14:07:09.042 logging-dvbapi]: ";

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
    ms_log::set_thread_name("logging-dvbapi");
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

#[test]
fn dvbapi_reply_uses_cpp_text() {
    let lines = init();

    let mut client = DvbapiClient::new(DvbapiConfig::default());
    client.set_socket_id(7);

    // Big-endian DVBAPI_DMX_STOP (0x6F2A), key 3, demux 0, filter 1, pid 256.
    client.on_data(&[0x00, 0x00, 0x6F, 0x2A, 3, 0, 1, 0x01, 0x00], 0);

    let messages: Vec<String> = lines.lock().unwrap().clone();
    let expected = format!(
        "{PREFIX}dvbapi read from socket 7 the following data (9 bytes), pos = 0, op 00006F2A, key 3"
    );
    assert!(
        messages.contains(&expected),
        "missing {expected:?} in {messages:?}"
    );
}
