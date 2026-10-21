//! Ports of tests/test_satipc.cpp.

use ms_tuner::satipc::{Client, ReplyOutcome, State, parse_satip_xml};
use ms_tuner::{DeliverySystem, Polarization};

const QPSK: u32 = 0;
const FEC_2_3: u8 = 2;
const ROLLOFF_35: u8 = 0;
const PLS_MODE_ROOT: i32 = 0;
const PLS_MODE_GOLD: i32 = 1;

// The common transponder of the get_s2_url tests.
fn s2_client() -> Client {
    let mut sip = Client::new(0, "h", 554);
    sip.enabled = true;
    sip.satip_fe = 0;
    sip.sys0 = DeliverySystem::DvbS2;
    sip.tp.src = 1;
    sip.tp.src_specified = true;
    sip.tp.freq = 11_362_000;
    sip.tp.msys = DeliverySystem::DvbS2;
    sip.tp.mtype = QPSK;
    sip.tp.mtype_specified = true;
    sip.tp.pol = Polarization::Horizontal;
    sip.tp.sr = 22_000_000;
    sip.tp.fec = FEC_2_3;
    sip.tp.fec_specified = true;
    sip.tp.ro = Some(ROLLOFF_35);
    sip.tp.plts = Some(false);
    sip
}

// Ported from tests/test_satipc.cpp:41
#[test]
fn test_get_s2_url_multistream_isi() {
    let mut sip = s2_client();
    sip.tp.plp = Some(3);
    let url = sip.s2_url();
    assert!(
        url.contains("&isi=3"),
        "ISI parameter missing from proxy URL"
    );
}

// Ported from tests/test_satipc.cpp:72
#[test]
fn test_get_s2_url_pls_mode_root() {
    let mut sip = s2_client();
    sip.tp.plp = Some(0);
    sip.tp.plsm = Some(PLS_MODE_ROOT);
    sip.tp.plsc = Some(42);
    let url = sip.s2_url();
    assert!(
        url.contains("&plsm=root"),
        "PLS mode ROOT missing from proxy URL"
    );
    assert!(
        url.contains("&plsc=42"),
        "PLS code missing from proxy URL when pls_mode is ROOT"
    );
}

// Ported from tests/test_satipc.cpp:107
#[test]
fn test_get_s2_url_pls_mode_gold() {
    let mut sip = s2_client();
    sip.tp.plp = Some(5);
    sip.tp.plsm = Some(PLS_MODE_GOLD);
    sip.tp.plsc = Some(100);
    let url = sip.s2_url();
    assert!(
        url.contains("&isi=5"),
        "ISI parameter missing from proxy URL"
    );
    assert!(
        url.contains("&plsm=gold"),
        "PLS mode GOLD missing from proxy URL"
    );
    assert!(
        url.contains("&plsc=100"),
        "PLS code missing from proxy URL when pls_mode is GOLD"
    );
}

// Ported from tests/test_satipc.cpp:144
#[test]
fn test_get_s2_url_no_multistream() {
    let sip = s2_client();
    let url = sip.s2_url();
    assert!(!url.contains("isi"), "ISI should not appear when unset");
    assert!(
        !url.contains("plsm"),
        "PLS mode should not appear when unset"
    );
    assert!(
        !url.contains("plsc"),
        "PLS code should not appear when unset"
    );
}

// Ported from tests/test_satipc.cpp:178
#[test]
fn test_satip_getxml_data_parsing() {
    let xml = "<satip:X_SATIPCAP>DVBS2-4,DVBT-2</satip:X_SATIPCAP>";
    let s = parse_satip_xml(xml, "1.2.3.4").expect("satipcap");
    assert_eq!(
        s.tuners_for(DeliverySystem::DvbS2),
        4,
        "Expected 4 tuners for DVBS2"
    );
    assert_eq!(
        s.tuners_for(DeliverySystem::DvbT),
        2,
        "Expected 2 tuners for DVBT"
    );
}

// Ported from tests/test_satipc.cpp:194
#[test]
fn test_satipc_get_pids_add_and_del() {
    // no adapter registered, get_adapter_pids returns ""
    let adapter_pids = "";

    // combined add + delete must separate the parameters with '&'
    let mut sip_add_del = Client::new(0, "h", 554);
    sip_add_del.apid = vec![202];
    sip_add_del.dpid = vec![201, 213];
    let url = sip_add_del.get_pids(adapter_pids, false);
    assert_eq!(
        url, "addpids=202&delpids=201,213",
        "addpids and delpids parameters must be separated by '&'"
    );
    assert!(
        sip_add_del.apid.is_empty() && sip_add_del.dpid.is_empty(),
        "pending add/del pids must be cleared after building the URL"
    );

    // add-only must not gain a separator
    let mut sip_add = Client::new(0, "h", 554);
    sip_add.apid = vec![2211, 2212];
    let url = sip_add.get_pids(adapter_pids, false);
    assert_eq!(
        url, "addpids=2211,2212",
        "add-only URL must be plain addpids="
    );

    // del-only must not gain a leading separator
    let mut sip_del = Client::new(0, "h", 554);
    sip_del.dpid = vec![18];
    let url = sip_del.get_pids(adapter_pids, false);
    assert_eq!(url, "delpids=18", "del-only URL must be plain delpids=");
}

// Ported from tests/test_satipc.cpp:235
#[test]
fn test_satipc_reply_empty_buffer_ignored() {
    let mut sip = Client::new(0, "h", 554);
    sip.enabled = true;
    sip.rtsp_connected = true;
    let before = sip.clone();
    assert_eq!(sip.reply(&[]), ReplyOutcome::Empty);
    assert!(
        sip.ops.is_empty(),
        "empty buffer must not trigger a request"
    );
    assert_eq!(
        sip.state, before.state,
        "empty buffer must leave the client alone"
    );
    assert_eq!(sip.cseq, before.cseq);
    assert_eq!(sip.expect_reply, before.expect_reply);
}

