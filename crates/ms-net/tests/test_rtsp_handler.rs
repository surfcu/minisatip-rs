//! RTSP/HTTP handler tests, including ports of the upstream test_stream cases.

use ms_net::handler::{
    HandlerContext, HttpResource, PlayError, StreamBackend, StreamId, StreamInfo, StreamType,
    TransportError, handle_http, handle_rtsp,
};
use ms_net::response::Response;
use ms_net::rtsp::{
    MAX_HEADERS, ParseOutcome, Protocol, Request, TransportKind, TransportMode, parse_request,
    parse_transport,
};
use ms_net::satip::parse_satip_url;
use ms_tuner::{DeliverySystem, PID_ALL, PidSet, Polarization, TuneParams};
use std::collections::BTreeMap;

const DATE: &str = "Wed, 21 Oct 2026 00:00:00 2026 GMT";
const SERVER: &str = "minisatip/1.2.3";

#[derive(Clone)]
struct FakeStream {
    info: StreamInfo,
    params: TuneParams,
}

#[derive(Default)]
struct FakeBackend {
    streams: BTreeMap<StreamId, FakeStream>,
    next_id: StreamId,
    full: bool,
    play_error: Option<PlayError>,
    sdp: Option<String>,
    resources: BTreeMap<String, HttpResource>,
    user_agents: BTreeMap<StreamId, String>,
}

impl FakeBackend {
    fn add_stream(&mut self, stream_type: StreamType) -> StreamId {
        let id = self.next_id;
        self.next_id += 1;
        let info = StreamInfo {
            stream_type,
            session_id: 1000 + i64::from(id),
            stream_number: id + 1,
            remote_host: "127.0.0.1".to_owned(),
            remote_port: 5000,
            server_port: 5500,
            rtcp_server_port: 5501,
            rtp_seq: 1,
            timeout_ms: None,
        };
        self.streams.insert(
            id,
            FakeStream {
                info,
                params: TuneParams::default(),
            },
        );
        id
    }
}

impl StreamBackend for FakeBackend {
    fn find_session(&mut self, session_id: i64) -> Option<StreamId> {
        self.streams
            .iter()
            .find(|(_, stream)| stream.info.session_id == session_id)
            .map(|(id, _)| *id)
    }

    fn get_stream(&self, id: StreamId) -> Option<StreamInfo> {
        self.streams.get(&id).map(|stream| stream.info.clone())
    }

    fn streams_full(&self) -> bool {
        self.full || self.streams.len() >= ms_net::handler::MAX_STREAMS as usize
    }

    fn setup_stream(&mut self, url: &str, current: Option<StreamId>) -> Option<StreamId> {
        if let Some(id) = current
            && let Some(stream) = self.streams.get_mut(&id)
        {
            parse_satip_url(url, &mut stream.params);
            return Some(id);
        }
        if self.streams_full() {
            return None;
        }
        let id = self.add_stream(StreamType::None);
        if let Some(stream) = self.streams.get_mut(&id) {
            parse_satip_url(url, &mut stream.params);
        }
        Some(id)
    }

    fn set_session_id(&mut self, id: StreamId, session_id: i64) {
        if let Some(stream) = self.streams.get_mut(&id) {
            stream.info.session_id = session_id;
        }
    }

    fn start_play(&mut self, _id: StreamId, _protocol: Protocol) -> Result<(), PlayError> {
        match &self.play_error {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        }
    }

    fn decode_transport(&mut self, id: StreamId, value: &str) -> Result<(), TransportError> {
        let Some(stream) = self.streams.get_mut(&id) else {
            return Err(TransportError);
        };
        let spec = parse_transport(value);
        stream.info.stream_type = match spec.kind {
            TransportKind::Udp => StreamType::RtspUdp,
            TransportKind::Tcp => StreamType::RtspTcp,
            TransportKind::Srt => StreamType::RtspSrt,
        };
        if let Some(port) = spec.client_port.or(spec.port) {
            stream.info.remote_port = port;
        }
        if let Some(destination) = spec.destination {
            stream.info.remote_host = destination;
        }
        Ok(())
    }

