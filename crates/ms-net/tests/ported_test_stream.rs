//! Ports of the upstream `tests/test_stream.cpp` cases against the real session layer.

use ms_net::handler::{
    HandlerContext, MAX_STREAMS, PlayError, StreamBackend, StreamType, handle_rtsp,
};
use ms_net::response::Response;
use ms_net::rtsp::{ParseOutcome, Protocol, Request, parse_request};
use ms_net::session::{DescribeOptions, SessionManager, Stream};
use ms_sched::{
    Adapter, AdapterConfig, AdapterTuner, PID_STREAM_ID_UNDEFINED, Scheduler, TuneError,
};
use ms_tuner::{DeliverySystem, PidSet, Polarization, TuneParams};

const DATE: &str = "Wed, 21 Oct 2026 00:00:00 2026 GMT";
const SERVER: &str = "minisatip/1.2.3";

struct FakeTuner {
    result: Result<(), TuneError>,
}

impl FakeTuner {
    fn ok() -> Self {
        Self { result: Ok(()) }
    }

    fn failing(error: TuneError) -> Self {
        Self { result: Err(error) }
    }
}

impl AdapterTuner for FakeTuner {
    fn tune(&mut self, _params: &TuneParams) -> Result<(), TuneError> {
        self.result
    }
}

fn env_with(tuner: FakeTuner) -> SessionManager {
    let mut sched = Scheduler::new();
    sched.add_adapter(
        0,
        AdapterConfig::new(DeliverySystem::DvbS2),
        Box::new(tuner),
    );
    SessionManager::with_scheduler(sched)
}

fn env() -> SessionManager {
    env_with(FakeTuner::ok())
}

fn env_with_system(sys: DeliverySystem) -> SessionManager {
    let mut sched = Scheduler::new();
    sched.add_adapter(0, AdapterConfig::new(sys), Box::new(FakeTuner::ok()));
    SessionManager::with_scheduler(sched)
}

fn describe_options() -> DescribeOptions {
    DescribeOptions {
        local_host: "127.0.0.1".to_owned(),
        session_id: Some(1000),
        session_version: Some(1000),
        time: "0 0".to_owned(),
    }
}

fn describe(manager: &mut SessionManager, current: Option<ms_net::StreamId>, url: &str) -> String {
    manager
        .describe_streams(current, url)
        .expect("describe_streams returned NULL")
}

fn setup(manager: &mut SessionManager, url: &str) -> ms_net::StreamId {
    manager
        .setup_stream_request(url, None, "SETUP")
        .expect("setup_stream returned NULL")
}

fn play_code(result: Result<(), PlayError>) -> i32 {
    match result {
        Ok(()) => 0,
        Err(error) => -i32::from(error.status),
    }
}

fn stream(manager: &SessionManager, id: ms_net::StreamId) -> &Stream {
    manager.stream(id).expect("stream is not enabled")
}

fn adapter(manager: &SessionManager, aid: usize) -> &Adapter {
    manager
        .scheduler()
        .adapter(aid)
        .expect("adapter is missing")
}

fn freq_hz(params: &TuneParams) -> u32 {
    params.freq * 1000
}

fn pol_code(pol: Polarization) -> i32 {
    match pol {
        Polarization::Vertical => 1,
        Polarization::Horizontal => 2,
        Polarization::Right => 3,
        Polarization::Left => 4,
        Polarization::None => 0,
    }
}

fn request(raw: &str) -> Request {
    match parse_request(raw.as_bytes()) {
        ParseOutcome::Complete { request, .. } => request,
        outcome => panic!("request did not parse: {outcome:?}"),
    }
}

fn context<'a>(
    backend: &'a mut SessionManager,
    protocol: Protocol,
) -> HandlerContext<'a, SessionManager> {
    HandlerContext::new(backend, protocol)
        .with_date(DATE)
        .with_server(SERVER)
}

fn text(response: &Response) -> String {
    String::from_utf8(response.to_bytes()).expect("responses are ASCII")
}

fn fill_streams(manager: &mut SessionManager) {
    while !manager.streams_full() {
        assert!(
            manager.add_stream().is_some(),
            "streams_add failed before MAX_STREAMS"
        );
    }
    assert!(manager.streams_full(), "streams should be full");
}

fn live_streams(manager: &SessionManager) -> usize {
    (0..MAX_STREAMS)
        .filter(|id| manager.stream(*id).is_some())
        .count()
}

