//! Ports of tests/test_satipc_rtsp.cpp: the RTSP loop against a scripted fake
//! server, and the reconnect/open paths against loopback sockets.

mod common;

use std::io::Read;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::os::fd::AsRawFd;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use common::fake_satip_server::FakeSatipServer;
#[cfg(feature = "srt")]
use ms_srt::SRT_INVALID_SOCK;
use ms_tuner::satipc::{
    Driver, MAX_QUALITY, MAX_STRENGTH, PidSlot, PidState, SatipServer, SatipcConfig, State,
    Transport,
};
use ms_tuner::{DeliverySystem, Polarization, TuneParams};

const SETUP_OK: &str = "RTSP/1.0 200 OK\r\nCSeq: 1\r\n\
Transport: RTP/AVP;unicast;destination=192.168.1.37;client_port=6508-6509;\
server_port=6974-6975\r\n\
Session: EDCD7C81;timeout=60\r\n\
com.ses.streamID: 308\r\n\r\n";
const PLAY_OK: &str = "RTSP/1.0 200 OK\r\nCSeq: 2\r\nSession: EDCD7C81\r\n\r\n";
const PLAY_503: &str = "RTSP/1.0 503 Service Unavailable\r\nCSeq: 2\r\n\r\n";
const PLAY_454: &str = "RTSP/1.0 454 Session Not Found\r\nCSeq: 2\r\n\r\n";
const SETUP_403: &str = "RTSP/1.0 403 Forbidden\r\nCSeq: 1\r\n\
Content-Type: text/parameters\r\nContent-Length: 17\r\n\r\n\
Out-of-Range: src\r\n";
const PLAY_403: &str = "RTSP/1.0 403 Forbidden\r\nCSeq: 2\r\n\
Content-Type: text/parameters\r\nContent-Length: 17\r\n\r\n\
Out-of-Range: src\r\n";
const SETUP_500: &str = "RTSP/1.0 500 Internal Server Error\r\nCSeq: 1\r\n\r\n";
const PLAY_503_PIDS: &str = "RTSP/1.0 503 Service Unavailable\r\nCSeq: 2\r\n\
Content-Type: text/parameters\r\nContent-Length: 14\r\n\r\n\
No-More: pids\r\n";
const PLAY_503_FRONTENDS: &str = "RTSP/1.0 503 Service Unavailable\r\nCSeq: 2\r\n\
Content-Type: text/parameters\r\nContent-Length: 18\r\n\r\n\
No-More: frontends\r\n";
const TEARDOWN_OK: &str = "RTSP/1.0 200 OK\r\nCSeq: 3\r\nSession: EDCD7C81\r\n\r\n";
const DESCRIBE_OK: &str = "RTSP/1.0 200 OK\r\nCSeq: 4\r\nContent-Type: application/sdp\r\n\r\n\
tuner=1,215,1,12,0,0,0,0\r\n";
const OPTIONS_OK: &str = "RTSP/1.0 200 OK\r\nCSeq: 9\r\n\
Public: OPTIONS, DESCRIBE, SETUP, PLAY, TEARDOWN\r\n\r\n";

// The socket tests bind fixed RTP/RTCP ports, so the tests of this file run
// one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn config(aid: usize, host: &str, port: u16, start_rtp: u16) -> SatipcConfig {
    SatipcConfig {
        adapter_id: aid,
        server: SatipServer {
            host: host.to_owned(),
            port,
            fe: -1,
            delsys: DeliverySystem::DvbS2,
            source_ip: String::new(),
            transport: Transport::Udp,
            no_pids_all: false,
        },
        start_rtp,
        user_agent: "minisatip test".to_owned(),
        adapter_timeout_ms: 0,
        ignore_signal: false,
        bind_dev_ip: None,
        receive_buffer: 0,
    }
}

/// Ported from tests/test_satipc_rtsp.cpp:69 (`RtspFixture`).
struct RtspFixture {
    d: Driver,
    pid_slots: usize,
}

impl RtspFixture {
    fn setup() -> Self {
        let mut d = Driver::new(&config(0, "192.168.1.43", 554, 5500));
        let sip = &mut d.client;
        sip.tp = TuneParams::default();
        sip.tp.src = 1;
        sip.tp.src_specified = true;
        sip.tp.freq = 11_362_000;
        sip.tp.msys = DeliverySystem::DvbS2;
        sip.tp.pol = Polarization::Horizontal;
        sip.tp.sr = 22_000_000;
        sip.tp.fec = 2;
        sip.tp.fec_specified = true;
        sip.sys0 = DeliverySystem::DvbS2;
        sip.sid_cnt = 1;
        sip.data_open = false; // ad.sock = -1
        sip.rtsp_connected = true; // ad.fe_sock = rtsp_id (the pipe)
        sip.enabled = true;
        sip.state = State::Setup;
        sip.stream_id = -1;
        sip.cseq = 1;
        sip.timeout_ms = 30_000;
        Self { d, pid_slots: 0 }
    }

    fn run(&mut self, srv: &mut FakeSatipServer) -> i32 {
        srv.run(&mut self.d.client)
    }
}

// Mirrors update_pids: queues the pid with satipc and marks it on the adapter.
fn add_pid(fx: &mut RtspFixture, pid: u16) {
    fx.d.client.set_pid(pid);
    fx.d.client.pids.push(Some(PidSlot {
        pid,
        flags: PidState::Active,
        fd: true,
    }));
    fx.pid_slots += 1;
}

