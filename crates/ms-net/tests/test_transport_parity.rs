//! `decode_transport`/`start_play` destination rules derived from stream.cpp.

use ms_net::handler::{HandlerContext, StreamType, handle_rtsp};
use ms_net::rtsp::{ParseOutcome, Protocol, parse_request};
use ms_net::session::SessionManager;
use ms_sched::{AdapterConfig, AdapterTuner, Scheduler, TuneError};
use ms_tuner::{DeliverySystem, TuneParams};
use std::io;
use std::net::UdpSocket;
use std::sync::{Arc, Mutex};

struct FakeTuner;

impl AdapterTuner for FakeTuner {
    fn tune(&mut self, _params: &TuneParams) -> Result<(), TuneError> {
        Ok(())
    }
}

const URL: &str = "rtsp://127.0.0.1/?src=1&freq=11362&pol=h&sr=22000&msys=dvbs2&pids=0";

fn manager() -> SessionManager {
    let mut sched = Scheduler::new();
    sched.add_adapter(
        0,
        AdapterConfig::new(DeliverySystem::DvbS2),
        Box::new(FakeTuner),
    );
    let mut manager = SessionManager::with_scheduler(sched);
    manager.set_start_rtp(45500);
    manager.set_transport_defaults(None, "239.255.255.250");
    manager
}

fn setup(manager: &mut SessionManager) -> i32 {
    manager
        .setup_stream_request(URL, None, "SETUP")
        .expect("setup_stream")
}

fn target(manager: &SessionManager, id: i32) -> (String, u16) {
    let stream = manager.stream(id).expect("enabled");
    (stream.remote_host.clone(), stream.remote_port)
}

// stream.cpp:505-506: unicast without destination= uses the RTSP peer.
#[test]
fn unicast_without_destination_uses_peer() {
    let mut manager = manager();
    let id = setup(&mut manager);
    manager
        .decode_transport_from(
            id,
            "RTP/AVP;unicast;client_port=5000-5001",
            "10.0.0.7",
            40000,
        )
        .unwrap();
    assert_eq!(target(&manager, id), ("10.0.0.7".to_owned(), 5000));
}

// stream.cpp:505-508: without `unicast` the destination falls back to disc_host.
#[test]
fn transport_without_mode_uses_disc_host() {
    let mut manager = manager();
    let id = setup(&mut manager);
    manager
        .decode_transport_from(id, "RTP/AVP;client_port=5000-5001", "10.0.0.7", 40000)
        .unwrap();
    assert_eq!(target(&manager, id), ("239.255.255.250".to_owned(), 5000));
}

// stream.cpp:509-510: a missing port is start_rtp, and `p` is rebuilt per call.
#[test]
fn missing_port_uses_start_rtp() {
    let mut manager = manager();
    let id = setup(&mut manager);
    manager
        .decode_transport_from(id, "RTP/AVP;unicast;client_port=5000", "10.0.0.7", 1)
        .unwrap();
    manager
        .decode_transport_from(id, "RTP/AVP;unicast", "10.0.0.7", 1)
        .unwrap();
    assert_eq!(target(&manager, id), ("10.0.0.7".to_owned(), 45500));
}

// stream.cpp:485-496: tokens are scanned in reverse, so the first port token wins.
#[test]
fn first_port_token_wins() {
    let mut manager = manager();
    let id = setup(&mut manager);
    manager
        .decode_transport_from(
            id,
            "RTP/AVP;unicast;port=6000-6001;client_port=5000-5001",
            "10.0.0.7",
            1,
        )
        .unwrap();
    assert_eq!(target(&manager, id).1, 6000);
}

// stream.cpp:503-504: `-r` replaces even an explicit destination.
#[test]
fn remote_rtp_option_overrides_destination() {
    let mut manager = manager();
    manager.set_transport_defaults(Some("192.168.1.50".to_owned()), "239.255.255.250");
    let id = setup(&mut manager);
    manager
        .decode_transport_from(
            id,
            "RTP/AVP;unicast;destination=10.1.1.1;client_port=5000",
            "10.0.0.7",
            1,
        )
        .unwrap();
    assert_eq!(target(&manager, id), ("192.168.1.50".to_owned(), 5000));
}

// stream.cpp:470-481: TCP copies the RTSP peer address and ignores port tokens.
#[test]
fn tcp_transport_uses_peer_address() {
    let mut manager = manager();
    let id = setup(&mut manager);
    manager
        .decode_transport_from(
            id,
            "RTP/AVP/TCP;unicast;client_port=5000-5001;interleaved=0-1",
            "10.0.0.7",
            40000,
        )
        .unwrap();
    assert_eq!(target(&manager, id), ("10.0.0.7".to_owned(), 40000));
    assert_eq!(manager.stream(id).unwrap().stream_type, StreamType::RtspTcp);
}

// stream.cpp:513-547: only a UDP stream is re-targeted; TCP keeps its transport.
#[test]
fn udp_transport_does_not_replace_tcp() {
    let mut manager = manager();
    let id = setup(&mut manager);
    manager
        .decode_transport_from(id, "RTP/AVP/TCP;interleaved=0-1", "10.0.0.7", 40000)
        .unwrap();
    manager
        .decode_transport_from(id, "RTP/AVP;unicast;client_port=5000", "10.0.0.7", 40000)
        .unwrap();
    assert_eq!(manager.stream(id).unwrap().stream_type, StreamType::RtspTcp);
    assert_eq!(target(&manager, id), ("10.0.0.7".to_owned(), 40000));
}