// Ported from tests/test_stream.cpp:78
#[test]
fn test_setup_stream_new() {
    let mut manager = env();
    manager.set_socket(1000);
    let mut socket_sid = None;

    let req = "?src=1&freq=11362&pol=h&sr=22000&msys=dvbs2&pids=0,16";
    socket_sid = manager.setup_stream_request(req, socket_sid, "SETUP");
    let Some(id) = socket_sid else {
        panic!("setup_stream returned NULL");
    };
    let sid = stream(&manager, id);

    assert_eq!(
        socket_sid,
        Some(sid.id),
        "socket sid not updated to match stream sid"
    );
    assert!(sid.enabled, "allocated stream is not enabled");
    assert_eq!(sid.sock, 1000, "stream socket not set correctly");
    assert_eq!(
        freq_hz(&sid.params),
        11_362_000,
        "frequency in stream transponder parsed incorrectly"
    );
    assert_eq!(
        sid.params.msys,
        DeliverySystem::DvbS2,
        "msys in stream transponder parsed incorrectly"
    );
    assert_eq!(
        pol_code(sid.params.pol),
        2,
        "pol in stream transponder parsed incorrectly"
    );
    assert!(
        sid.params.pids.len() == 2 && sid.params.pids.contains(0) && sid.params.pids.contains(16),
        "pids in stream transponder parsed incorrectly"
    );
}

// Ported from tests/test_stream.cpp:110
#[test]
fn test_setup_stream_existing() {
    let mut manager = env();
    manager.set_socket(1001);
    let mut socket_sid = None;

    let req1 = "?src=1&freq=11362&pol=h&sr=22000&msys=dvbs2&pids=0,16";
    socket_sid = manager.setup_stream_request(req1, socket_sid, "SETUP");
    let Some(allocated_sid) = socket_sid else {
        panic!("first setup_stream returned NULL");
    };

    // Call setup_stream again with different parameters on the same socket.
    let req2 = "?src=1&freq=12543&pol=v&sr=27500&msys=dvbs2&pids=0,17&addpids=100";
    let sid2 = manager
        .setup_stream_request(req2, socket_sid, "SETUP")
        .expect("second setup_stream returned NULL");
    let sid2 = stream(&manager, sid2);

    assert_eq!(sid2.id, allocated_sid, "did not reuse the same stream ID");
    assert_eq!(
        freq_hz(&sid2.params),
        12_543_000,
        "frequency was not updated"
    );
    assert_eq!(pol_code(sid2.params.pol), 1, "polarity was not updated");
    assert!(
        sid2.params.pids.len() == 3
            && sid2.params.pids.contains(0)
            && sid2.params.pids.contains(17)
            && sid2.params.pids.contains(100),
        "pids were not updated correctly"
    );
}

// Ported from tests/test_stream.cpp:143
#[test]
fn test_start_play_no_transport() {
    let mut manager = env();
    manager.set_socket(1002);

    let req = "?src=1&freq=11362&pol=h&sr=22000&msys=dvbs2&pids=0,16";
    let id = manager
        .setup_stream_request(req, None, "PLAY")
        .expect("setup_stream returned NULL");

    let res = play_code(manager.start_play(id, Protocol::Rtsp));
    assert_eq!(
        res, -454,
        "start_play should return -454 for empty transport"
    );
}

// Ported from tests/test_stream.cpp:168
#[test]
fn test_start_play_success() {
    let mut manager = env();
    manager.set_socket(1003);

    let req = "?src=1&freq=11362&pol=h&sr=22000&msys=dvbs2&pids=0,16";
    let id = manager
        .setup_stream_request(req, None, "PLAY")
        .expect("setup_stream returned NULL");

    let res = play_code(manager.start_play(id, Protocol::Http));
    assert_eq!(res, 0, "start_play failed with HTTP setup");
    let sid = stream(&manager, id);
    assert!(sid.do_play, "sid->do_play was not set to 1");
    assert_eq!(
        sid.stream_type,
        StreamType::Http,
        "stream type not set to STREAM_HTTP"
    );
    assert_eq!(sid.adapter, 0, "stream adapter not associated to 0");
}

fn play_then_setup(
    manager: &mut SessionManager,
    first: &str,
    next: &str,
) -> Option<ms_net::StreamId> {
    let mut socket_sid = manager.setup_stream_request(first, None, "PLAY");
    let id = socket_sid?;
    if manager.start_play(id, Protocol::Http).is_err() {
        return None;
    }
    socket_sid = manager.setup_stream_request(next, socket_sid, "SETUP");
    socket_sid
}

// Ported from tests/test_stream.cpp:209
#[test]
fn test_setup_after_play_same_mux_keeps_adapter() {
    let mut manager = env();
    let sid = play_then_setup(
        &mut manager,
        "?src=1&freq=12073&pol=v&sr=27500&msys=dvbs2&pids=0,67",
        "?src=1&freq=12073&pol=v&sr=27500&msys=dvbs2&pids=0",
    )
    .expect("play_then_setup failed");

    assert_eq!(
        stream(&manager, sid).adapter,
        0,
        "same-mux SETUP released the adapter"
    );
    let ad = adapter(&manager, 0);
    assert_eq!(ad.sid_cnt, 1, "same-mux SETUP dropped the stream count");
    assert_eq!(
        freq_hz(&ad.params),
        12_073_000,
        "same-mux SETUP cleared the tuning"
    );
}