fn get_cseq(req: &str) -> i32 {
    req.find("CSeq: ")
        .and_then(|pos| req[pos + 6..].split("\r\n").next())
        .and_then(|value| value.parse().ok())
        .unwrap_or(-1)
}

fn has_prefix(req: &str, prefix: &str) -> bool {
    req.starts_with(prefix)
}

fn has_part(req: &str, part: &str) -> bool {
    req.contains(part)
}

fn tuned_fixture(script: &[(&str, &str)]) -> (RtspFixture, FakeSatipServer) {
    let mut fx = RtspFixture::setup();
    let mut srv = FakeSatipServer::new(script);
    fx.d.client.tune();
    add_pid(&mut fx, 0);
    fx.d.client.commit();
    assert!(fx.run(&mut srv) > 0, "initial tune must complete");
    (fx, srv)
}

// Ported from tests/test_satipc_rtsp.cpp:163
#[test]
fn test_rtsp_happy_loop() {
    let _serial = serial();
    let mut fx = RtspFixture::setup();
    let mut srv = FakeSatipServer::new(&[
        ("SETUP ", SETUP_OK),
        ("PLAY ", PLAY_OK),
        ("TEARDOWN ", TEARDOWN_OK),
        ("DESCRIBE ", DESCRIBE_OK),
    ]);

    fx.d.client.tune();
    add_pid(&mut fx, 0);
    add_pid(&mut fx, 16);
    add_pid(&mut fx, 17);
    fx.d.client.commit();
    assert_eq!(
        fx.run(&mut srv),
        3,
        "tune must exchange SETUP, PLAY and DESCRIBE"
    );
    assert_eq!(srv.requests.len(), 3, "expected exactly 3 requests");
    assert!(
        has_prefix(&srv.requests[0], "SETUP ") && has_part(&srv.requests[0], "pids=none"),
        "first request must SETUP with pids=none"
    );
    assert!(
        has_prefix(&srv.requests[1], "PLAY ")
            && has_part(&srv.requests[1], "stream=308")
            && has_part(&srv.requests[1], "Session: EDCD7C81")
            && has_part(&srv.requests[1], "addpids=0,16,17"),
        "PLAY must carry the session, stream and pids"
    );
    assert!(
        has_prefix(&srv.requests[2], "DESCRIBE "),
        "idle client must ask for signal with DESCRIBE"
    );
    assert!(
        get_cseq(&srv.requests[0]) == 1
            && get_cseq(&srv.requests[1]) == 2
            && get_cseq(&srv.requests[2]) == 3,
        "CSeq must count up from 1"
    );
    assert_ne!(fx.d.client.strength, 0, "signal must come from DESCRIBE");
    assert_eq!(fx.d.client.state, State::Play, "client must end up playing");

    // Closing the last stream mirrors close_adapter_for_stream: standby
    // first, then the transponder is cleared so nothing restarts.
    fx.d.client.standby();
    fx.d.client.tp = TuneParams::default();
    assert_eq!(fx.run(&mut srv), 1, "standby must exchange only TEARDOWN");
    assert!(
        has_prefix(&srv.requests[3], "TEARDOWN ") && has_part(&srv.requests[3], "stream=308"),
        "last request must tear down the stream"
    );
    assert_eq!(
        fx.d.client.state,
        State::Inactive,
        "client must go inactive after TEARDOWN"
    );
}

// Ported from tests/test_satipc_rtsp.cpp:215
#[test]
fn test_rtsp_503_recovery() {
    let _serial = serial();
    let (mut fx, mut srv) = tuned_fixture(&[
        ("SETUP ", SETUP_OK),
        (
            "PLAY rtsp://192.168.1.43:554/stream=308?addpids=5167",
            PLAY_503,
        ),
        ("PLAY ", PLAY_OK),
        ("TEARDOWN ", TEARDOWN_OK),
        ("DESCRIBE ", DESCRIBE_OK),
    ]);

    let base = srv.requests.len();
    add_pid(&mut fx, 5167);
    add_pid(&mut fx, 5171);
    fx.d.client.commit();
    assert_eq!(
        fx.run(&mut srv),
        4,
        "503 must cycle PLAY, TEARDOWN, SETUP, PLAY"
    );
    assert!(
        has_prefix(&srv.requests[base], "PLAY ")
            && has_prefix(&srv.requests[base + 1], "TEARDOWN ")
            && has_prefix(&srv.requests[base + 2], "SETUP ")
            && has_prefix(&srv.requests[base + 3], "PLAY "),
        "503 must tear down and re-establish the session"
    );
    assert!(
        has_part(&srv.requests[base + 3], "pids=0,5167,5171"),
        "recovery PLAY must resend the full pid list"
    );
    assert!(!fx.d.client.force_pids, "full pid list goes out once");
    assert_eq!(fx.d.client.state, State::Play, "client must end up playing");
    assert_eq!(fx.d.client.stream_id, 308, "stream must be reattached");
    assert!(!fx.d.client.err, "recovery must not flag the adapter");
}