// stream.cpp:591-605: a new UDP target closes another stream sending to it.
#[test]
fn same_udp_destination_closes_older_stream() {
    let mut manager = manager();
    let first = setup(&mut manager);
    manager
        .decode_transport_from(first, "RTP/AVP;unicast;client_port=5000", "10.0.0.7", 1)
        .unwrap();
    let second = setup(&mut manager);
    manager
        .decode_transport_from(second, "RTP/AVP;unicast;client_port=5002", "10.0.0.7", 1)
        .unwrap();
    assert!(manager.stream(first).is_some(), "different port is kept");
    let third = setup(&mut manager);
    manager
        .decode_transport_from(third, "RTP/AVP;unicast;client_port=5000", "10.0.0.7", 1)
        .unwrap();
    assert!(manager.stream(first).is_none(), "same host:port is closed");
    assert!(manager.stream(second).is_some());
    assert!(manager.stream(third).is_some());
}

// stream.cpp:556-591: udp_bind_connect runs before the duplicate check, and
// a failure returns -1 (a 400) without closing the other stream.
#[test]
fn failed_udp_bind_rejects_the_transport() {
    let mut manager = manager();
    let first = setup(&mut manager);
    manager
        .decode_transport_from(first, "RTP/AVP;unicast;client_port=5000", "10.0.0.7", 1)
        .unwrap();
    let binds = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&binds);
    manager.set_udp_binder(Box::new(move |server_port, host, port| {
        seen.lock()
            .unwrap()
            .push((server_port, host.to_owned(), port));
        Err(io::Error::from(io::ErrorKind::AddrInUse))
    }));
    let second = setup(&mut manager);
    assert!(
        manager
            .decode_transport_from(second, "RTP/AVP;unicast;client_port=5000", "10.0.0.7", 1)
            .is_err()
    );
    assert!(manager.stream(first).is_some(), "duplicate kept on failure");
    assert!(manager.udp_sockets(second).is_none());
    let server_port = manager.stream(second).expect("enabled").server_port;
    assert_eq!(
        *binds.lock().unwrap(),
        [(server_port, "10.0.0.7".to_owned(), 5000)]
    );
}

// stream.cpp:513-550: after a failed bind `rsock < 0`, so a SETUP to the same
// destination neither rebinds nor fails.
#[test]
fn same_destination_after_failed_bind_is_left_alone() {
    let mut manager = manager();
    let calls = Arc::new(Mutex::new(0));
    let seen = Arc::clone(&calls);
    manager.set_udp_binder(Box::new(move |_, _, _| {
        *seen.lock().unwrap() += 1;
        Err(io::Error::from(io::ErrorKind::AddrInUse))
    }));
    let id = setup(&mut manager);
    let transport = "RTP/AVP;unicast;client_port=5000";
    assert!(
        manager
            .decode_transport_from(id, transport, "10.0.0.7", 1)
            .is_err()
    );
    manager
        .decode_transport_from(id, transport, "10.0.0.7", 1)
        .unwrap();
    assert_eq!(*calls.lock().unwrap(), 1, "no second bind");
    assert!(manager.udp_sockets(id).is_none());
}

// stream.cpp:513-541: a new client port replaces the stream's sockets.
#[test]
fn udp_bind_runs_at_setup_and_on_port_change() {
    let mut manager = manager();
    manager.set_udp_binder(Box::new(|_, host, port| {
        let rtp = UdpSocket::bind("127.0.0.1:0")?;
        rtp.connect((host, port))?;
        let rtcp = UdpSocket::bind("127.0.0.1:0")?;
        rtcp.connect((host, port + 1))?;
        Ok((rtp, rtcp))
    }));
    let id = setup(&mut manager);
    let peer = |manager: &SessionManager| {
        manager
            .udp_sockets(id)
            .map(|(rtp, _)| rtp.peer_addr().unwrap().port())
    };
    manager
        .decode_transport_from(id, "RTP/AVP;unicast;client_port=5000", "127.0.0.1", 1)
        .unwrap();
    assert_eq!(peer(&manager), Some(5000));
    manager
        .decode_transport_from(id, "RTP/AVP;unicast;client_port=5002", "127.0.0.1", 1)
        .unwrap();
    assert_eq!(peer(&manager), Some(5002));
    manager.close_stream(id);
    assert!(manager.udp_sockets(id).is_none(), "closed with the stream");
}

// stream.cpp:236-241: an HTTP stream records the client as its remote address.
#[test]
fn http_play_records_peer() {
    let mut manager = manager();
    let id = manager
        .setup_stream_request(URL, None, "GET")
        .expect("setup_stream");
    manager
        .start_play_from(id, Protocol::Http, "10.0.0.9", 51000)
        .unwrap();
    assert_eq!(target(&manager, id), ("10.0.0.9".to_owned(), 51000));
}

// read_rtsp passes the RTSP peer to decode_transport (minisatip.cpp:1286-1287).
#[test]
fn rtsp_setup_echoes_peer_destination() {
    let mut manager = manager();
    let text = format!(
        "SETUP {URL} RTSP/1.0\r\nCSeq: 1\r\nTransport: RTP/AVP;unicast;client_port=5000-5001\r\n\r\n"
    );
    let ParseOutcome::Complete { request, .. } = parse_request(text.as_bytes()) else {
        panic!("request did not parse");
    };
    let mut ctx = HandlerContext::new(&mut manager, Protocol::Rtsp).with_date("D");
    ctx.peer_host = "10.0.0.7".to_owned();
    ctx.peer_port = 40000;
    ctx.local_host = "10.0.0.1".to_owned();
    let response = handle_rtsp(&mut ctx, &request).expect("response");
    let text = String::from_utf8(response.to_bytes()).unwrap();
    assert!(
        text.contains(
            "Transport: RTP/AVP;unicast;destination=10.0.0.7;source=10.0.0.1;client_port=5000-5001;"
        ),
        "{text}"
    );
}