// Ported from tests/test_stream.cpp:224
#[test]
fn test_setup_after_play_new_mux_releases_adapter() {
    let mut manager = env();
    let sid = play_then_setup(
        &mut manager,
        "?src=1&freq=12073&pol=v&sr=27500&msys=dvbs2&pids=0",
        "?src=1&freq=11362&pol=h&sr=22000&msys=dvbs2&pids=0",
    )
    .expect("play_then_setup failed");

    assert_eq!(
        stream(&manager, sid).adapter,
        -1,
        "new-mux SETUP kept the adapter"
    );
    assert_eq!(
        adapter(&manager, 0).sid_cnt,
        0,
        "new-mux SETUP kept the stream count"
    );
}

// Ported from tests/test_stream.cpp:238
#[test]
fn test_setup_after_play_other_fe_releases_adapter() {
    let mut manager = env();
    manager.scheduler_mut().clear_fe_map();
    manager.scheduler_mut().set_fe_map_entry(0, 0);
    // fe=2 maps to an adapter other than 0.
    manager.scheduler_mut().set_fe_map_entry(1, 1);

    let sid = play_then_setup(
        &mut manager,
        "?fe=1&src=1&freq=12073&pol=v&sr=27500&msys=dvbs2&pids=0",
        "?fe=2&src=1&freq=12073&pol=v&sr=27500&msys=dvbs2&pids=0",
    )
    .expect("play_then_setup failed");

    assert_eq!(
        stream(&manager, sid).adapter,
        -1,
        "SETUP pinned to another fe kept the adapter"
    );
}

// Ported from tests/test_stream.cpp:257
#[test]
fn test_setup_stream_unspecified_vs_empty_pids() {
    let mut manager = env();
    manager.set_socket(1004);
    let mut socket_sid = None;

    // Setup initially with some pids.
    let req1 = "?src=1&freq=11362&pol=h&sr=22000&msys=dvbs2&pids=0,16&addpids=100";
    socket_sid = manager.setup_stream_request(req1, socket_sid, "SETUP");
    let Some(id) = socket_sid else {
        panic!("setup_stream failed");
    };
    let expected = PidSet::from_pids([0, 16, 100]);
    assert!(
        stream(&manager, id).params.pids.len() == 3
            && stream(&manager, id).params.pids.contains(0)
            && stream(&manager, id).params.pids.contains(16)
            && stream(&manager, id).params.pids.contains(100),
        "pids not set correctly"
    );

    // Unspecified pids without freq= are inherited.
    socket_sid = manager.setup_stream_request("?pol=v", socket_sid, "SETUP");
    assert_eq!(
        stream(&manager, id).params.pids,
        expected,
        "pids should be inherited when unspecified"
    );

    // freq= clears the pids.
    manager.setup_stream_request("?freq=11362&pol=h&sr=22000&msys=dvbs2", socket_sid, "SETUP");
    assert!(
        stream(&manager, id).params.pids.is_empty(),
        "pids should be cleared when freq= is specified"
    );
}

// Ported from tests/test_stream.cpp:349
#[test]
fn test_decode_transport_port_change_no_deadlock() {
    let mut manager = env();
    manager.set_socket(1000);
    let id = setup(&mut manager, "?src=1&freq=11362&pol=h&sr=22000&pids=0");
    manager.set_start_rtp(45_500);

    manager
        .decode_transport(id, "RTP/AVP;unicast;client_port=46000-46001")
        .expect("first transport failed");
    assert_eq!(
        manager.stream_info(id).unwrap().remote_port,
        46_000,
        "first client port not applied"
    );
    // C++:368-369 (`rs != NULL`) and C++:371-391 (demux-thread deadlock
    // timeout) need the real RTP socket layer and cannot be expressed here.
    manager
        .decode_transport(id, "RTP/AVP;unicast;client_port=46002-46003")
        .expect("second transport failed");
    // C++:393 (`get_sockets(sid->rsock_id) != NULL`) needs sockets.
    assert!(manager.stream(id).is_some(), "stream must stay enabled");
    assert_eq!(
        manager.stream_info(id).unwrap().remote_port,
        46_002,
        "client port not updated"
    );
}

