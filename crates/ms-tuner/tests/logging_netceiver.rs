//! Exact-text check for a ported `netceiver.cpp` log line.
//!
//! [`ms_log::init`] may only run once per process, so this test lives in its
//! own binary.

use std::sync::{Arc, Mutex};

use ms_log::{Backend, Timestamp};
use ms_tuner::netceiver::fake::FakeMcli;
use ms_tuner::netceiver::{NetcvConfig, NetcvError, find_netcv_adapters};

struct Capture(Arc<Mutex<Vec<String>>>);

impl Backend for Capture {
    fn timestamp(&self) -> Timestamp {
        Timestamp::new(1, 1, 0, 0, 0, 0)
    }

    fn syslog(&self, line: &str) {
        self.0.lock().unwrap().push(line.to_owned());
    }
}

fn log_lines() -> Arc<Mutex<Vec<String>>> {
    let lines = Arc::new(Mutex::new(Vec::new()));
    // `slog` suppresses the timestamp/thread prefix, so the backend receives
    // the message alone and the assertion can be byte-for-byte.
    ms_log::init(ms_log::Config {
        log: ms_log::LOG_GENERAL | ms_log::LOG_NETCEIVER,
        debug: 0,
        slog: 1,
        file_line: false,
        backend: Box::new(Capture(Arc::clone(&lines))),
    });
    lines
}

/// netceiver.cpp:424: `LOG("network interface %s not available", opts.netcv_if)`.
#[test]
fn missing_interface_line_matches_cpp() {
    let lines = log_lines();

    let mut api = FakeMcli::new();
    api.interface_up = false;
    let config = NetcvConfig {
        ifname: "eth9".to_owned(),
        count: 1,
    };
    let result = find_netcv_adapters(&mut api, Some(&config), 0, 100, |_| false);
    assert_eq!(
        result,
        Err(NetcvError::InterfaceUnavailable("eth9".to_owned()))
    );

    let lines = lines.lock().unwrap();
    assert!(
        lines
            .iter()
            .any(|line| line == "network interface eth9 not available"),
        "missing ported line in {lines:?}"
    );
}