// Ported from tests/test_satipc_rtsp.cpp:257
#[test]
fn test_rtsp_403_setup_gives_up() {
    let _serial = serial();
    let mut fx = RtspFixture::setup();
    let mut srv = FakeSatipServer::new(&[("SETUP ", SETUP_403)]);
    fx.d.client.tune();
    add_pid(&mut fx, 0);
    fx.d.client.commit();
    assert_eq!(fx.run(&mut srv), 1, "403 on SETUP must not retry");
    assert!(
        srv.requests.len() == 1 && has_prefix(&srv.requests[0], "SETUP "),
        "only the failed SETUP must go out"
    );
    assert_eq!(fx.d.client.state, State::Inactive, "client must park");
    assert!(fx.d.client.err, "403 must flag the adapter");
}

// Ported from tests/test_satipc_rtsp.cpp:279
#[test]
fn test_rtsp_500_setup_gives_up() {
    let _serial = serial();
    let mut fx = RtspFixture::setup();
    let mut srv = FakeSatipServer::new(&[("SETUP ", SETUP_500)]);
    fx.d.client.tune();
    add_pid(&mut fx, 0);
    fx.d.client.commit();
    assert_eq!(fx.run(&mut srv), 1, "500 on SETUP must not retry");
    assert_eq!(fx.d.client.state, State::Inactive, "client must park");
    assert!(fx.d.client.err, "500 must flag the adapter");
}

// Ported from tests/test_satipc_rtsp.cpp:299
#[test]
fn test_rtsp_403_play_tears_down() {
    let _serial = serial();
    let (mut fx, mut srv) = tuned_fixture(&[
        ("SETUP ", SETUP_OK),
        (
            "PLAY rtsp://192.168.1.43:554/stream=308?addpids=5167",
            PLAY_403,
        ),
        ("PLAY ", PLAY_OK),
        ("TEARDOWN ", TEARDOWN_OK),
        ("DESCRIBE ", DESCRIBE_OK),
    ]);
    let base = srv.requests.len();
    add_pid(&mut fx, 5167);
    add_pid(&mut fx, 5171);
    fx.d.client.commit();
    assert_eq!(fx.run(&mut srv), 2, "403 must tear down once and stop");
    assert!(
        has_prefix(&srv.requests[base], "PLAY ")
            && has_prefix(&srv.requests[base + 1], "TEARDOWN "),
        "403 must end the session without re-setup"
    );
    assert_eq!(fx.d.client.state, State::Inactive, "client must park");
    assert!(fx.d.client.err, "403 must flag the adapter");
}

// Ported from tests/test_satipc_rtsp.cpp:335
#[test]
fn test_rtsp_503_no_more_pids_gives_up() {
    let _serial = serial();
    let (mut fx, mut srv) = tuned_fixture(&[
        ("SETUP ", SETUP_OK),
        (
            "PLAY rtsp://192.168.1.43:554/stream=308?addpids=5167",
            PLAY_503_PIDS,
        ),
        ("PLAY ", PLAY_OK),
        ("TEARDOWN ", TEARDOWN_OK),
        ("DESCRIBE ", DESCRIBE_OK),
    ]);
    let base = srv.requests.len();
    add_pid(&mut fx, 5167);
    add_pid(&mut fx, 5171);
    fx.d.client.commit();
    assert_eq!(
        fx.run(&mut srv),
        2,
        "No-More: pids must tear down once and stop"
    );
    assert!(
        has_prefix(&srv.requests[base], "PLAY ")
            && has_prefix(&srv.requests[base + 1], "TEARDOWN "),
        "exhausted pids must end the session without re-setup"
    );
    assert_eq!(fx.d.client.state, State::Inactive, "client must park");
    assert!(fx.d.client.err, "exhausted pids must flag the adapter");
}

// Ported from tests/test_satipc_rtsp.cpp:371
#[test]
fn test_rtsp_503_no_more_frontends_recovers() {
    let _serial = serial();
    let (mut fx, mut srv) = tuned_fixture(&[
        ("SETUP ", SETUP_OK),
        (
            "PLAY rtsp://192.168.1.43:554/stream=308?addpids=5167",
            PLAY_503_FRONTENDS,
        ),
        ("PLAY ", PLAY_OK),
        ("TEARDOWN ", TEARDOWN_OK),
        ("DESCRIBE ", DESCRIBE_OK),
    ]);
    let base = srv.requests.len();
    add_pid(&mut fx, 5167);
    add_pid(&mut fx, 5171);
    fx.d.client.commit();
    assert_eq!(
        fx.run(&mut srv),
        4,
        "busy frontends must cycle PLAY, TEARDOWN, SETUP, PLAY"
    );
    assert!(
        has_prefix(&srv.requests[base], "PLAY ")
            && has_prefix(&srv.requests[base + 1], "TEARDOWN ")
            && has_prefix(&srv.requests[base + 2], "SETUP ")
            && has_prefix(&srv.requests[base + 3], "PLAY "),
        "busy frontends must tear down and re-establish the session"
    );
    assert_eq!(fx.d.client.state, State::Play, "client must end up playing");
    assert!(!fx.d.client.err, "recovery must not flag the adapter");
}