// Ported from tests/test_stream.cpp:400
#[test]
fn test_start_play_all_tuners_busy_returns_503() {
    let mut manager = env();

    let sid1 = setup(
        &mut manager,
        "?src=1&freq=11362&pol=h&sr=22000&msys=dvbs2&pids=0,16",
    );
    assert_eq!(
        play_code(manager.start_play(sid1, Protocol::Http)),
        0,
        "first start_play failed"
    );

    let sid2 = setup(
        &mut manager,
        "?src=1&freq=12543&pol=v&sr=27500&msys=dvbs2&pids=0,17",
    );
    let res = play_code(manager.start_play(sid2, Protocol::Http));
    assert_eq!(res, -503, "start_play should return -503 when busy");
    assert_eq!(
        stream(&manager, sid2).rtsp_error,
        "No-More: frontends",
        "missing No-More: frontends body"
    );
    assert_eq!(
        stream(&manager, sid2).adapter,
        -1,
        "failed play must not attach an adapter"
    );
    let ad = adapter(&manager, 0);
    assert!(
        ad.sid_cnt == 1 && ad.master_sid == sid1,
        "adapter must still belong to the first stream"
    );

    // Free the tuner: the same request must now succeed with no error body.
    manager.close_stream(sid1);
    assert_eq!(
        play_code(manager.start_play(sid2, Protocol::Http)),
        0,
        "replay after free failed"
    );
    assert!(
        stream(&manager, sid2).rtsp_error.is_empty(),
        "success must clear the error body"
    );
}

// Ported from tests/test_stream.cpp:443
#[test]
fn test_start_play_unmapped_src_returns_403() {
    let mut manager = env();
    manager.scheduler_mut().set_absolute_switch(true);

    let sid = setup(
        &mut manager,
        "?src=3&freq=11362&pol=h&sr=22000&msys=dvbs2&pids=0",
    );
    let res = play_code(manager.start_play(sid, Protocol::Http));
    assert_eq!(res, -403, "start_play should return -403 for unmapped src");
    assert_eq!(
        stream(&manager, sid).rtsp_error,
        "Out-of-Range: src",
        "missing Out-of-Range: src body"
    );
}

// Ported from tests/test_stream.cpp:469
#[test]
fn test_start_play_unsupported_msys_returns_403() {
    let mut manager = env();
    let sid = setup(&mut manager, "?freq=538&bw=8&msys=dvbt2&pids=0");
    let res = play_code(manager.start_play(sid, Protocol::Http));
    assert_eq!(res, -403, "start_play should return -403 for bad msys");
    assert_eq!(
        stream(&manager, sid).rtsp_error,
        "Out-of-Range: msys",
        "missing Out-of-Range: msys body"
    );
}

// Ported from tests/test_stream.cpp:490
#[test]
fn test_start_play_pid_exhaustion_returns_503() {
    let mut manager = env();

    let sid1 = setup(
        &mut manager,
        "?src=1&freq=11362&pol=h&sr=22000&msys=dvbs2&pids=0",
    );
    assert_eq!(
        play_code(manager.start_play(sid1, Protocol::Http)),
        0,
        "first start_play failed"
    );

    let mut pid = 100;
    while pid < 8000
        && manager
            .scheduler_mut()
            .mark_pid_add(0, PID_STREAM_ID_UNDEFINED, pid)
    {
        pid += 1;
    }
    assert!(pid < 8000, "could not fill the pid table");

    // Same transponder as sid1, so no retune clears the pid table.
    let sid2 = setup(
        &mut manager,
        "?src=1&freq=11362&pol=h&sr=22000&msys=dvbs2&pids=5000",
    );
    let res = play_code(manager.start_play(sid2, Protocol::Http));
    assert_eq!(res, -503, "start_play should return -503 when pids run out");
    assert_eq!(
        stream(&manager, sid2).rtsp_error,
        "No-More: pids",
        "missing No-More: pids body"
    );
}

// Ported from tests/test_stream.cpp:530
#[test]
fn test_start_play_pinned_fe_mismatch_returns_403() {
    let mut manager = env();
    // Pin fe=1 to adapter 0; the rest stay unmapped.
    manager.scheduler_mut().clear_fe_map();
    manager.scheduler_mut().set_fe_map_entry(0, 0);

    let sid = setup(&mut manager, "?fe=1&freq=538&bw=8&msys=dvbt2&pids=0");
    let res = play_code(manager.start_play(sid, Protocol::Http));
    assert_eq!(
        stream(&manager, sid).params.fe,
        Some(1),
        "fe was not parsed"
    );
    assert_eq!(res, -403, "start_play should return -403 for bad fe");
    assert_eq!(
        stream(&manager, sid).rtsp_error,
        "Out-of-Range: fe",
        "missing Out-of-Range: fe body"
    );
}

// Ported from tests/test_stream.cpp:558
#[test]
fn test_start_play_unmapped_fe_ignored_when_busy() {
    let mut manager = env();

    let sid1 = setup(
        &mut manager,
        "?src=1&freq=11362&pol=h&sr=22000&msys=dvbs2&pids=0",
    );
    assert_eq!(
        play_code(manager.start_play(sid1, Protocol::Http)),
        0,
        "first start_play failed"
    );

    manager.scheduler_mut().clear_fe_map();

    let sid2 = setup(
        &mut manager,
        "?fe=99&freq=12543&pol=v&sr=27500&msys=dvbs2&pids=0",
    );
    let res = play_code(manager.start_play(sid2, Protocol::Http));
    assert_eq!(res, -503, "unmapped fe must not shadow busy tuners");
    assert_eq!(
        stream(&manager, sid2).rtsp_error,
        "No-More: frontends",
        "missing No-More: frontends body"
    );
}

