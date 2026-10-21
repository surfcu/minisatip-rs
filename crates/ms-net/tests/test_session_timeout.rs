//! Session idle-timeout behavior (`stream_timeout`, `close_http`).
//!
//! These are new tests: the upstream `tests/test_stream.cpp` used as the
//! reference for other ports is not part of the fetched tree.

use std::sync::Arc;

use ms_net::handler::StreamBackend;
use ms_net::rtsp::Protocol;
use ms_net::session::{DEFAULT_TIMEOUT_MS, ManualClock, SessionManager, TIMEOUT_GRACE_MS};
use ms_sched::{AdapterConfig, AdapterTuner, Scheduler, TuneError};
use ms_tuner::{DeliverySystem, TuneParams};

struct FakeTuner;

impl AdapterTuner for FakeTuner {
    fn tune(&mut self, _params: &TuneParams) -> Result<(), TuneError> {
        Ok(())
    }
}

fn manager(clock: &Arc<ManualClock>) -> SessionManager {
    let mut scheduler = Scheduler::new();
    scheduler.add_adapter(
        0,
        AdapterConfig::new(DeliverySystem::DvbS2),
        Box::new(FakeTuner),
    );
    let mut manager = SessionManager::with_scheduler(scheduler);
    let manual: Arc<ManualClock> = Arc::clone(clock);
    let injected: Arc<dyn ms_net::session::Clock> = manual;
    manager.set_clock(injected);
    manager.set_server_timeout_ms(DEFAULT_TIMEOUT_MS);
    manager
}

const TUNED_URL: &str =
    "rtsp://127.0.0.1:554/?src=1&freq=11362&pol=h&sr=22000&msys=dvbs2&pids=0,16";

#[test]
fn rtsp_udp_stream_expires_after_timeout_plus_grace() {
    let clock = Arc::new(ManualClock::new(1_000));
    let mut manager = manager(&clock);
    let id = manager
        .setup_stream_request(TUNED_URL, None, "SETUP")
        .unwrap();
    manager
        .decode_transport(id, "RTP/AVP;unicast;client_port=5000")
        .unwrap();

    // stream.cpp:1366: close when ctime - rtime > timeout + 10000.
    clock.advance(i64::from(DEFAULT_TIMEOUT_MS) + TIMEOUT_GRACE_MS);
    assert!(
        manager.expired_streams().is_empty(),
        "the grace period is inclusive"
    );
    clock.advance(1);
    assert_eq!(manager.expired_streams(), vec![id]);
}

#[test]
fn keepalive_request_refreshes_the_idle_timer() {
    let clock = Arc::new(ManualClock::new(0));
    let mut manager = manager(&clock);
    let id = manager
        .setup_stream_request(TUNED_URL, None, "SETUP")
        .unwrap();
    manager
        .decode_transport(id, "RTP/AVP;unicast;client_port=5000")
        .unwrap();

    clock.advance(30_000);
    StreamBackend::touch_stream(&mut manager, id);
    clock.advance(30_000);
    assert!(manager.expired_streams().is_empty(), "touched recently");

    clock.advance(10_001);
    assert_eq!(manager.expired_streams(), vec![id]);
}

#[test]
fn expired_stream_releases_its_adapter() {
    let clock = Arc::new(ManualClock::new(0));
    let mut manager = manager(&clock);
    let id = manager
        .setup_stream_request(TUNED_URL, None, "SETUP")
        .unwrap();
    manager
        .decode_transport(id, "RTP/AVP;unicast;client_port=5000")
        .unwrap();
    manager.start_play(id, Protocol::Rtsp).expect("play");
    assert_eq!(manager.scheduler().adapter(0).unwrap().master_sid, id);
    assert_eq!(manager.stream(id).unwrap().adapter, 0);

    clock.advance(i64::from(DEFAULT_TIMEOUT_MS) + TIMEOUT_GRACE_MS + 1);
    let expired = manager.expired_streams();
    assert_eq!(expired, vec![id]);
    // The runtime does this for every returned id.
    for expired_id in expired {
        manager.close_stream(expired_id);
    }

    assert!(manager.stream(id).is_none());
    let adapter = manager.scheduler().adapter(0).unwrap();
    assert_eq!(adapter.master_sid, -1, "adapter released");
    assert_eq!(adapter.sid_cnt, 0, "no stream keeps the adapter");
}

#[test]
fn http_stream_has_no_idle_timeout_and_closes_with_its_connection() {
    let clock = Arc::new(ManualClock::new(0));
    let mut manager = manager(&clock);
    let id = manager
        .setup_stream_request("rtsp://127.0.0.1/", None, "")
        .unwrap();
    manager.start_play(id, Protocol::Http).expect("HTTP play");
    assert!(manager.closes_on_control_close(id));

    clock.advance(10_000_000);
    assert!(
        manager.expired_streams().is_empty(),
        "HTTP has no idle timeout"
    );
}

#[test]
fn disabled_timeout_keeps_the_stream_and_makes_close_final() {
    let clock = Arc::new(ManualClock::new(0));
    let mut manager = manager(&clock);
    let id = manager
        .setup_stream_request(TUNED_URL, None, "SETUP")
        .unwrap();
    manager
        .decode_transport(id, "RTP/AVP;unicast;client_port=5000")
        .unwrap();
    assert!(
        !manager.closes_on_control_close(id),
        "an RTSP/UDP stream with a timeout survives TEARDOWN-less close"
    );

    // C++ sets timeout 0 for LIVE555/Lavf user agents (minisatip.cpp:1291-1296).
    StreamBackend::disable_timeout(&mut manager, id);
    clock.advance(10_000_000);
    assert!(manager.expired_streams().is_empty());
    assert!(
        manager.closes_on_control_close(id),
        "a zero timeout does not protect the stream from close_http"
    );
}

#[test]
fn tcp_stream_closes_with_its_control_connection() {
    let clock = Arc::new(ManualClock::new(0));
    let mut manager = manager(&clock);
    let id = manager
        .setup_stream_request(TUNED_URL, None, "SETUP")
        .unwrap();
    manager
        .decode_transport(id, "RTP/AVP/TCP;unicast;interleaved=2-3")
        .unwrap();
    assert!(manager.closes_on_control_close(id));
}

#[test]
fn undecoded_stream_close_policy_matches_close_http() {
    let clock = Arc::new(ManualClock::new(0));
    let mut manager = manager(&clock);
    let id = manager
        .setup_stream_request(TUNED_URL, None, "SETUP")
        .unwrap();
    assert!(
        !manager.closes_on_control_close(id),
        "type 0 with a timeout is kept alive"
    );
}