// Ported from tests/test_satipc_rtsp.cpp:409
#[test]
fn test_rtsp_454_recovery() {
    let _serial = serial();
    let (mut fx, mut srv) = tuned_fixture(&[
        ("SETUP ", SETUP_OK),
        (
            "PLAY rtsp://192.168.1.43:554/stream=308?addpids=18",
            PLAY_454,
        ),
        ("PLAY ", PLAY_OK),
        ("TEARDOWN ", TEARDOWN_OK),
        ("DESCRIBE ", DESCRIBE_OK),
    ]);
    let base = srv.requests.len();
    add_pid(&mut fx, 18);
    fx.d.client.commit();
    assert_eq!(fx.run(&mut srv), 3, "454 must cycle PLAY, SETUP, PLAY");
    assert!(
        has_prefix(&srv.requests[base], "PLAY ")
            && has_prefix(&srv.requests[base + 1], "SETUP ")
            && has_prefix(&srv.requests[base + 2], "PLAY "),
        "454 must restart the session without TEARDOWN"
    );
    assert!(
        has_part(&srv.requests[base + 2], "pids=0,18"),
        "recovery PLAY must resend the full pid list"
    );
    assert!(!fx.d.client.force_pids, "full pid list goes out once");
    assert_eq!(fx.d.client.state, State::Play, "client must end up playing");
}

// Ported from tests/test_satipc_rtsp.cpp:447
#[test]
fn test_rtsp_keepalive() {
    let _serial = serial();
    let mut fx = RtspFixture::setup();
    let mut srv = FakeSatipServer::new(&[("OPTIONS ", OPTIONS_OK), ("DESCRIBE ", DESCRIBE_OK)]);
    fx.d.client.state = State::Play;
    fx.d.client.stream_id = 308;
    fx.d.client.session = "EDCD7C81".to_owned();
    fx.d.client.timeout();
    assert_eq!(
        fx.run(&mut srv),
        2,
        "idle timeout must send OPTIONS then DESCRIBE"
    );
    assert!(
        has_prefix(&srv.requests[0], "OPTIONS ") && has_part(&srv.requests[0], "Session: EDCD7C81"),
        "OPTIONS must carry the session"
    );
    assert!(
        has_prefix(&srv.requests[1], "DESCRIBE "),
        "client must ask for signal while unknown"
    );
    assert_eq!(fx.run(&mut srv), 0, "OPTIONS reply must stay quiet");

    fx.d.client.want_tune = true;
    fx.d.client.timeout();
    assert_eq!(
        fx.run(&mut srv),
        0,
        "timeout with queued tune must not send OPTIONS"
    );
}

// Binds an ephemeral localhost UDP port and releases it, so the SRT
// connect below fails fast against a closed port.
#[cfg(feature = "srt")]
fn closed_udp_port() -> u16 {
    UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .and_then(|socket| socket.local_addr())
        .map_or(9, |address| address.port())
}

// Ported from tests/test_satipc_rtsp.cpp:500
#[cfg(feature = "srt")]
#[test]
fn test_rtsp_srt_failure_keeps_socket() {
    let _serial = serial();
    let mut fx = RtspFixture::setup();

    fx.d.client.transport = Transport::Srt;
    fx.d.srt_sock = SRT_INVALID_SOCK;
    fx.d.client.host = "127.0.0.1".to_owned();
    fx.d.client.port = closed_udp_port();

    // The dispatch deletes the socket on nonzero timeout returns.
    assert_eq!(
        fx.d.timeout(),
        0,
        "SRT failure must not delete the RTSP socket"
    );
    // The SRT restart branch ran and released its claim.
    assert!(!fx.d.client.srt_restart_claimed);
    assert_eq!(fx.d.srt_sock, SRT_INVALID_SOCK);
    assert!(fx.d.client.rtsp_connected);
}

// Binds an ephemeral localhost TCP port and releases it, so a connect
// fails fast with ECONNREFUSED.
fn closed_tcp_port() -> u16 {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .and_then(|listener| listener.local_addr())
        .map_or(9, |address| address.port())
}

// Ported from tests/test_satipc_rtsp.cpp:540
// A failed RTSP reopen must stay flagged so the next timeout retries.
#[test]
fn test_rtsp_timeout_reopen_retries() {
    let _serial = serial();
    let mut fx = RtspFixture::setup();
    fx.d.client.host = "127.0.0.1".to_owned();
    fx.d.client.port = closed_tcp_port();
    fx.d.client.rtsp_socket_closed = true;

    assert_eq!(fx.d.timeout(), 0, "failed reopen must return 0");
    assert!(
        fx.d.client.rtsp_socket_closed,
        "failed reopen must stay flagged"
    );
    assert_eq!(fx.d.timeout(), 0, "retry must return 0");
}

// Ported from tests/test_satipc_rtsp.cpp:559
// A server silent past timeout_ms must flag a restart and drop the session.
#[test]
fn test_rtsp_timeout_restart_on_silent_server() {
    let _serial = serial();
    let mut fx = RtspFixture::setup();
    fx.d.client.expect_reply = true;
    fx.d.client.last_response_sent = fx.d.client.tick() - 60_000;
    fx.d.client.timeout_ms = 30_000;

    assert_eq!(fx.d.timeout(), 0, "restart timeout must return 0");
    assert!(
        fx.d.client.restart_needed,
        "silent server must flag restart"
    );
    assert_eq!(
        fx.d.client.state,
        State::DISCONNECTED,
        "silent server must disconnect"
    );
}