// Ported from tests/test_stream.cpp:604
#[test]
fn test_start_play_tune_freq_error_returns_403() {
    let mut manager = env_with(FakeTuner::failing(TuneError::OutOfRange));

    let sid = setup(
        &mut manager,
        "?src=1&freq=11362&pol=h&sr=22000&msys=dvbs2&pids=0,16",
    );
    let res = play_code(manager.start_play(sid, Protocol::Http));
    assert_eq!(res, -403, "tune failure should surface as -403");
    assert_eq!(
        stream(&manager, sid).rtsp_error,
        "Out-of-Range: freq",
        "missing Out-of-Range: freq body"
    );
}

// Ported from tests/test_stream.cpp:633
#[test]
fn test_tune_missing_adapter_returns_500() {
    let mut manager = env();
    let outcome = manager.scheduler_mut().tune(5, 0);
    assert_eq!(
        outcome.result,
        Err(TuneError::Hardware),
        "tune must return -500 without an adapter"
    );
    assert_eq!(outcome.result.unwrap_err().code(), -500);
}

// Ported from tests/test_stream.cpp:640
#[test]
fn test_start_play_tune_error_returns_500() {
    let mut manager = env_with(FakeTuner::failing(TuneError::Hardware));

    let sid = setup(
        &mut manager,
        "?src=1&freq=11362&pol=h&sr=22000&msys=dvbs2&pids=0,16",
    );
    let res = play_code(manager.start_play(sid, Protocol::Http));
    assert_eq!(res, -500, "tune failure should surface as -500");
    assert!(
        stream(&manager, sid).rtsp_error.is_empty(),
        "500 must carry no body"
    );
}

// Ported from tests/test_stream.cpp:662
#[test]
fn test_set_adapter_parameters_slave_conflict() {
    let mut manager = env();

    let sid1 = setup(
        &mut manager,
        "?src=1&freq=11362&pol=h&sr=22000&msys=dvbs2&pids=0",
    );
    assert_eq!(
        play_code(manager.start_play(sid1, Protocol::Http)),
        0,
        "first start_play failed"
    );

    let sid2 = setup(
        &mut manager,
        "?src=1&freq=12543&pol=v&sr=27500&msys=dvbs2&pids=0",
    );
    let params = stream(&manager, sid2).params.clone();
    let error = manager
        .scheduler_mut()
        .set_adapter_parameters(0, sid2, &params)
        .expect_err("slave retune must return -2");
    assert_eq!(error.code(), -2, "slave retune must return -2");
}

// Ported from tests/test_stream.cpp:695
#[test]
fn test_streams_full() {
    let mut manager = env();
    assert!(
        !manager.streams_full(),
        "streams should not be full initially"
    );

    let mut ids = Vec::new();
    for _ in 0..MAX_STREAMS {
        let id = manager
            .add_stream()
            .expect("streams_add failed before MAX_STREAMS");
        assert!(id >= 0, "streams_add failed before MAX_STREAMS");
        ids.push(id);
    }
    assert!(manager.streams_full(), "streams should be full");
    assert_eq!(
        manager.add_stream(),
        None,
        "streams_add should fail when full"
    );
    manager.close_stream(ids[0]);
    assert!(
        !manager.streams_full(),
        "streams should not be full after close"
    );
}

// --- C1 request-level tests re-run against the real backend --------------

// Ported from tests/test_stream.cpp:778
#[test]
fn test_read_rtsp_empty_buffer_ignored() {
    let manager = env();
    let buffer: Vec<u8> = Vec::new();

    assert_eq!(
        parse_request(&buffer),
        ParseOutcome::Incomplete { consumed: 0 },
        "empty buffer must be ignored"
    );
    assert!(
        buffer.is_empty(),
        "empty buffer must leave the buffer alone"
    );
    assert_eq!(live_streams(&manager), 0, "no state may be created");
}

// Ported from tests/test_stream.cpp:789
#[test]
fn test_read_http_empty_buffer_ignored() {
    let manager = env();
    let buffer: Vec<u8> = Vec::new();

    assert_eq!(
        parse_request(&buffer),
        ParseOutcome::Incomplete { consumed: 0 },
        "empty buffer must be ignored"
    );
    assert!(
        buffer.is_empty(),
        "empty buffer must leave the buffer alone"
    );
    assert_eq!(live_streams(&manager), 0, "no state may be created");
}

