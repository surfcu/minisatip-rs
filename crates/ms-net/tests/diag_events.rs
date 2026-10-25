//! Diagnostic events emitted by `ms-net` call sites.
//!
//! Gated on `diag-log`: the events are no-ops without the feature.

#![cfg(feature = "diag-log")]

use std::sync::{Arc, Mutex, OnceLock};

use ms_log::Backend;
use ms_net::dataplane::ContinuityCounter;
use ms_net::rtsp::Protocol;
use ms_net::session::{ManualClock, SessionManager};
use ms_sched::{AdapterConfig, AdapterTuner, Scheduler, TuneError};
use ms_tuner::{DeliverySystem, TuneParams};

/// `init` may only run once per test binary, so the capture is shared.
fn captured_lines() -> &'static Arc<Mutex<Vec<String>>> {
    static LINES: OnceLock<Arc<Mutex<Vec<String>>>> = OnceLock::new();
    LINES.get_or_init(|| {
        let lines = Arc::new(Mutex::new(Vec::new()));
        ms_log::init(ms_log::Config {
            log: ms_log::ALL_LOG_MODULES | ms_log::LOG_GENERAL,
            debug: 0,
            slog: 0,
            file_line: false,
            backend: Box::new(Capture(Arc::clone(&lines))),
        });
        lines
    })
}

struct Capture(Arc<Mutex<Vec<String>>>);

impl Backend for Capture {
    fn timestamp(&self) -> ms_log::Timestamp {
        ms_log::Timestamp::new(1, 1, 0, 0, 0, 0)
    }

    fn syslog(&self, _line: &str) {}

    fn stdout(&self, line: &str) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(line.to_owned());
    }
}

fn ts_packet(pid: u16, counter: u32) -> [u8; 188] {
    let mut packet = [0xFFu8; 188];
    packet[0] = 0x47;
    packet[1] = (pid >> 8) as u8 & 0x1F;
    packet[2] = pid as u8;
    packet[3] = 0x10 | (counter & 0x0F) as u8;
    packet
}

/// The latest captured line for `event`, so a parallel test cannot hide it.
fn event(lines: &Arc<Mutex<Vec<String>>>, event: &str) -> String {
    let needle = format!("\"event\":\"{event}\"");
    let captured = lines
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    captured
        .iter()
        .rev()
        .find(|line| line.contains(&needle))
        .cloned()
        .unwrap_or_else(|| panic!("no {event} event: {captured:?}"))
}

#[test]
fn continuity_errors_are_counted_and_flushed_per_pid() {
    let lines = captured_lines();
    let mut cc = ContinuityCounter::new().with_adapter(7);
    assert!(!cc.check_packet(&ts_packet(0x123, 1)));
    assert!(cc.check_packet(&ts_packet(0x123, 3)), "CC 3 after 1");

    ms_log::diag::flush_counters();

    let event = event(lines, "cc_errors");
    assert!(event.contains("\"adapter\":7"), "{event}");
    assert!(event.contains("\"pid\":291"), "{event}");
    assert!(event.contains("\"count\":1"), "{event}");
    assert!(event.contains("\"period_s\":10"), "{event}");
}

struct FakeTuner;

impl AdapterTuner for FakeTuner {
    fn tune(&mut self, _params: &TuneParams) -> Result<(), TuneError> {
        Ok(())
    }
}

const TUNED_URL: &str =
    "rtsp://127.0.0.1:554/?src=1&freq=11362&pol=h&sr=22000&msys=dvbs2&pids=0,16";

#[test]
fn playback_emits_session_open_and_close() {
    let lines = captured_lines();
    let clock = Arc::new(ManualClock::new(0));
    let mut scheduler = Scheduler::new();
    scheduler.add_adapter(
        0,
        AdapterConfig::new(DeliverySystem::DvbS2),
        Box::new(FakeTuner),
    );
    let mut manager = SessionManager::with_scheduler(scheduler);
    let manual: Arc<ManualClock> = Arc::clone(&clock);
    let injected: Arc<dyn ms_net::session::Clock> = manual;
    manager.set_clock(injected);

    let id = manager
        .setup_stream_request(TUNED_URL, None, "SETUP")
        .unwrap();
    manager
        .decode_transport(id, "RTP/AVP;unicast;client_port=5000")
        .unwrap();
    manager.start_play(id, Protocol::Rtsp).expect("play");
    manager.close_stream(id);

    for name in ["session_open", "session_close"] {
        let event = event(lines, name);
        assert!(event.contains(&format!("\"session\":{id}")), "{event}");
        assert!(event.contains("\"proto\":\"rtsp\""), "{event}");
        assert!(event.contains("\"adapter\":0"), "{event}");
    }
}