fn listen_on_loopback() -> (TcpListener, u16) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("listen");
    let port = listener.local_addr().expect("addr").port();
    (listener, port)
}

/// Asserts that the exact descriptor `fd` is closed, so a leaked socket is
/// detected without counting every descriptor of the process (other test
/// threads open and close sockets concurrently).
#[allow(unsafe_code)]
fn assert_fd_closed(fd: std::os::fd::RawFd, what: &str) {
    // SAFETY: F_GETFD reads descriptor flags and takes no pointer argument.
    let result = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    assert_eq!(result, -1, "{what}: fd {fd} must be closed");
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::EBADF),
        "{what}: fd {fd} must be closed"
    );
}

/// The peer of a closed socket reads EOF; a leaked descriptor keeps it open
/// until the read timeout expires.
fn assert_peer_eof(peer: &mut TcpStream, what: &str) {
    peer.set_nonblocking(false).expect("blocking");
    peer.set_read_timeout(Some(Duration::from_secs(2)))
        .expect("timeout");
    let mut byte = [0u8; 1];
    assert_eq!(peer.read(&mut byte).expect("read"), 0, "{what}");
}

// Occupies the RTP port without reuse so the setup bind fails.
fn block_udp(port: u16) -> (UdpSocket, Option<UdpSocket>) {
    let block4 = UdpSocket::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)))
        .unwrap_or_else(|error| panic!("cannot occupy UDP port {port}: {error}"));
    let block6 = UdpSocket::bind(SocketAddr::from((Ipv6Addr::UNSPECIFIED, port))).ok();
    (block4, block6)
}

fn assert_nothing_tracked(d: &Driver) {
    assert!(
        d.rtsp.is_none() && d.rtp.is_none() && d.rtcp.is_none(),
        "failed open must not leak tracking"
    );
}

#[allow(unsafe_code)]
fn receive_buffer(fd: std::os::fd::RawFd) -> i32 {
    let mut value: libc::c_int = 0;
    let mut length = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
    // SAFETY: value and length describe the SO_RCVBUF output buffer.
    let result = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_RCVBUF,
            (&raw mut value).cast(),
            &mut length,
        )
    };
    assert_eq!(result, 0, "getsockopt(SO_RCVBUF)");
    value
}

// src_satipc.cpp:605: with both multipliers zero the server signal is ignored;
// otherwise the reported strength/quality are used.
#[test]
fn test_satipc_per_adapter_multipliers_select_signal() {
    let _serial = serial();
    let mut ignoring = config(0, "127.0.0.1", 554, 5500);
    ignoring.ignore_signal = true;
    let mut d = Driver::new(&ignoring);
    d.client
        .set_adapter_signal("tuner=1,106,1,0,538.00,8,dvbc,256qam,6900");
    assert_eq!(d.client.strength, MAX_STRENGTH, "signal must be forced");
    assert_eq!(d.client.snr, MAX_QUALITY, "quality must be forced");

    let mut honouring = config(0, "127.0.0.1", 554, 5500);
    honouring.ignore_signal = false;
    let mut d = Driver::new(&honouring);
    d.client
        .set_adapter_signal("tuner=1,106,1,0,538.00,8,dvbc,256qam,6900");
    assert_eq!(d.client.strength, 106, "server strength must be kept");
    assert_eq!(d.client.snr, 7, "AVM quality workaround must apply");
    assert!(d.client.status, "lock must be reported");
}

// src_satipc.cpp:395-400: the per-server source is passed to tcp_connect_src.
#[test]
fn test_satipc_source_ip_binds_rtsp_socket() {
    let _serial = serial();
    let (listener, port) = listen_on_loopback();
    let mut cfg = config(0, "127.0.0.1", port, 5500);
    cfg.server.source_ip = "127.0.0.2".to_owned();
    let mut d = Driver::new(&cfg);
    d.open_rtsp_socket(true).expect("connect");
    let (peer, _) = listener.accept().expect("accept");
    assert_eq!(
        peer.peer_addr().expect("peer").ip(),
        Ipv4Addr::new(127, 0, 0, 2),
        "RTSP connection must use the per-server source"
    );
}

// src_satipc.cpp:400: without a per-server source, bind_dev_ip() is used.
#[test]
fn test_satipc_bind_dev_ip_binds_rtsp_socket() {
    let _serial = serial();
    let (listener, port) = listen_on_loopback();
    let mut cfg = config(0, "127.0.0.1", port, 5500);
    cfg.bind_dev_ip = Some(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2)));
    let mut d = Driver::new(&cfg);
    d.open_rtsp_socket(true).expect("connect");
    let (peer, _) = listener.accept().expect("accept");
    assert_eq!(
        peer.peer_addr().expect("peer").ip(),
        Ipv4Addr::new(127, 0, 0, 2),
        "RTSP connection must use bind_dev_ip()"
    );
}

// socketworks.cpp:427-445: a wrong-family source is ignored, not fatal.
#[test]
fn test_satipc_wrong_family_source_is_ignored() {
    let _serial = serial();
    let (listener, port) = listen_on_loopback();
    let mut cfg = config(0, "127.0.0.1", port, 5500);
    cfg.server.source_ip = "::1".to_owned();
    let mut d = Driver::new(&cfg);
    d.open_rtsp_socket(true)
        .expect("connect without the source");
    let (peer, _) = listener.accept().expect("accept");
    assert_eq!(
        peer.peer_addr().expect("peer").ip(),
        Ipv4Addr::LOCALHOST,
        "wrong-family source must be skipped"
    );
}

