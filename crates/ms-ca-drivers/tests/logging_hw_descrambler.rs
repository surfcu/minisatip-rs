//! Exact-text check for ported `hw_descrambler.cpp` log lines.
//!
//! [`ms_log::init`] may only run once per process, so this test lives in its
//! own binary.

use std::sync::{Arc, Mutex};

use ms_ca_drivers::hw_descrambler::fake::RecordingCaDevice;
use ms_ca_drivers::hw_descrambler::{HwAdapter, HwCw, HwDescrambler, HwPmt, HwRegistry};
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

fn log_lines() -> Arc<Mutex<Vec<String>>> {
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
    lines
}

/// hw_descrambler.cpp:311-314 and the ioctl result lines below it.
#[test]
fn hw_set_cw_lines_match_cpp() {
    let lines = log_lines();

    let mut hw = HwDescrambler::new(
        true,
        RecordingCaDevice::with_node("/dev/dvb/adapter0/ca0", 4),
    );
    let mut registry = HwRegistry::default();
    registry.adapters.insert(
        0,
        HwAdapter {
            id: 0,
            pa: 0,
            fe: 0,
            enabled: true,
        },
    );
    let mut cw = HwCw {
        id: 5,
        parity: 1,
        ..HwCw::default()
    };
    HwDescrambler::<RecordingCaDevice>::create_key(&mut cw);
    let pmt = HwPmt {
        id: 9,
        adapter: Some(0),
        enabled: true,
        stream_pids: vec![0x101],
    };
    hw.set_cw(&mut cw, &pmt, &registry);

    let lines = lines.lock().unwrap();
    for expected in [
        "hw_descrambler: hw_create_key cw_id = 5, algo = 0",
        "hw_descrambler: CA_GET_DESCR_INFO returned 4 hardware descrambler slots",
        "hw_descrambler: successfully opened shared /dev/dvb/adapter0/ca0 (ca_fd = 101, max slots = 4) for physical adapter 0",
        "hw_descrambler: Allocated hardware descrambler slot index 0 for PMT 9 on physical adapter 0",
        "hw_descrambler: hw_set_cw called for PMT 9, adapter 0 (pa 0, ca 0), slot 0/4, algo 0, parity 1",
        "hw_descrambler: CA_SET_DESCR_MODE ioctl (slot=0, algo=0, mode=0) -> result=0 SUCCESS",
        "hw_descrambler: CA_SET_PID ioctl (pid=257, slot=0) -> result=0 SUCCESS",
        "hw_descrambler: CA_SET_DESCR (DVBCSA) ioctl (slot=0, parity=1) -> result=0 SUCCESS",
    ] {
        assert!(
            lines.iter().any(|line| line == expected),
            "missing {expected:?} in {lines:?}"
        );
    }
}