    fn close_stream(&mut self, id: StreamId) {
        self.streams.remove(&id);
    }

    fn describe_streams(&mut self, current: Option<StreamId>, _url: &str) -> Option<String> {
        current
            .filter(|id| self.streams.contains_key(id))
            .and_then(|_| self.sdp.clone())
    }

    fn http_resource(&mut self, uri: &str) -> Option<HttpResource> {
        self.resources.get(uri).cloned()
    }

    fn set_user_agent(&mut self, id: StreamId, agent: &str) {
        self.user_agents.insert(id, agent.to_owned());
    }
}

fn request(raw: &str) -> Request {
    match parse_request(raw.as_bytes()) {
        ParseOutcome::Complete { request, .. } => request,
        outcome => panic!("request did not parse: {outcome:?}"),
    }
}

fn context(backend: &mut FakeBackend, protocol: Protocol) -> HandlerContext<'_, FakeBackend> {
    HandlerContext::new(backend, protocol)
        .with_date(DATE)
        .with_server(SERVER)
}

fn text(response: &Response) -> String {
    String::from_utf8(response.to_bytes()).expect("responses are ASCII")
}

fn option_url(parameters: &str) -> String {
    format!("rtsp://127.0.0.1/?{parameters}")
}

fn parse_one(parameters: &str) -> TuneParams {
    let (params, _) = ms_net::satip::parse_satip_url_new(&option_url(parameters));
    params
}

// --- Ported tests ---------------------------------------------------------

fn fill_streams(backend: &mut FakeBackend) {
    while !backend.streams_full() {
        let id = backend.add_stream(StreamType::None);
        assert!(id >= 0, "streams_add failed before MAX_STREAMS");
    }
    assert!(backend.streams_full(), "streams should be full");
}

// Ported from tests/test_stream.cpp:778
#[test]
fn test_read_rtsp_empty_buffer_ignored() {
    let backend = FakeBackend::default();
    let buffer: Vec<u8> = Vec::new();

    // `read_rtsp` returns 0 immediately when `rlen` is 0. In Rust the request
    // reader is `parse_request`, which reports that an empty buffer needs more
    // data without consuming anything.
    assert_eq!(
        parse_request(&buffer),
        ParseOutcome::Incomplete { consumed: 0 },
        "empty buffer must be ignored"
    );

    // C++ asserts `s->flags == 0` and `s->buf` was not reallocated. The Rust
    // reader takes an immutable slice, so the buffer cannot change and no
    // dispatch happens: assert the observable equivalent.
    assert!(
        buffer.is_empty(),
        "empty buffer must leave the buffer alone"
    );
    assert!(backend.streams.is_empty(), "no state may be created");
}

// Ported from tests/test_stream.cpp:789
#[test]
fn test_read_http_empty_buffer_ignored() {
    let backend = FakeBackend::default();
    let buffer: Vec<u8> = Vec::new();

    // `read_http` returns 0 immediately when `rlen` is 0.
    assert_eq!(
        parse_request(&buffer),
        ParseOutcome::Incomplete { consumed: 0 },
        "empty buffer must be ignored"
    );

    assert!(
        buffer.is_empty(),
        "empty buffer must leave the buffer alone"
    );
    assert!(backend.streams.is_empty(), "no state may be created");
}