// src_satipc.cpp:816-822: udp_bind(bind_dev_ip(), listen_udp).
#[test]
fn test_satipc_bind_dev_ip_binds_rtp_sockets() {
    let _serial = serial();
    let aid = 0;
    for _ in 0..50 {
        let probe = UdpSocket::bind((Ipv4Addr::new(127, 0, 0, 2), 0)).expect("probe");
        let port = probe.local_addr().expect("addr").port();
        drop(probe);
        let Some(start_rtp) = port.checked_sub(1000) else {
            continue;
        };
        let mut cfg = config(aid, "127.0.0.1", 554, start_rtp);
        cfg.bind_dev_ip = Some(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2)));
        let mut d = Driver::new(&cfg);
        if d.setup_rtp_udp_sockets().is_err() {
            continue;
        }
        let rtp = d.rtp.as_ref().expect("rtp");
        let rtcp = d.rtcp.as_ref().expect("rtcp");
        assert_eq!(
            rtp.local_addr().expect("rtp addr").ip(),
            Ipv4Addr::new(127, 0, 0, 2)
        );
        assert_eq!(rtp.local_addr().expect("rtp addr").port(), port);
        assert_eq!(
            rtcp.local_addr().expect("rtcp addr").ip(),
            Ipv4Addr::new(127, 0, 0, 2)
        );
        assert_eq!(rtcp.local_addr().expect("rtcp addr").port(), port + 1);
        return;
    }
    panic!("could not bind RTP sockets on the loopback alias");
}

// src_satipc.cpp:410-411, 842-843: opts.satipc_buffer becomes SO_RCVBUF on the
// TCP media socket and the RTP socket only.
#[test]
fn test_satipc_receive_buffer_applied() {
    let _serial = serial();
    let aid = 0;
    let requested = 1024;
    for _ in 0..50 {
        let probe = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("probe");
        let port = probe.local_addr().expect("addr").port();
        drop(probe);
        let Some(start_rtp) = port.checked_sub(1000) else {
            continue;
        };
        let mut cfg = config(aid, "127.0.0.1", 554, start_rtp);
        cfg.receive_buffer = requested;
        let mut d = Driver::new(&cfg);
        if d.setup_rtp_udp_sockets().is_err() {
            continue;
        }
        let rtp = receive_buffer(d.rtp.as_ref().expect("rtp").as_raw_fd());
        let rtcp = receive_buffer(d.rtcp.as_ref().expect("rtcp").as_raw_fd());
        assert!(
            (requested..=8192).contains(&rtp),
            "RTP SO_RCVBUF {rtp} must reflect {requested}"
        );
        assert!(rtcp > rtp, "RTCP must keep the default buffer ({rtcp})");
        return;
    }
    panic!("could not bind RTP sockets");
}

// src_satipc.cpp:410-411: the TCP media socket is the RTSP socket.
#[test]
fn test_satipc_receive_buffer_applied_to_tcp_media() {
    let _serial = serial();
    let (listener, port) = listen_on_loopback();
    let mut cfg = config(0, "127.0.0.1", port, 5500);
    cfg.server.transport = Transport::Tcp;
    cfg.receive_buffer = 1024;
    let mut d = Driver::new(&cfg);
    d.open_rtsp_socket(true).expect("connect");
    let _peer = listener.accept().expect("accept");
    let value = receive_buffer(d.rtsp.as_ref().expect("rtsp").as_raw_fd());
    assert!(
        (1024..=8192).contains(&value),
        "TCP SO_RCVBUF {value} must reflect --satip-receive-buffer"
    );
}

// Ported from tests/test_satipc_rtsp.cpp:579
// A failed RTP setup must release the RTSP socket opened just before.
#[test]
fn test_satipc_open_device_cleans_rtsp_on_rtp_failure() {
    let _serial = serial();
    let aid = 3;
    let (srv, port) = listen_on_loopback();
    // accept() runs after open_device, so a missing connection must fail the
    // test rather than block forever.
    srv.set_nonblocking(true).expect("nonblocking");
    let start_rtp = 5500;
    let listen_udp = start_rtp + 1000 + aid as u16 * 2;
    let _block = block_udp(listen_udp);
    let mut d = Driver::new(&config(aid, "127.0.0.1", port, start_rtp));

    let rv = d.open_device();
    assert_ne!(rv, 0, "RTP setup must fail on the occupied port");
    assert!(
        d.rtsp.is_none() && !d.client.rtsp_connected,
        "failed open must drop the RTSP socket"
    );
    // The RTSP fd opened for the failed attempt is released: its peer sees EOF.
    let (mut peer, _) = srv.accept().expect("accept");
    assert_peer_eof(&mut peer, "failed open must not leak fds");
    assert_nothing_tracked(&d);
}

