//! Exact-text check for a ported `adapter.cpp` log line.
//!
//! [`ms_log::init`] may only run once per process, so this test lives in its
//! own binary.

use std::sync::{Arc, Mutex};

use ms_log::{Backend, Timestamp};
use ms_sched::{AdapterConfig, AdapterTuner, Scheduler, StreamId, TuneError};
use ms_tuner::{DeliverySystem, PidSet, TuneParams};

struct Capture(Arc<Mutex<Vec<String>>>);

impl Backend for Capture {
    fn timestamp(&self) -> Timestamp {
        Timestamp::new(1, 1, 0, 0, 0, 0)
    }

    fn syslog(&self, line: &str) {
        self.0.lock().unwrap().push(line.to_owned());
    }
}

struct NoTuner;

impl AdapterTuner for NoTuner {
    fn tune(&mut self, _params: &TuneParams) -> Result<(), TuneError> {
        Ok(())
    }

    fn set_pids(&mut self, _pids: &PidSet) -> Result<(), TuneError> {
        Ok(())
    }
}

fn log_lines() -> Arc<Mutex<Vec<String>>> {
    let lines = Arc::new(Mutex::new(Vec::new()));
    // `slog` suppresses the timestamp/thread prefix, so the backend receives
    // the message alone and the assertion can be byte-for-byte.
    ms_log::init(ms_log::Config {
        log: ms_log::LOG_GENERAL | ms_log::LOG_ADAPTER,
        debug: 0,
        slog: 1,
        file_line: false,
        backend: Box::new(Capture(Arc::clone(&lines))),
    });
    lines
}

/// adapter.cpp:865: `LOG("set adapter %d for sid %d m:%d s:%d", ...)`.
#[test]
fn set_adapter_for_stream_line_matches_cpp() {
    let lines = log_lines();

    let mut scheduler = Scheduler::new();
    scheduler.add_adapter(
        0,
        AdapterConfig::new(DeliverySystem::DvbS2),
        Box::new(NoTuner),
    );
    let sid: StreamId = 7;
    assert!(scheduler.set_adapter_for_stream(sid, 0));

    let lines = lines.lock().unwrap();
    assert!(
        lines
            .iter()
            .any(|line| line == "set adapter 0 for sid 7 m:7 s:1"),
        "missing ported line in {lines:?}"
    );
}