fn queue_has(q: &[u16], pid: u16) -> bool {
    q.contains(&pid)
}

// Ported from tests/test_satipc.cpp:281
#[test]
fn test_satipc_cancel_keeps_queued_pids() {
    let mut sip_add = Client::new(0, "h", 554);
    sip_add.enabled = true;
    sip_add.err = false;
    sip_add.set_pid(100);
    sip_add.set_pid(200);
    sip_add.set_pid(300);
    sip_add.del_filters(100, 100);
    assert_eq!(sip_add.apid.len(), 2, "cancel shrinks the add queue by one");
    assert!(queue_has(&sip_add.apid, 200), "keeps pid 200");
    assert!(queue_has(&sip_add.apid, 300), "keeps pid 300");

    let mut sip_del = Client::new(0, "h", 554);
    sip_del.enabled = true;
    sip_del.del_filters(100, 400);
    sip_del.del_filters(100, 500);
    sip_del.del_filters(100, 600);
    sip_del.set_pid(400);
    assert_eq!(sip_del.dpid.len(), 2, "cancel shrinks the del queue by one");
    assert!(queue_has(&sip_del.dpid, 500), "keeps pid 500");
    assert!(queue_has(&sip_del.dpid, 600), "keeps pid 600");
}

fn send_rtsp_reply(sip: &mut Client, reply: &str) {
    sip.reply(reply.as_bytes());
}

// Ported from tests/test_satipc.cpp:313
#[test]
fn test_satipc_reject_teardown_then_retry() {
    let mut sip = Client::new(0, "h", 554);
    sip.sys0 = DeliverySystem::Undefined;
    sip.tp.freq = 11_362_000;
    sip.sid_cnt = 1;
    sip.data_open = false;
    sip.rtsp_connected = false;
    sip.enabled = true;
    sip.state = State::Play;
    sip.stream_id = 308;
    sip.session = "EDCD7C81".to_owned();

    // 503 to PLAY releases the session, then retries like 454
    send_rtsp_reply(
        &mut sip,
        "RTSP/1.0 503 Service Unavailable\r\nCSeq: 110\r\n\r\n",
    );
    assert!(!sip.err, "503 must not flag the adapter in error");
    assert_eq!(sip.state, State::Teardown, "503 must move to TEARDOWN");
    assert_eq!(sip.stream_id, -1, "TEARDOWN must drop the stream");

    // The TEARDOWN reply restarts the session with a fresh SETUP
    sip.ignore_packets = false;
    send_rtsp_reply(&mut sip, "RTSP/1.0 200 OK\r\nCSeq: 111\r\n\r\n");
    assert_eq!(
        sip.state,
        State::Setup,
        "TEARDOWN reply must restart the session"
    );
    assert!(sip.ignore_packets, "a fresh SETUP must follow the teardown");

    // 503 with no session retries the SETUP at once
    sip.stream_id = -1;
    sip.state = State::Setup;
    sip.ignore_packets = false;
    send_rtsp_reply(
        &mut sip,
        "RTSP/1.0 503 Service Unavailable\r\nCSeq: 112\r\n\r\n",
    );
    assert_eq!(
        sip.state,
        State::Setup,
        "503 without session must retry SETUP"
    );
    assert!(sip.ignore_packets, "SETUP must go out right after the 503");

    // 503 while in error stays parked
    sip.err = true;
    sip.state = State::Setup;
    sip.ignore_packets = false;
    send_rtsp_reply(
        &mut sip,
        "RTSP/1.0 503 Service Unavailable\r\nCSeq: 113\r\n\r\n",
    );
    assert_eq!(
        sip.state,
        State::Inactive,
        "503 must not retry while in error"
    );
    assert!(!sip.ignore_packets, "no SETUP may follow while in error");

    // TEARDOWN completing while in error must not restart the session
    sip.err = false;
    sip.state = State::Play;
    sip.stream_id = 310;
    sip.session = "AA112233".to_owned();
    send_rtsp_reply(
        &mut sip,
        "RTSP/1.0 503 Service Unavailable\r\nCSeq: 114\r\n\r\n",
    );
    assert_eq!(sip.state, State::Teardown, "503 must move to TEARDOWN");
    sip.err = true; // error raised while the TEARDOWN was in flight
    sip.ignore_packets = false;
    send_rtsp_reply(&mut sip, "RTSP/1.0 200 OK\r\nCSeq: 115\r\n\r\n");
    assert_eq!(
        sip.state,
        State::Inactive,
        "TEARDOWN reply must park the adapter while in error"
    );
    assert!(!sip.ignore_packets, "no SETUP may restart while in error");

    // 454 without error still restarts the session (expiry recovery)
    sip.err = false;
    sip.state = State::Play;
    sip.stream_id = 309;
    send_rtsp_reply(
        &mut sip,
        "RTSP/1.0 454 Session Not Found\r\nCSeq: 116\r\n\r\n",
    );
    assert_eq!(
        sip.state,
        State::Setup,
        "454 must restart the session when healthy"
    );

    // 454 while in error stays parked
    sip.err = true;
    sip.state = State::Inactive;
    sip.ignore_packets = false;
    send_rtsp_reply(
        &mut sip,
        "RTSP/1.0 454 Session Not Found\r\nCSeq: 117\r\n\r\n",
    );
    assert_eq!(
        sip.state,
        State::Inactive,
        "454 must not restart while in error"
    );
    assert!(!sip.ignore_packets, "no SETUP may follow while in error");
}