// Ported from tests/test_satipc_rtsp.cpp:692
// Only the assertions with a Rust counterpart: there is no socket table, so
// the id-mismatch and untracked-fd cases have nothing to race against.
#[test]
fn test_satipc_drop_raced_socket() {
    let _serial = serial();
    let (srv, port) = listen_on_loopback();
    let mut d = Driver::new(&config(8, "127.0.0.1", port, 5500));
    d.open_rtsp_socket(true).expect("connect");
    let (mut peer, _) = srv.accept().expect("accept");
    d.client.rtsp_socket_closed = false;
    // tests/test_satipc_rtsp.cpp:710: the expected fd must be closed.
    let rtsp_fd = d.rtsp.as_ref().expect("rtsp").as_raw_fd();
    d.drop_rtsp_socket();
    assert!(
        d.rtsp.is_none() && !d.client.rtsp_connected,
        "fields must reset"
    );
    assert!(d.client.rtsp_socket_closed, "flag must return");
    assert_fd_closed(rtsp_fd, "exactly the RTSP fd closes");
    assert_peer_eof(&mut peer, "fd must be closed");

    // TCP: the media socket is the RTSP socket and is released as well.
    let mut d = Driver::new(&config(8, "127.0.0.1", port, 5500));
    d.client.transport = Transport::Tcp;
    d.open_rtsp_socket(true).expect("connect");
    let (mut peer, _) = srv.accept().expect("accept");
    assert!(d.client.data_open);
    // tests/test_satipc_rtsp.cpp:748: the TCP dvr fd must close.
    let dvr_fd = d.rtsp.as_ref().expect("rtsp").as_raw_fd();
    d.drop_rtsp_socket();
    assert!(!d.client.data_open, "TCP dvr must reset");
    assert_fd_closed(dvr_fd, "the TCP media fd must be released");
    assert_peer_eof(&mut peer, "TCP dvr fd must close");
}

// Ported from tests/test_satipc_rtsp.cpp:757
// Abort releases exactly the raced publish: matching state is torn
// down, anything else (or nobody) is left untouched.
#[cfg(feature = "srt")]
#[test]
fn test_satipc_abort_srt() {
    let _serial = serial();
    let mut d = Driver::new(&config(9, "127.0.0.1", 554, 5500));
    assert!(ms_srt::startup());
    let s = ms_srt::create_socket();
    assert_ne!(s, SRT_INVALID_SOCK, "srt_create_socket");
    // ad.sock = sockets_add(pipe, TYPE_DVR): the media entry exists.
    d.client.data_open = true;
    d.srt_sock = s;
    d.client.srt_streamid = "abort-me".to_owned();
    // sip.udp_sock and ad.dvr are one descriptor in Rust.
    d.srt_udp = 4001;
    d.abort_srt(s);
    assert_eq!(d.srt_sock, SRT_INVALID_SOCK, "srt must invalidate");
    assert!(d.client.srt_streamid.is_empty(), "streamid must clear");
    assert_eq!(d.srt_udp, -1, "udp/dvr must reset");
    // SOCK_TIMEOUT: the media entry stays, now without a descriptor.
    assert!(d.client.data_open && d.srt_udp == -1, "handle must swap");
    assert!(!ms_srt::is_connected(s), "closed srt must read down");
    let s2 = ms_srt::create_socket();
    d.srt_sock = s2;
    d.client.srt_streamid = "keep-me".to_owned();
    d.abort_srt(SRT_INVALID_SOCK);
    assert!(
        d.srt_sock == s2 && d.client.srt_streamid == "keep-me",
        "invalid fresh must noop"
    );
    d.abort_srt(s);
    assert_eq!(d.srt_sock, s2, "stale fresh must noop");
    ms_srt::close(s2);
    drop(d);
    assert!(ms_srt::cleanup());
}

