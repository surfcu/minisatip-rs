//! Exact-text check for a ported `dvb.cpp` log line.
//!
//! [`ms_log::init`] may only run once per process, so this test lives in its
//! own binary.
#![cfg(feature = "linux-dvb")]

use std::io;
use std::sync::{Arc, Mutex};

use ms_log::{Backend, Timestamp};
use ms_tuner::linux_dvb::{
    DiseqcMessage, DiseqcParams, MiniCmd, SecDevice, Tone, Voltage, send_diseqc,
};

struct Capture(Arc<Mutex<Vec<String>>>);

impl Backend for Capture {
    fn timestamp(&self) -> Timestamp {
        Timestamp::new(1, 1, 0, 0, 0, 0)
    }

    fn syslog(&self, line: &str) {
        self.0.lock().unwrap().push(line.to_owned());
    }
}

/// Records nothing; `log_fd` is the fd the C++ `send_diseqc` logs.
struct FakeSec;

impl SecDevice for FakeSec {
    fn set_tone(&mut self, _tone: Tone) -> io::Result<()> {
        Ok(())
    }

    fn set_voltage(&mut self, _voltage: Voltage) -> io::Result<()> {
        Ok(())
    }

    fn send_master_cmd(&mut self, _cmd: &DiseqcMessage) -> io::Result<()> {
        Ok(())
    }

    fn send_burst(&mut self, _burst: MiniCmd) -> io::Result<()> {
        Ok(())
    }

    fn sleep_ms(&mut self, _ms: i32) {}

    fn log_fd(&self) -> i32 {
        7
    }
}

fn log_lines() -> Arc<Mutex<Vec<String>>> {
    let lines = Arc::new(Mutex::new(Vec::new()));
    // `slog` suppresses the timestamp/thread prefix, so the backend receives
    // the message alone and the assertion can be byte-for-byte.
    ms_log::init(ms_log::Config {
        log: ms_log::LOG_GENERAL | ms_log::LOG_DVB,
        debug: 0,
        slog: 1,
        file_line: false,
        backend: Box::new(Capture(Arc::clone(&lines))),
    });
    lines
}

/// dvb.cpp:999: `LOGM("send_diseqc fd %d, pos = %d (c %d u %d), pol = %d, hiband = %d", ...)`.
#[test]
fn send_diseqc_line_matches_cpp() {
    let lines = log_lines();

    let mut dev = FakeSec;
    let params = DiseqcParams::default();
    send_diseqc(&mut dev, 2, true, 1, 0, &params, -1);

    let lines = lines.lock().unwrap();
    assert!(
        lines
            .iter()
            .any(|line| line == "send_diseqc fd 7, pos = 2 (c 2 u 2), pol = 1, hiband = 0"),
        "missing ported line in {lines:?}"
    );
}