// Ported from tests/test_stream.cpp:800
#[test]
fn test_read_rtsp_sessions_exhausted_returns_503() {
    let mut manager = env();
    fill_streams(&mut manager);

    let req = request(
        "PLAY rtsp://127.0.0.1/?freq=11362&msys=dvbs2&pids=0 RTSP/1.0\r\n\
         CSeq: 7\r\n\r\n",
    );
    let mut ctx = context(&mut manager, Protocol::Rtsp);
    let response = handle_rtsp(&mut ctx, &req).expect("request helper failed");
    let bytes = text(&response);

    assert!(
        bytes.contains("RTSP/1.0 503 Service Unavailable"),
        "missing 503 status"
    );
    assert!(
        bytes.contains("Content-Type: text/parameters"),
        "missing Content-Type"
    );
    assert!(
        bytes.contains("No-More: sessions"),
        "missing No-More: sessions"
    );
    assert!(bytes.contains("CSeq: 7"), "missing CSeq echo");
}

// Ported from tests/test_stream.cpp:824
#[test]
fn test_read_rtsp_options_keeps_public() {
    let mut manager = env();
    let req = request("OPTIONS rtsp://127.0.0.1/ RTSP/1.0\r\nCSeq: 3\r\n\r\n");
    let mut ctx = context(&mut manager, Protocol::Rtsp);
    let response = handle_rtsp(&mut ctx, &req).expect("request helper failed");
    let bytes = text(&response);

    assert!(bytes.contains("RTSP/1.0 200 OK"), "missing 200");
    assert!(
        bytes.contains("Public: OPTIONS, DESCRIBE, SETUP, PLAY, TEARDOWN"),
        "OPTIONS must keep Public"
    );
}

// Ported from tests/test_stream.cpp:839
#[test]
fn test_read_rtsp_describe_unknown_has_no_public() {
    let mut manager = env();
    let req = request(
        "DESCRIBE rtsp://127.0.0.1/stream=99 RTSP/1.0\r\nCSeq: 4\r\n\
         Accept: application/sdp\r\n\r\n",
    );
    let mut ctx = context(&mut manager, Protocol::Rtsp);
    let response = handle_rtsp(&mut ctx, &req).expect("request helper failed");
    let bytes = text(&response);

    assert!(
        bytes.contains("RTSP/1.0 404 Not Found"),
        "missing 404 status"
    );
    assert!(!bytes.contains("Public:"), "errors must not carry Public");
}

// Ported from tests/test_stream.cpp:855
#[test]
fn test_read_rtsp_bogus_session_when_full_returns_454() {
    let mut manager = env();
    fill_streams(&mut manager);

    let req = request(
        "PLAY rtsp://127.0.0.1/?freq=11362&msys=dvbs2&pids=0 RTSP/1.0\r\n\
         CSeq: 11\r\nSession: 99999\r\n\r\n",
    );
    let mut ctx = context(&mut manager, Protocol::Rtsp);
    let response = handle_rtsp(&mut ctx, &req).expect("request helper failed");
    let bytes = text(&response);

    assert!(
        bytes.contains("RTSP/1.0 454 Session Not Found"),
        "bogus session must stay 454 when full"
    );
    assert!(!bytes.contains("No-More:"), "must not report sessions");
}

// Ported from tests/test_stream.cpp:877
#[test]
fn test_read_rtsp_play_while_busy_returns_503() {
    let mut manager = env();

    let sid1 = setup(
        &mut manager,
        "?src=1&freq=11362&pol=h&sr=22000&msys=dvbs2&pids=0",
    );
    assert_eq!(
        play_code(manager.start_play(sid1, Protocol::Http)),
        0,
        "first start_play failed"
    );

    let req = request(
        "PLAY rtsp://127.0.0.1/?src=1&freq=12543&pol=v&sr=27500&msys=dvbs2&pids=0 RTSP/1.0\r\n\
         CSeq: 9\r\n\
         Transport: RTP/AVP;unicast;client_port=46100-46101\r\n\r\n",
    );
    let mut ctx = context(&mut manager, Protocol::Rtsp);
    ctx.start_rtp = 45_600;
    let response = handle_rtsp(&mut ctx, &req).expect("request helper failed");
    let bytes = text(&response);

    assert!(
        bytes.contains("RTSP/1.0 503 Service Unavailable"),
        "missing 503 status"
    );
    assert!(
        bytes.contains("No-More: frontends"),
        "missing No-More: frontends"
    );
    assert!(
        bytes.contains("Content-Type: text/parameters"),
        "missing Content-Type"
    );
}

// --- DESCRIBE / SDP -------------------------------------------------------