/// The SRT transport end to end: `satipc_open_srt` connects to the server's
/// RTSP port with a numeric stream id, SETUP advertises that id, and
/// `satipc_srt_read` delivers the TS the server sends.
#[cfg(feature = "srt")]
#[test]
fn satipc_srt_connects_with_streamid_and_receives_ts() {
    use std::sync::Arc;
    use std::thread;
    use std::time::Instant;

    use ms_tuner::TsBuf;
    use ms_tuner::satipc::Op;

    let _serial = serial();
    assert!(ms_srt::startup());
    let udp = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("udp bind");
    let port = udp.local_addr().expect("local addr").port();
    let listener = ms_srt::create_socket();
    assert_ne!(listener, SRT_INVALID_SOCK, "listener socket");
    assert!(ms_srt::bind_acquire(
        listener,
        std::os::fd::IntoRawFd::into_raw_fd(udp)
    ));
    let seen = Arc::new(Mutex::new(None::<String>));
    let record = Arc::clone(&seen);
    assert!(ms_srt::set_listen_callback(
        listener,
        Arc::new(move |streamid: Option<&str>, _sock| {
            *record.lock().expect("streamid") = streamid.map(str::to_owned);
            true
        }),
    ));
    assert!(ms_srt::listen(listener, 1));
    let acceptor = thread::spawn(move || ms_srt::accept(listener));

    let mut cfg = config(0, "127.0.0.1", port, 5500);
    cfg.server.transport = Transport::Srt;
    let mut d = Driver::new(&cfg);
    assert_eq!(d.open_srt(), 0, "SRT connect must succeed");
    assert!(d.client.srt_connected);
    assert!(d.srt_udp >= 0, "SRT owns the UDP descriptor");
    let streamid = d.client.srt_streamid.clone();
    assert!(
        streamid.parse::<u32>().is_ok(),
        "numeric streamid: {streamid}"
    );
    let server = acceptor.join().expect("accept thread");
    assert_ne!(server, SRT_INVALID_SOCK, "server must accept");
    assert_eq!(seen.lock().expect("streamid").as_deref(), Some(&*streamid));

    // A second open reuses the connected socket and keeps the stream id.
    let sock = d.srt_sock;
    assert_eq!(d.open_srt(), 0);
    assert_eq!(d.srt_sock, sock);
    assert_eq!(d.client.srt_streamid, streamid);

    d.client.enabled = true;
    d.client.rtsp_connected = true;
    d.client.http_request(Some("src=1"), "SETUP");
    let setup = d
        .client
        .ops
        .iter()
        .find_map(|op| match op {
            Op::Send(data) => Some(String::from_utf8_lossy(data).into_owned()),
            _ => None,
        })
        .expect("SETUP queued");
    assert!(
        setup.contains(&format!(
            "\r\nTransport: RTP/AVP/SRT;srt_streamid={streamid}\r\n"
        )),
        "{setup}"
    );
    d.client.ops.clear();

    let mut ts = Vec::new();
    for index in 0..7u8 {
        let mut packet = [0xffu8; 188];
        packet[..4].copy_from_slice(&[0x47, 0, 0x11, 0x10 | index]);
        ts.extend_from_slice(&packet);
    }
    // Live mode delivers after the 1 s latency; keep the stream flowing until
    // the first messages arrive.
    // satipc_open_device: the media entry is open.
    d.client.data_open = true;
    let mut buf = TsBuf::with_packets(64);
    let deadline = Instant::now() + Duration::from_secs(5);
    while buf.is_empty() && Instant::now() < deadline {
        assert_eq!(ms_srt::send(server, &ts), ts.len() as i32);
        d.read_media(&mut buf);
        thread::sleep(Duration::from_millis(50));
    }
    assert!(!buf.is_empty(), "no TS received over SRT");
    assert_eq!(buf.len() % ts.len(), 0, "whole SRT messages only");
    for message in buf.as_bytes().chunks(ts.len()) {
        assert_eq!(message, &ts[..]);
    }

    // Without the media entry nothing reads, though the socket stays up.
    d.client.data_open = false;
    let deadline = Instant::now() + Duration::from_millis(1500);
    let mut idle = TsBuf::with_packets(64);
    while Instant::now() < deadline {
        assert_eq!(ms_srt::send(server, &ts), ts.len() as i32);
        assert_eq!(
            d.read_media(&mut idle),
            0,
            "closed media entry must not read"
        );
        thread::sleep(Duration::from_millis(50));
    }
    assert!(
        ms_srt::is_connected(d.srt_sock),
        "SRT socket is kept for reuse"
    );

    // A RestartSrt dropped with the queue must release its claim.
    d.device_open = true;
    d.client.srt_restart_claimed = true;
    d.close_device();
    assert!(
        !d.client.srt_restart_claimed,
        "close must release the claim"
    );

    ms_srt::close(server);
    ms_srt::close(listener);
    drop(d);
    assert!(ms_srt::cleanup());
}

// Ported from tests/test_satipc_rtsp.cpp:800
// A reopen on a live adapter stays published.
#[test]
fn test_rtsp_timeout_reopen_keeps_live_adapter() {
    let _serial = serial();
    let mut fx = RtspFixture::setup();
    let (_srv, port) = listen_on_loopback();
    fx.d.client.host = "127.0.0.1".to_owned();
    fx.d.client.port = port;
    fx.d.client.rtsp_socket_closed = true;
    assert_eq!(fx.d.timeout(), 0, "reopen must return 0");
    assert!(
        !fx.d.client.rtsp_socket_closed,
        "live reopen must stay open"
    );
    assert!(fx.d.rtsp.is_some(), "live reopen must track the socket");
    assert!(
        fx.d.client.rtsp_connected,
        "reopened socket must be tracked"
    );
}

// Ported from tests/test_satipc_rtsp.cpp:829
// The socket-table-full precondition has no Rust counterpart; the fd-leak
// assertions of the failed open are kept.
#[test]
fn test_satipc_open_device_full_table_closes_untracked() {
    let _serial = serial();
    let aid = 4;
    let (srv, port) = listen_on_loopback();
    // accept() runs after open_device, so a missing connection must fail the
    // test rather than block forever.
    srv.set_nonblocking(true).expect("nonblocking");
    let start_rtp = 5500;
    let listen_udp = start_rtp + 1000 + aid as u16 * 2;
    let block4 = UdpSocket::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, listen_udp)))
        .unwrap_or_else(|error| panic!("cannot occupy UDP port {listen_udp}: {error}"));
    let mut d = Driver::new(&config(aid, "127.0.0.1", port, start_rtp));

    let rv = d.open_device();
    assert_ne!(rv, 0, "RTP setup must fail on the occupied port");
    assert!(d.rtsp.is_none(), "untracked fd must be released");
    // The RTSP fd opened for the failed attempt is released: its peer sees EOF.
    let (mut peer, _) = srv.accept().expect("accept");
    assert_peer_eof(&mut peer, "no fd may leak");
    assert_nothing_tracked(&d);
    drop(block4);
}