// Ported from tests/test_stream.cpp:800
#[test]
fn test_read_rtsp_sessions_exhausted_returns_503() {
    let mut backend = FakeBackend::default();
    fill_streams(&mut backend);
    assert!(backend.streams_full(), "streams should be full");

    let req = request(
        "PLAY rtsp://127.0.0.1/?freq=11362&msys=dvbs2&pids=0 RTSP/1.0\r\n\
         CSeq: 7\r\n\r\n",
    );
    let mut ctx = context(&mut backend, Protocol::Rtsp);
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
    let mut backend = FakeBackend::default();
    let req = request("OPTIONS rtsp://127.0.0.1/ RTSP/1.0\r\nCSeq: 3\r\n\r\n");
    let mut ctx = context(&mut backend, Protocol::Rtsp);
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
    let mut backend = FakeBackend::default();
    let req = request(
        "DESCRIBE rtsp://127.0.0.1/stream=99 RTSP/1.0\r\nCSeq: 4\r\n\
         Accept: application/sdp\r\n\r\n",
    );
    let mut ctx = context(&mut backend, Protocol::Rtsp);
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
    let mut backend = FakeBackend::default();
    fill_streams(&mut backend);

    let req = request(
        "PLAY rtsp://127.0.0.1/?freq=11362&msys=dvbs2&pids=0 RTSP/1.0\r\n\
         CSeq: 11\r\nSession: 99999\r\n\r\n",
    );
    let mut ctx = context(&mut backend, Protocol::Rtsp);
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
    let mut backend = FakeBackend::default();

    // The first stream owns the only frontend (C++ setup_stream + start_play).
    let id = backend
        .setup_stream("?src=1&freq=11362&pol=h&sr=22000&msys=dvbs2&pids=0", None)
        .expect("first setup_stream returned NULL");
    assert!(
        backend.start_play(id, Protocol::Http).is_ok(),
        "first start_play failed"
    );
    // The next PLAY on a different mux finds no free frontend.
    backend.play_error = Some(PlayError::new(503, "No-More: frontends".to_owned()));

    let req = request(
        "PLAY rtsp://127.0.0.1/?src=1&freq=12543&pol=v&sr=27500&msys=dvbs2&pids=0 RTSP/1.0\r\n\
         CSeq: 9\r\n\
         Transport: RTP/AVP;unicast;client_port=46100-46101\r\n\r\n",
    );
    let mut ctx = context(&mut backend, Protocol::Rtsp);
    ctx.start_rtp = 45600;
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

// Ported from tests/test_stream.cpp:349
#[test]
fn test_decode_transport_port_change_no_deadlock() {
    let mut backend = FakeBackend::default();
    let id = backend
        .setup_stream("?src=1&freq=11362&pol=h&sr=22000&pids=0", None)
        .expect("setup_stream returned NULL");

    backend
        .decode_transport(id, "RTP/AVP;unicast;client_port=46000-46001")
        .expect("first transport failed");
    assert_eq!(backend.get_stream(id).unwrap().remote_port, 46_000);

    // C++ runs the second decode while a demux thread holds the RTP socket
    // mutex and requires it to finish within 3 s. The fake backend owns no
    // sockets or locks, so only the resulting port change is observable here.
    // C++:368-370 `rs != NULL` and C++:371-391 (deadlock timeout) are not
    // ported: they cover the real RTP socket layer.
    backend
        .decode_transport(id, "RTP/AVP;unicast;client_port=46002-46003")
        .expect("second transport failed");
    // C++:393 `get_sockets(sid->rsock_id) != NULL` needs the real socket layer.
    assert!(backend.get_stream(id).is_some(), "stream must stay enabled");
    assert_eq!(
        backend.get_stream(id).unwrap().remote_port,
        46_002,
        "client port not updated"
    );
}

// Ported from tests/test_stream.cpp:257
#[test]
fn test_setup_stream_unspecified_vs_empty_pids() {
    let mut backend = FakeBackend::default();

    // Setup initially with some pids.
    let id = backend
        .setup_stream(
            "?src=1&freq=11362&pol=h&sr=22000&msys=dvbs2&pids=0,16&addpids=100",
            None,
        )
        .expect("setup_stream failed");
    let expected = PidSet::from_pids([0, 16, 100]);
    assert_eq!(
        backend.streams[&id].params.pids, expected,
        "pids not set correctly"
    );

    // Setup again with unspecified pids and no freq=: they are inherited.
    backend
        .setup_stream("?pol=v", Some(id))
        .expect("setup_stream failed");
    assert_eq!(
        backend.streams[&id].params.pids, expected,
        "pids should be inherited when unspecified"
    );

    // Setup again with freq=: the pids are cleared.
    backend
        .setup_stream("?freq=11362&pol=h&sr=22000&msys=dvbs2", Some(id))
        .expect("setup_stream failed");
    assert!(
        backend.streams[&id].params.pids.is_empty(),
        "pids should be cleared when freq= is specified"
    );
}

// --- One test per URL parameter ------------------------------------------

#[test]
fn url_msys() {
    assert_eq!(parse_one("msys=dvbs2").msys, DeliverySystem::DvbS2);
    assert_eq!(parse_one("msys=DVBT").msys, DeliverySystem::DvbT);
    assert_eq!(parse_one("msys=dvbc2").msys, DeliverySystem::DvbC2);
}

#[test]
fn url_freq() {
    // kHz, like the C++ `parse_float(val, 1000)`.
    assert_eq!(parse_one("freq=11361.75").freq, 11_361_750);
    assert_eq!(parse_one("freq=11362").freq, 11_362_000);
    assert_eq!(parse_one("freq=538").freq, 538_000);
    // The C++ rounds through `float` before multiplying and truncating.
    assert_eq!(parse_one("freq=474.166").freq, 474_165);
    assert_eq!(parse_one("freq=11550").freq, 11_550_000);
    assert_eq!(parse_one("freq=11550.9").freq, 11_550_900);
    assert_eq!(parse_one("freq=0").freq, 0);
    assert_eq!(parse_one("freq=abc").freq, 0);
    assert_eq!(parse_one("freq=11362junk").freq, 11_362_000);
    assert_eq!(parse_one("freq=-11362").freq, -11_362_000);
    assert_eq!(parse_one("freq=-0.5").freq, -500);
}

#[test]
fn url_pol() {
    assert_eq!(parse_one("pol=v").pol, Polarization::Vertical);
    assert_eq!(parse_one("pol=H").pol, Polarization::Horizontal);
    assert_eq!(parse_one("pol=r").pol, Polarization::Right);
    assert_eq!(parse_one("pol=l").pol, Polarization::Left);
    assert_eq!(parse_one("pol=none").pol, Polarization::None);
}

#[test]
fn url_sr() {
    // sym/s, like the C++ `tp->sr = sr * 1000`.
    assert_eq!(parse_one("sr=22000").sr, 22_000_000);
    assert_eq!(parse_one("sr=27500").sr, 27_500_000);
    assert_eq!(parse_one("sr=0").sr, 0);
    assert_eq!(parse_one("sr=-1").sr, 0);
    assert_eq!(parse_one("sr=1000000").sr, 1_000_000_000);
    assert_eq!(parse_one("sr=1000001").sr, 0);
    assert_eq!(parse_one("sr=2000000").sr, 0);
}

#[test]
fn url_fe() {
    assert_eq!(parse_one("fe=1").fe, Some(1));
    assert_eq!(parse_one("fe=abc").fe, Some(0));
}

#[test]
fn url_src() {
    assert_eq!(parse_one("src=2").src, 2);
    assert_eq!(parse_one("src=abc").src, 0);
}

#[test]
fn url_ro() {
    assert_eq!(parse_one("ro=0.35").ro, Some(0));
    assert_eq!(parse_one("ro=0.20").ro, Some(1));
    assert_eq!(parse_one("ro=bogus").ro, None);
}

#[test]
fn url_mtype() {
    assert_eq!(parse_one("mtype=qpsk").mtype, 0);
    assert_eq!(parse_one("mtype=8psk").mtype, 9);
    assert_eq!(parse_one("mtype=256qam").mtype, 5);
}

#[test]
fn url_fec() {
    assert_eq!(parse_one("fec=12").fec, 1);
    assert_eq!(parse_one("fec=34").fec, 3);
    assert_eq!(parse_one("fec=910").fec, 11);
    assert_eq!(parse_one("fec=bogus").fec, 0);
}

#[test]
fn url_plts() {
    assert_eq!(parse_one("plts=on").plts, Some(true));
    assert_eq!(parse_one("plts=off").plts, Some(false));
    assert_eq!(parse_one("plts=auto").plts, None);
}

#[test]
fn url_gi() {
    assert_eq!(parse_one("gi=132").gi, Some(0));
    assert_eq!(parse_one("gi=18").gi, Some(2));
    assert_eq!(parse_one("gi=pn945").gi, Some(10));
}

#[test]
fn url_tmode() {
    assert_eq!(parse_one("tmode=2k").tmode, Some(0));
    assert_eq!(parse_one("tmode=8k").tmode, Some(1));
    assert_eq!(parse_one("tmode=32k").tmode, Some(6));
}

#[test]
fn url_bw() {
    assert_eq!(parse_one("bw=8").bw, Some(8_000_000));
    assert_eq!(parse_one("bw=0.008").bw, Some(8_000));
    assert_eq!(parse_one("bw=8000000").bw, Some(0));
}

#[test]
fn url_specinv() {
    assert_eq!(parse_one("specinv=1").specinv, Some(1));
    assert_eq!(parse_one("specinv=abc").specinv, Some(0));
}

#[test]
fn url_c2tft() {
    assert_eq!(parse_one("c2tft=2").c2tft, Some(2));
}

#[test]
fn url_ds() {
    assert_eq!(parse_one("ds=3").ds, Some(3));
}

#[test]
fn url_plp() {
    assert_eq!(parse_one("plp=4").plp, Some(4));
    assert_eq!(parse_one("isi=5").plp, Some(5));
    assert_eq!(parse_one("plp=abc").plp, Some(0));
}

#[test]
fn url_t2id() {
    assert_eq!(parse_one("t2id=5").t2id, Some(5));
}

#[test]
fn url_sm() {
    assert_eq!(parse_one("sm=6").sm, Some(6));
}

#[test]
fn url_plsm_plsc() {
    assert_eq!(parse_one("plsm=gold&plsc=1").plsm, Some(1));
    assert_eq!(parse_one("plsm=gold&plsc=1").plsc, Some(1));
    assert!(parse_one("plsm=root&plsc=1").plsc.is_some());
}

#[test]
fn url_x_pmt() {
    assert_eq!(parse_one("x_pmt=4096").x_pmt, Some(4096));
    assert_eq!(parse_one("x_pmt=-1").x_pmt, None);
}

#[test]
fn url_pids() {
    let params = parse_one("pids=1,2,300");
    assert_eq!(params.pids, PidSet::from_pids([1, 2, 300]));
    assert!(parse_one("pids=all").pids.is_all());
    assert!(parse_one("pids=none").pids.is_empty());
    assert_eq!(parse_one("pids=").pids, PidSet::new());
}

#[test]
fn url_addpids() {
    let mut params = TuneParams {
        pids: PidSet::from_pids([1]),
        ..TuneParams::default()
    };
    parse_satip_url(&option_url("addpids=2,3"), &mut params);
    assert_eq!(params.pids, PidSet::from_pids([1, 2, 3]));
    parse_satip_url(&option_url("addpids=all"), &mut params);
    assert!(params.pids.is_all());
}

#[test]
fn url_delpids() {
    let mut params = TuneParams {
        pids: PidSet::from_pids([1, 2, 3]),
        ..TuneParams::default()
    };
    parse_satip_url(&option_url("delpids=2,3"), &mut params);
    assert_eq!(params.pids, PidSet::from_pids([1]));
    parse_satip_url(&option_url("delpids=all"), &mut params);
    assert_eq!(params.pids, PidSet::from_pids([1]));
    assert!(!params.pids.contains(PID_ALL));
}

#[test]
fn freq_parameter_clears_previous_pids() {
    let mut params = TuneParams {
        pids: PidSet::from_pids([1, 2]),
        ..TuneParams::default()
    };
    let report = parse_satip_url(&option_url("freq=11550"), &mut params);
    assert!(report.reset_pids);
    assert!(params.pids.is_empty());
}

// --- Parsing edge cases ---------------------------------------------------

#[test]
fn oversized_incomplete_buffer_is_discarded() {
    let buffer = vec![b'X'; ms_net::rtsp::DISCARD_THRESHOLD + 1];
    assert_eq!(parse_request(&buffer), ParseOutcome::Discarded);
}

#[test]
fn malformed_request_line_is_rejected() {
    assert_eq!(parse_request(b"garbage\r\n\r\n"), ParseOutcome::Malformed);
    assert_eq!(parse_request(b"OPTIONS\r\n\r\n"), ParseOutcome::Malformed);
}

#[test]
fn user_agent_keeps_only_the_first_token() {
    // header_parameter returns the next whitespace-delimited token
    // (utils.cpp:145-172; minisatip.cpp:1320-1325).
    let mut backend = FakeBackend::default();
    backend.add_stream(StreamType::RtspUdp);
    let request = request(
        "PLAY rtsp://127.0.0.1/ RTSP/1.0\r\nCSeq: 1\r\nSession: 1000\r\nUser-Agent: Mozilla/5.0 (X11; Linux)\r\n\r\n",
    );
    let response =
        handle_rtsp(&mut context(&mut backend, Protocol::Rtsp), &request).expect("PLAY answered");
    assert_eq!(response.status, 200);
    assert_eq!(
        backend.user_agents.get(&0).map(String::as_str),
        Some("Mozilla/5.0")
    );
}

#[test]
fn too_many_headers_is_rejected() {
    let mut raw = String::from("OPTIONS rtsp://x/ RTSP/1.0\r\n");
    for _ in 0..=MAX_HEADERS {
        raw.push_str("X-Test: value\r\n");
    }
    raw.push_str("\r\n");
    assert_eq!(parse_request(raw.as_bytes()), ParseOutcome::Malformed);
}

#[test]
fn partial_headers_wait_for_more_data() {
    let partial = b"OPTIONS rtsp://x/ RTSP/1.0\r\nCSeq: 1\r\n";
    assert_eq!(
        parse_request(partial),
        ParseOutcome::Incomplete { consumed: 0 }
    );

    let complete = b"OPTIONS rtsp://x/ RTSP/1.0\r\nCSeq: 1\r\n\r\n";
    let ParseOutcome::Complete { request, consumed } = parse_request(complete) else {
        panic!("complete request expected");
    };
    assert_eq!(consumed, complete.len());
    assert_eq!(request.cseq(), 1);
}

#[test]
fn content_length_matches_cpp_four_byte_slack() {
    // C++ `is_rtsp_http_header` checks `terminator + Content-Length <= len`,
    // so a request is complete four bytes before the declared body ends
    // (DEV-026, withdrawn).
    let header = b"POST /x HTTP/1.0\r\nContent-Length: 4\r\n\r\n";

    let ParseOutcome::Complete { request, consumed } = parse_request(header) else {
        panic!("C++ accepts the header before any body byte arrives");
    };
    assert!(request.body.is_empty());
    assert_eq!(consumed, header.len());

    let mut body = header.to_vec();
    body.extend_from_slice(b"abc");
    let ParseOutcome::Complete { request, consumed } = parse_request(&body) else {
        panic!("three of four body bytes are already enough");
    };
    assert_eq!(request.body, b"abc");
    assert_eq!(consumed, body.len());

    // For a longer body the threshold is exactly four bytes short.
    let header = b"POST /x HTTP/1.0\r\nContent-Length: 10\r\n\r\n";
    let mut short = header.to_vec();
    short.extend_from_slice(&[b'x'; 5]);
    assert_eq!(
        parse_request(&short),
        ParseOutcome::Incomplete { consumed: 0 }
    );

    let mut complete = header.to_vec();
    complete.extend_from_slice(&[b'x'; 6]);
    let ParseOutcome::Complete { request, consumed } = parse_request(&complete) else {
        panic!("six of ten body bytes complete the request");
    };
    assert_eq!(request.body, [b'x'; 6]);
    assert_eq!(consumed, complete.len());

    let mut full = header.to_vec();
    full.extend_from_slice(&[b'x'; 10]);
    let ParseOutcome::Complete { request, consumed } = parse_request(&full) else {
        panic!("a full body must also complete the request");
    };
    assert_eq!(request.body, [b'x'; 10]);
    assert_eq!(consumed, full.len());
}

#[test]
fn session_header_keeps_id_and_timeout_suffix() {
    let req = request("PLAY rtsp://x/ RTSP/1.0\r\nCSeq: 7\r\nSession: 12345;timeout=60\r\n\r\n");
    let session = req.session().unwrap();
    assert_eq!(session.id, 12345);
    assert_eq!(session.timeout, Some(60));
}

#[test]
fn interleaved_frames_are_skipped_before_the_request() {
    let mut buffer = vec![0x24, 0, 0, 4, 0xAA, 0xBB, 0xCC, 0xDD];
    let raw = b"OPTIONS rtsp://x/ RTSP/1.0\r\nCSeq: 1\r\n\r\n";
    buffer.extend_from_slice(raw);

    let ParseOutcome::Complete { request, consumed } = parse_request(&buffer) else {
        panic!("request after interleaved frame expected");
    };
    assert_eq!(request.method, "OPTIONS");
    assert_eq!(consumed, 8 + raw.len());
}

#[test]
fn partial_interleaved_frame_waits() {
    let buffer = vec![0x24, 0, 0, 10, 1, 2, 3];
    assert_eq!(
        parse_request(&buffer),
        ParseOutcome::Incomplete { consumed: 0 }
    );
}

#[test]
fn interleaved_frame_only_is_ignored() {
    let buffer = vec![0x24, 1, 0, 4, 0xAA, 0xBB, 0xCC, 0xDD];
    assert_eq!(
        parse_request(&buffer),
        ParseOutcome::Ignored { consumed: 8 }
    );
}

#[test]
fn transport_unicast() {
    let spec = parse_transport("RTP/AVP;unicast;client_port=5000-5001;destination=10.0.0.2");
    assert_eq!(spec.kind, TransportKind::Udp);
    assert_eq!(spec.mode, Some(TransportMode::Unicast));
    assert_eq!(spec.client_port, Some(5000));
    assert_eq!(spec.destination.as_deref(), Some("10.0.0.2"));
}

#[test]
fn transport_multicast() {
    let spec = parse_transport("RTP/AVP;multicast;destination=239.1.1.1;port=6000-6001;ttl=32");
    assert_eq!(spec.mode, Some(TransportMode::Multicast));
    assert_eq!(spec.port, Some(6000));
    assert_eq!(spec.ttl, Some(32));
}

#[test]
fn transport_tcp_interleaved() {
    let spec = parse_transport("RTP/AVP/TCP;unicast;interleaved=2-3");
    assert_eq!(spec.kind, TransportKind::Tcp);
    assert_eq!(spec.interleaved, Some(2));
}

#[test]
fn transport_srt() {
    let spec = parse_transport("RTP/AVP/SRT;srt_streamid=abc123");
    assert_eq!(spec.kind, TransportKind::Srt);
    assert_eq!(spec.srt_streamid.as_deref(), Some("abc123"));
}

// --- HTTP and response formatting ----------------------------------------

#[test]
fn http_get_root_uses_backend_resource() {
    let mut backend = FakeBackend::default();
    backend.resources.insert(
        "/hello".to_owned(),
        HttpResource {
            content_type: "text/plain".to_owned(),
            body: b"hello".to_vec(),
        },
    );
    let req = request("GET /hello HTTP/1.0\r\n\r\n");
    let mut ctx = context(&mut backend, Protocol::Http);
    let response = handle_http(&mut ctx, &req).unwrap();
    let bytes = text(&response);

    assert_eq!(response.status, 200);
    assert!(bytes.starts_with("HTTP/1.0 200 OK\r\n"));
    assert!(bytes.contains("Content-Type: text/plain\r\n"));
    assert!(bytes.contains("Content-Length: 5\r\n\r\nhello"));
    assert!(!bytes.contains("CSeq:"));
}

#[test]
fn http_head_sends_headers_without_body() {
    let mut backend = FakeBackend::default();
    backend.resources.insert(
        "/hello".to_owned(),
        HttpResource {
            content_type: "text/plain".to_owned(),
            body: b"hello".to_vec(),
        },
    );
    let req = request("HEAD /hello HTTP/1.0\r\n\r\n");
    let mut ctx = context(&mut backend, Protocol::Http);
    let response = handle_http(&mut ctx, &req).unwrap();

    assert_eq!(response.status, 200);
    assert!(response.body.is_empty());
    assert!(!text(&response).contains("Content-Length:"));
}

#[test]
fn http_get_satip_url_is_dispatched_as_rtsp() {
    let mut backend = FakeBackend {
        play_error: Some(PlayError::new(503, "No-More: frontends".to_owned())),
        ..FakeBackend::default()
    };
    let req = request("GET /?freq=11550 HTTP/1.0\r\n\r\n");
    let mut ctx = context(&mut backend, Protocol::Http);
    let response = handle_http(&mut ctx, &req).unwrap();

    assert_eq!(response.status, 503);
    assert!(text(&response).starts_with("HTTP/1.0 503 Service Unavailable\r\n"));
}

#[test]
fn unserialized_response_has_exact_cpp_layout() {
    let mut backend = FakeBackend::default();
    let req = request("OPTIONS rtsp://x/ RTSP/1.0\r\nCSeq: 9\r\n\r\n");
    let mut ctx = context(&mut backend, Protocol::Rtsp);
    let response = handle_rtsp(&mut ctx, &req).unwrap();
    let expected = format!(
        "RTSP/1.0 200 OK\r\nDate: {DATE}\r\nCSeq: 9\r\nServer: {SERVER}\r\n\
         Public: OPTIONS, DESCRIBE, SETUP, PLAY, TEARDOWN\r\n\r\n"
    );
    assert_eq!(text(&response), expected);
}

#[test]
fn describe_known_stream_has_sdp_and_session() {
    let mut backend = FakeBackend::default();
    let id = backend.add_stream(StreamType::RtspUdp);
    backend.sdp = Some("v=0\r\n".to_owned());
    let req = request("DESCRIBE rtsp://x/stream=1 RTSP/1.0\r\nCSeq: 10\r\n\r\n");
    let mut ctx = context(&mut backend, Protocol::Rtsp);
    ctx.sid = Some(id);
    let response = handle_rtsp(&mut ctx, &req).unwrap();
    let bytes = text(&response);

    assert_eq!(response.status, 200);
    assert!(bytes.contains("Session: 0000001000\r\n"));
    assert!(bytes.contains("Content-type: application/sdp\r\n"));
    assert!(bytes.contains("Content-Base: rtsp://127.0.0.1/\r\n"));
    assert!(bytes.contains("Content-Length: 5\r\n\r\nv=0\r\n"));
}

#[test]
fn unimplemented_method_returns_501_with_public_only() {
    let mut backend = FakeBackend::default();
    let req = request("PAUSE rtsp://x/ RTSP/1.0\r\nCSeq: 11\r\n\r\n");
    let mut ctx = context(&mut backend, Protocol::Rtsp);
    let response = handle_rtsp(&mut ctx, &req).unwrap();
    let bytes = text(&response);

    assert_eq!(response.status, 501);
    assert!(bytes.contains("Public: "));
    assert!(!bytes.contains("Date:"));
    assert!(!bytes.contains("Server:"));
}

#[test]
fn request_with_transport_but_no_stream_is_bad_request() {
    let mut backend = FakeBackend::default();
    let req = request(
        "OPTIONS rtsp://x/ RTSP/1.0\r\nCSeq: 12\r\n\
         Transport: RTP/AVP;unicast;client_port=5000\r\n\r\n",
    );
    let mut ctx = context(&mut backend, Protocol::Rtsp);
    let response = handle_rtsp(&mut ctx, &req).unwrap();
    assert_eq!(response.status, 400);
}