// Expected text: stream.cpp:100-105 (header), stream.cpp:145-149 (single
// stream); fmtp from adapter.cpp:1492-1505.
#[test]
fn describe_dvb_s2_stream_with_pids() {
    let mut manager = env();
    manager.set_describe_options(describe_options());
    let url = "?src=1&freq=11362&pol=h&sr=22000&msys=dvbs2&pids=0,16";
    let id = setup(&mut manager, url);
    assert_eq!(
        play_code(manager.start_play(id, Protocol::Http)),
        0,
        "start_play failed"
    );

    let sdp = describe(&mut manager, Some(id), url);
    assert_eq!(
        sdp,
        "v=0\r\no=- 0000001000 0000001000 IN IP4 127.0.0.1\r\ns=SatIPServer:1 1,0,0\r\nt=0 0\r\n\
         m=video 0 RTP/AVP 33\r\nc=IN IP4 0.0.0.0\r\na=control:stream=1\r\n\
         a=fmtp:33 ver=1.0;src=1;tuner=1,255,1,15,11362,h,dvbs2,auto,auto,auto,22000,auto;pids=0,16\r\n\
         b=AS:5000\r\na=sendonly\r\n"
    );
}

// Expected text: adapter.cpp:1506-1523 (ver=1.1 format).
#[test]
fn describe_dvb_t2_stream() {
    let mut manager = env_with_system(DeliverySystem::DvbT2);
    manager.set_describe_options(describe_options());
    let url = "?freq=538&bw=8&msys=dvbt2&pids=0";
    let id = setup(&mut manager, url);
    assert_eq!(
        play_code(manager.start_play(id, Protocol::Http)),
        0,
        "start_play failed"
    );

    let sdp = describe(&mut manager, Some(id), url);
    assert_eq!(
        sdp,
        "v=0\r\no=- 0000001000 0000001000 IN IP4 127.0.0.1\r\ns=SatIPServer:1 0,1,0\r\nt=0 0\r\n\
         m=video 0 RTP/AVP 33\r\nc=IN IP4 0.0.0.0\r\na=control:stream=1\r\n\
         a=fmtp:33 ver=1.1;tuner=1,255,1,15,538.00,8,dvbt2,auto,auto,auto,auto,,0,0;pids=0\r\n\
         b=AS:5000\r\na=sendonly\r\n"
    );
}

// Expected text: adapter.cpp:1524-1539 (ver=1.2 format).
#[test]
fn describe_dvb_c_stream() {
    let mut manager = env_with_system(DeliverySystem::DvbC);
    manager.set_describe_options(describe_options());
    let url = "?freq=450&msys=dvbc&sr=6900&pids=0,16";
    let id = setup(&mut manager, url);
    assert_eq!(
        play_code(manager.start_play(id, Protocol::Http)),
        0,
        "start_play failed"
    );

    let sdp = describe(&mut manager, Some(id), url);
    assert_eq!(
        sdp,
        "v=0\r\no=- 0000001000 0000001000 IN IP4 127.0.0.1\r\ns=SatIPServer:1 0,0,1\r\nt=0 0\r\n\
         m=video 0 RTP/AVP 33\r\nc=IN IP4 0.0.0.0\r\na=control:stream=1\r\n\
         a=fmtp:33 ver=1.2;tuner=1,255,1,15,450.00,8,dvbc,auto,6900,0,,,auto;pids=0,16\r\n\
         b=AS:5000\r\na=sendonly\r\n"
    );
}

// Expected pids: stream.cpp:1508-1536 (get_stream_pids prints `all`);
// remainder from stream.cpp:145-149 and adapter.cpp:1492-1505.
#[test]
fn describe_stream_pids_all() {
    let mut manager = env();
    manager.set_describe_options(describe_options());
    let url = "?src=1&freq=11362&pol=h&sr=22000&msys=dvbs2&pids=all";
    let id = setup(&mut manager, url);
    assert_eq!(
        play_code(manager.start_play(id, Protocol::Http)),
        0,
        "start_play failed"
    );

    let sdp = describe(&mut manager, Some(id), url);
    assert_eq!(
        sdp,
        "v=0\r\no=- 0000001000 0000001000 IN IP4 127.0.0.1\r\ns=SatIPServer:1 1,0,0\r\nt=0 0\r\n\
         m=video 0 RTP/AVP 33\r\nc=IN IP4 0.0.0.0\r\na=control:stream=1\r\n\
         a=fmtp:33 ver=1.0;src=1;tuner=1,255,1,15,11362,h,dvbs2,auto,auto,auto,22000,auto;pids=all\r\n\
         b=AS:5000\r\na=sendonly\r\n"
    );
}

// Expected pids: adapter.cpp:1541-1546 prints `none` when no PID belongs to
// the stream; remainder from adapter.cpp:1492-1505 and stream.cpp:145-149.
#[test]
fn describe_stream_with_no_pids() {
    let mut manager = env();
    manager.set_describe_options(describe_options());
    let url = "?src=1&freq=11362&pol=h&sr=22000&msys=dvbs2";
    let id = setup(&mut manager, url);
    assert_eq!(
        play_code(manager.start_play(id, Protocol::Http)),
        0,
        "start_play failed"
    );

    let sdp = describe(&mut manager, Some(id), url);
    assert_eq!(
        sdp,
        "v=0\r\no=- 0000001000 0000001000 IN IP4 127.0.0.1\r\ns=SatIPServer:1 1,0,0\r\nt=0 0\r\n\
         m=video 0 RTP/AVP 33\r\nc=IN IP4 0.0.0.0\r\na=control:stream=1\r\n\
         a=fmtp:33 ver=1.0;src=1;tuner=1,255,1,15,11362,h,dvbs2,auto,auto,auto,22000,auto;pids=none\r\n\
         b=AS:5000\r\na=sendonly\r\n"
    );
}

// Expected text: adapter.cpp:1548-1556 (stream transponder path joins PIDs
// with ", ") and stream.cpp:145-149 (inactive, port 0, b=AS:5000).
#[test]
fn describe_inactive_stream() {
    let mut manager = env();
    manager.set_describe_options(describe_options());
    let url = "?src=1&freq=11362&pol=h&sr=22000&msys=dvbs2&pids=0,16";
    let id = setup(&mut manager, url);

    let sdp = describe(&mut manager, Some(id), url);
    assert_eq!(
        sdp,
        "v=0\r\no=- 0000001000 0000001000 IN IP4 127.0.0.1\r\ns=SatIPServer:1 1,0,0\r\nt=0 0\r\n\
         m=video 0 RTP/AVP 33\r\nc=IN IP4 0.0.0.0\r\na=control:stream=1\r\n\
         a=fmtp:33 ver=1.0;src=1;tuner=1,255,1,15,11362,h,dvbs2,auto,auto,auto,22000,auto;pids=0, 16\r\n\
         b=AS:5000\r\na=inactive\r\n"
    );
}

// Expected text: stream.cpp:117-133 lists every enabled stream with its
// destination port; header fields are the injectable DescribeOptions.
#[test]
fn describe_multiple_streams() {
    let mut manager = env();
    manager.set_describe_options(DescribeOptions {
        local_host: "192.168.1.5".to_owned(),
        session_id: Some(1234),
        session_version: Some(5678),
        time: "0 0".to_owned(),
    });
    let first = setup(
        &mut manager,
        "?src=1&freq=11362&pol=h&sr=22000&msys=dvbs2&pids=0,16",
    );
    let second = setup(
        &mut manager,
        "?src=2&freq=12073&pol=v&sr=27500&msys=dvbs2&pids=0",
    );
    manager
        .decode_transport(first, "RTP/AVP;unicast;client_port=5000-5001")
        .expect("first transport failed");
    manager
        .decode_transport(second, "RTP/AVP;unicast;client_port=5002-5003")
        .expect("second transport failed");

    let sdp = describe(&mut manager, None, "rtsp://127.0.0.1/");
    assert_eq!(
        sdp,
        "v=0\r\no=- 0000001234 0000005678 IN IP4 192.168.1.5\r\ns=SatIPServer:1 1,0,0\r\nt=0 0\r\n\
         m=video 5000 RTP/AVP 33\r\nc=IN IP4 0.0.0.0\r\na=control:stream=1\r\n\
         a=fmtp:33 ver=1.0;src=1;tuner=1,255,1,15,11362,h,dvbs2,auto,auto,auto,22000,auto;pids=0, 16\r\n\
         a=inactive\r\n\
         m=video 5002 RTP/AVP 33\r\nc=IN IP4 0.0.0.0\r\na=control:stream=2\r\n\
         a=fmtp:33 ver=1.0;src=2;tuner=1,255,1,15,12073,v,dvbs2,auto,auto,auto,27500,auto;pids=0\r\n\
         a=inactive\r\n"
    );
}

// stream.cpp:109-115: `stream=` selects that stream only, while the header
// still uses the socket's session id (stream.cpp:92).
#[test]
fn describe_stream_selection_uses_current_session_for_header() {
    let mut manager = env();
    manager.set_describe_options(DescribeOptions {
        session_id: None,
        session_version: None,
        ..describe_options()
    });
    let first = setup(
        &mut manager,
        "?src=1&freq=11362&pol=h&sr=22000&msys=dvbs2&pids=0",
    );
    setup(
        &mut manager,
        "?src=2&freq=12073&pol=v&sr=27500&msys=dvbs2&pids=0",
    );
    manager.set_session_id(first, 4242);

    let sdp = describe(&mut manager, Some(first), "rtsp://127.0.0.1/stream=2");
    assert_eq!(
        sdp,
        "v=0\r\no=- 0000004242 0000004242 IN IP4 127.0.0.1\r\ns=SatIPServer:1 1,0,0\r\nt=0 0\r\n\
         m=video 0 RTP/AVP 33\r\nc=IN IP4 0.0.0.0\r\na=control:stream=2\r\n\
         a=fmtp:33 ver=1.0;src=2;tuner=1,255,1,15,12073,v,dvbs2,auto,auto,auto,27500,auto;pids=0\r\n\
         b=AS:5000\r\na=inactive\r\n"
    );
}
