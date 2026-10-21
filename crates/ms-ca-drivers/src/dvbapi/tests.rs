//! `tests/test_dvbapi.cpp` ported, plus a scriptable fake oscam and protocol tests.

use std::collections::HashMap;

use ms_ca::{
    CaDescriptor, CaDriver, CaEvent, CaService, CapmtDiff, CapmtOp, CwAlgo, FilterId, StreamPid,
};
use ms_psi::{CrcCheck, Pmt};

use super::*;
use crate::dvbapi::proto;

const NOW: i64 = 1_000;

/// Fake oscam: records the client's bytes and scripts server replies.
struct FakeOscam {
    sent: Vec<u8>,
}

impl FakeOscam {
    fn new() -> Self {
        Self { sent: Vec::new() }
    }

    /// Takes everything the client wrote so far.
    fn drain(&mut self, client: &mut DvbapiClient) -> Vec<u8> {
        while let Some(bytes) = client.take_output() {
            self.sent.extend_from_slice(&bytes);
        }
        std::mem::take(&mut self.sent)
    }

    /// Delivers server bytes to the client.
    fn feed(&mut self, client: &mut DvbapiClient, bytes: &[u8]) {
        client.on_data(bytes, NOW);
    }
}

fn setup(offset: i32) -> (DvbapiClient, FakeOscam) {
    let cfg = DvbapiConfig {
        host: "127.0.0.1".to_string(),
        port: 9000,
        offset,
        ..DvbapiConfig::default()
    };
    let mut client = DvbapiClient::new(cfg);
    client.on_connected(NOW);
    let mut oscam = FakeOscam::new();
    let hello = oscam.drain(&mut client);
    assert_eq!(
        hello,
        proto::client_info(proto::DVBAPI_PROTOCOL_VERSION, &client.config().client_name)
    );
    (client, oscam)
}

fn server_info(build: i32, version: u16) -> Vec<u8> {
    let info = format!("OSCam v1.20, build r{build} (x86_64-linux)");
    let mut b = proto::DVBAPI_SERVER_INFO.to_be_bytes().to_vec();
    b.extend_from_slice(&version.to_be_bytes());
    b.push(info.len() as u8);
    b.extend_from_slice(info.as_bytes());
    b
}

fn register(client: &mut DvbapiClient, oscam: &mut FakeOscam) {
    oscam.feed(client, &server_info(11720, 2));
    assert_eq!(client.next_event(), Some(CaEvent::Ready));
    assert!(client.is_registered());
}

fn enabled_keys(client: &DvbapiClient) -> usize {
    client.keys().iter().filter(|k| k.enabled).count()
}

fn service(pmt_id: usize, adapter: usize, sid: u16, pmt_pid: u16) -> CaService {
    CaService {
        pmt_id,
        adapter,
        sid,
        pmt_pid,
        ..CaService::default()
    }
}

/// Section built like the one in the C++ and `ms-ca` tests.
fn pmt_section() -> Vec<u8> {
    let payload = [
        0xE1, 0x00, 0xF0, 0x0E, // PCR 0x100, program_info_length 14
        0x09, 0x04, 0x0B, 0x00, 0xE5, 0x73, // CA 0B00/0573
        0x09, 0x06, 0x0B, 0x00, 0xE5, 0x73, 0xAA, 0xBB, // same CAID/PID
        0x02, 0xE1, 0x01, 0xF0, 0x08, // video 0x101
        0x09, 0x06, 0x09, 0x8C, 0xE6, 0x00, 0x01, 0x02, // CA 098C/0600
        0x05, 0xE1, 0x05, 0xF0, 0x06, // private 0x105, not audio/video
        0x09, 0x04, 0x01, 0x00, 0xE7, 0x00, // ignored like C++
    ];
    let len = 5 + payload.len() + 4;
    let mut s = vec![0x02, 0xB0 | (len >> 8) as u8, len as u8, 0x00, 0x64];
    s.extend_from_slice(&[0xC1, 0, 0]);
    s.extend_from_slice(&payload);
    let crc = ms_ts::crc32_mpeg2(&s);
    s.extend_from_slice(&crc.to_be_bytes());
    s
}

fn parsed_service(pmt_id: usize, adapter: usize) -> CaService {
    let pmt = Pmt::parse(&pmt_section(), CrcCheck::Verify).unwrap();
    CaService::from_pmt(pmt_id, adapter, 0x0030, &pmt)
}

fn set_filter_bytes(key: u8, demux: u8, filter: u8, pid: u16, first: u8, mask: u8) -> Vec<u8> {
    let mut b = vec![0u8; 65];
    b[..4].copy_from_slice(&proto::DVBAPI_DMX_SET_FILTER.to_be_bytes());
    b[4] = key;
    b[5] = demux;
    b[6] = filter;
    b[7..9].copy_from_slice(&pid.to_be_bytes());
    b[9] = first;
    b[25] = mask;
    b
}

fn set_filter_host_order(key: u8, demux: u8, filter: u8, pid: u16) -> Vec<u8> {
    let mut b = vec![0u8; 67];
    b[0] = key;
    b[1..5].copy_from_slice(&[0x2B, 0x6F, 0x3C, 0x40]);
    b[5] = demux;
    b[6] = filter;
    b[7..9].copy_from_slice(&pid.to_le_bytes());
    b
}

fn dmx_stop_bytes(key: u8, demux: u8, filter: u8, pid: u16) -> Vec<u8> {
    let mut b = proto::DVBAPI_DMX_STOP.to_be_bytes().to_vec();
    b.extend_from_slice(&[key, demux, filter]);
    b.extend_from_slice(&pid.to_be_bytes());
    b
}

fn ca_set_descr(key: u8, parity: i32, cw: [u8; 8]) -> Vec<u8> {
    let mut b = proto::DVBAPI_CA_SET_DESCR.to_be_bytes().to_vec();
    b.push(key);
    b.extend_from_slice(&0i32.to_be_bytes());
    b.extend_from_slice(&parity.to_be_bytes());
    b.extend_from_slice(&cw);
    b
}

fn ca_set_descr_mode(key: u8, algo: i32, mode: i32) -> Vec<u8> {
    let mut b = proto::DVBAPI_CA_SET_DESCR_MODE.to_be_bytes().to_vec();
    b.push(key);
    b.extend_from_slice(&0i32.to_be_bytes());
    b.extend_from_slice(&algo.to_be_bytes());
    b.extend_from_slice(&mode.to_be_bytes());
    b
}

fn ca_set_descr_aes(key: u8, parity: i32, cw: [u8; 16]) -> Vec<u8> {
    let mut b = proto::DVBAPI_CA_SET_DESCR_AES.to_be_bytes().to_vec();
    b.push(key);
    b.extend_from_slice(&0i32.to_be_bytes());
    b.extend_from_slice(&parity.to_be_bytes());
    b.extend_from_slice(&cw);
    b
}

fn ecm_info_bytes(key: u8) -> Vec<u8> {
    let mut b = proto::DVBAPI_ECM_INFO.to_be_bytes().to_vec();
    b.push(key);
    b.extend_from_slice(&[0x00, 0x64, 0x09, 0x8C, 0x06, 0x00]);
    b.extend_from_slice(&[0, 0, 0, 1, 0, 0, 0, 150]);
    b.push(70);
    b.extend_from_slice(&[b'x'; 70]);
    for s in [&b"reader"[..], b"from", b"proto"] {
        b.push(s.len() as u8);
        b.extend_from_slice(s);
    }
    b.push(2);
    b
}

/// A VideoGuard ECM in iCAM mode `mode`; mode 0 is a plain CSA ECM.
fn build_ecm(table: u8, mode: u8) -> Vec<u8> {
    let len = 0x20;
    let mut s = vec![0u8; len];
    s[0] = table;
    s[1] = 0x70;
    s[2] = (len - 3) as u8;
    let byte2 = s[2];
    s[4] = if mode != 0 { byte2.wrapping_sub(4) } else { 0 };
    s[0x15] = mode;
    s[len - 1] = mode;
    s
}

fn filter_ids(client: &mut DvbapiClient) -> HashMap<u16, FilterId> {
    let mut ids = HashMap::new();
    while let Some(event) = client.next_event() {
        if let CaEvent::SetFilter { id, pid, .. } = event {
            ids.insert(pid, id);
        }
    }
    ids
}

// A PMT update re-send reuses the live key, keeping the demux index
// stable instead of leaking a key per version bump (#1346).
#[test]
fn add_pmt_reuses_live_key() {
    // Ported from tests/test_dvbapi.cpp:58
    let (mut client, mut oscam) = setup(0);
    let mut svc = service(0, 0, 100, 48);
    let before = enabled_keys(&client);

    client.add_service(&svc, false, NOW).unwrap();
    let k = client.key_for_pmt(0).unwrap();
    assert!(k.enabled, "no live key after add");
    assert_eq!(enabled_keys(&client), before + 1, "one key expected");
    let demux = k.demux_index;
    let id = k.id;

    svc.sid = 200;
    client.add_service(&svc, true, NOW).unwrap();
    let k = client.key_for_pmt(0).unwrap();
    assert_eq!(k.id, id, "re-send must reuse the live key");
    assert!(k.enabled, "reused key must stay enabled");
    assert_eq!(k.demux_index, demux, "demux index must stay stable");
    assert_eq!(k.sid, 200, "re-send must refresh the key");
    assert_eq!(enabled_keys(&client), before + 1, "re-send must add no key");

    // a dead key must fall through to a fresh allocation, not be reused
    client.keys[id].ver = 42;
    client.keys_del(id, NOW);
    client.add_service(&svc, false, NOW).unwrap();
    assert_eq!(enabled_keys(&client), before + 1, "one key expected again");
    let k = client.key_for_pmt(0).unwrap();
    assert!(k.enabled, "no live key after re-add");
    assert_eq!(k.ver, -1, "re-add must allocate a fresh key");
    oscam.drain(&mut client);
}

// OSCam filters the CAT and the SDT through the same key as the ECMs. A CW
// is keyed in the mode of the last ECM, so those tables must not reset it.
#[test]
fn icam_mode_only_from_ecms() {
    // Ported from tests/test_dvbapi.cpp:127
    static CAT: [u8; 18] = [
        0x01, 0xB0, 0x0F, 0xFF, 0xFF, 0xC1, 0x00, 0x00, 0x09, 0x04, 0x09, 0x8C, 0xE0, 0xCC, 0x11,
        0x22, 0x33, 0x44,
    ];
    static SDT: [u8; 14] = [
        0x42, 0xF0, 0x0B, 0x00, 0x01, 0xC1, 0x00, 0x00, 0x00, 0x01, 0xFF, 0x11, 0x22, 0x33,
    ];

    let (mut client, mut oscam) = setup(0);
    client
        .add_service(&service(0, 0, 100, 48), false, NOW)
        .unwrap();
    oscam.drain(&mut client);

    for (pos, pid) in [0x1CB6u16, 0x0001, 0x0011].into_iter().enumerate() {
        oscam.feed(&mut client, &set_filter_bytes(0, 0, pos as u8, pid, 0, 0));
    }
    let ids = filter_ids(&mut client);
    let fid_ecm = ids[&0x1CB6];
    let fid_cat = ids[&0x0001];
    let fid_sdt = ids[&0x0011];
    assert!(client.enabled);

    client.deliver_section(fid_ecm, &build_ecm(0x80, 4), false, NOW);
    let k = client.key_for_pmt(0).unwrap();
    assert!(
        k.is_icam && k.icam_ecm == 4,
        "an iCAM ECM did not set the mode"
    );

    client.deliver_section(fid_cat, &CAT, false, NOW);
    let k = client.key_for_pmt(0).unwrap();
    assert!(
        k.is_icam && k.icam_ecm == 4,
        "a CAT section reset the iCAM mode"
    );
    client.deliver_section(fid_sdt, &SDT, false, NOW);
    let k = client.key_for_pmt(0).unwrap();
    assert!(
        k.is_icam && k.icam_ecm == 4,
        "an SDT section reset the mode"
    );

    client.deliver_section(fid_ecm, &build_ecm(0x81, 0), false, NOW);
    let k = client.key_for_pmt(0).unwrap();
    assert!(
        !k.is_icam && k.icam_ecm == 0,
        "a plain ECM kept the iCAM mode"
    );
    oscam.drain(&mut client);
}

#[test]
fn channel_falls_back_to_sid() {
    // Ported from tests/test_dvbapi.cpp:174
    let (mut client, mut oscam) = setup(0);
    client
        .add_service(&service(0, 0, 1234, 48), false, NOW)
        .unwrap();
    let id = client.key_for_pmt(0).unwrap().id;

    // No SDT seen: the name is empty so the channel shows the SID
    let empty = client.key_channel(id, |_| Some((String::new(), 1234)));
    assert_eq!(
        empty, "SID 1234",
        "channel must fall back to the SID without SDT"
    );

    // SDT seen: the real name wins over the fallback
    let named = client.key_channel(id, |_| Some(("Test Channel".to_string(), 1234)));
    assert_eq!(
        named, "Test Channel",
        "channel must show the SDT name when present"
    );
    assert_eq!(client.key_channel(999, |_| None), "");
    oscam.drain(&mut client);
}

// An update re-send with identical CAPMT content writes nothing to the
// server; a changed one re-sends (#1346).
#[test]
fn update_resend_skips_identical_capmt() {
    // Ported from tests/test_dvbapi.cpp:221
    let (mut client, mut oscam) = setup(0);
    let mut svc = service(0, 0, 100, 48);

    client.add_service(&svc, false, NOW).unwrap();
    assert!(client.key_for_pmt(0).is_some_and(|k| k.enabled));
    assert!(
        !oscam.drain(&mut client).is_empty(),
        "first add must write the CAPMT"
    );

    client.add_service(&svc, true, NOW).unwrap();
    assert!(
        oscam.drain(&mut client).is_empty(),
        "identical re-send must write nothing"
    );

    // A second live key flips this key's list-management ONLY->ADD;
    // framing alone must not force a re-send either.
    let other = client.keys_add(0, 0, NOW).unwrap();
    assert!(client.keys[other].enabled, "no second live key");
    client.add_service(&svc, true, NOW).unwrap();
    assert!(
        oscam.drain(&mut client).is_empty(),
        "listmgmt flip must write nothing"
    );
    client.keys_del(other, NOW);
    oscam.drain(&mut client); // discard teardown bytes before the changed re-send

    svc.sid = 200;
    client.add_service(&svc, true, NOW).unwrap();
    assert!(
        !oscam.drain(&mut client).is_empty(),
        "changed re-send must write the CAPMT"
    );

    let id = client.key_for_pmt(0).unwrap().id;
    client.keys_del(id, NOW);
    oscam.drain(&mut client);
}

#[test]
fn close_socket_marks_without_deleting() {
    // Ported from tests/test_dvbapi.cpp:410
    let (mut client, mut oscam) = setup(0);
    client
        .add_service(&service(0, 0, 100, 48), false, NOW)
        .unwrap();
    oscam.drain(&mut client);

    client.mark_close();

    assert!(
        client.close_requested(),
        "socket must be marked, not deleted"
    );
    assert!(!client.is_enabled(), "dvbapi must be disabled");
    assert!(
        client.is_connected(),
        "the socket is closed by the IO layer"
    );
    assert!(client.key_for_pmt(0).is_some_and(|k| k.enabled));
    assert_eq!(client.next_event(), None, "no close before the socket dies");

    client.on_closed(NOW);
    assert!(!client.is_connected());
    assert!(client.key_for_pmt(0).is_none());
    assert_eq!(client.next_event(), Some(CaEvent::Closed));
}

// A PMT that decrypted and then gets no CWs is restarted for OSCam: CA_STOP,
// a new key and CAPMT on the next pass, while the PMT itself keeps running.
#[test]
fn descramble_watchdog() {
    // Ported from tests/test_dvbapi.cpp:277
    let (mut client, mut oscam) = setup(0);
    register(&mut client, &mut oscam);
    let now = 100_000;
    let mut svc = service(0, 0, 100, 48);
    svc.ca.push(CaDescriptor {
        caid: 0x0100,
        pid: 0x1000,
        private_data: Vec::new(),
    });
    client.add_service(&svc, false, now).unwrap();
    let id = client.key_for_pmt(0).unwrap().id;
    assert!(client.keys[id].enabled, "no live key after the CAPMT");
    oscam.drain(&mut client);

    let mut pmt = PmtStatus {
        pmt_id: 0,
        adapter: 0,
        enabled: true,
        running: true,
        last_decrypt_ok: 0,
        last_scrambled_pkt: 0,
    };

    // a PMT that never decrypted is left alone
    client.keys[id].created = now - 10_000;
    pmt.last_scrambled_pkt = now;
    client.watchdog(0, now, &[pmt]);
    assert_eq!(client.keys[id].stall_start, 0);
    assert!(client.keys[id].enabled);

    // a stall starts the clock, but no restart before the stall time
    pmt.last_decrypt_ok = now - 5_000;
    client.watchdog(0, now, &[pmt]);
    assert!(client.keys[id].stall_start > 0, "a stall starts the clock");
    assert!(client.keys[id].enabled, "no restart before the stall time");

    // a decrypt ends the stall
    pmt.last_decrypt_ok = now;
    client.watchdog(0, now, &[pmt]);
    assert_eq!(client.keys[id].stall_start, 0);
    pmt.last_decrypt_ok = now - 5_000;

    // no recent scrambled packet, no stall
    pmt.last_scrambled_pkt = now - 3_000;
    client.watchdog(0, now, &[pmt]);
    assert_eq!(client.keys[id].stall_start, 0);

    // a new key gets its grace
    pmt.last_scrambled_pkt = now;
    client.keys[id].stall_start = now - 4_000;
    client.keys[id].created = now;
    client.watchdog(0, now, &[pmt]);
    assert!(client.keys[id].enabled);

    // a key of another PMT is left alone
    client.keys[id].created = now - 10_000;
    client.keys[id].pmt_id = 1;
    client.watchdog(0, now, &[pmt]);
    assert!(client.keys[id].enabled);
    client.keys[id].pmt_id = 0;

    // only a running PMT is restarted
    pmt.running = false;
    client.watchdog(0, now, &[pmt]);
    assert!(client.keys[id].enabled);
    pmt.running = true;

    // nothing to restart without OSCam
    client.enabled = false;
    client.watchdog(0, now, &[pmt]);
    assert!(client.keys[id].enabled);
    client.enabled = true;

    // nothing to restart once dvbapi is unregistered
    client.registered = false;
    client.watchdog(0, now, &[pmt]);
    assert!(client.keys[id].enabled);
    client.registered = true;

    // a PMT of another adapter is left alone
    pmt.adapter = 1;
    client.watchdog(0, now, &[pmt]);
    assert!(client.keys[id].enabled);
    pmt.adapter = 0;

    // a key dvbapi deleted is left alone
    client.keys[id].enabled = false;
    client.watchdog(0, now, &[pmt]);
    assert_eq!(client.opaque.get(&0), Some(&id), "pmt opaque must stay");
    client.keys[id].enabled = true;

    assert!(
        oscam.drain(&mut client).is_empty(),
        "nothing sent to OSCam so far"
    );

    // a stall deletes the dvbapi key, the PMT keeps running
    client.watchdog(0, now, &[pmt]);
    assert!(!client.keys[id].enabled);
    assert!(
        client.key_for_pmt(0).is_none(),
        "a stall deletes the dvbapi key"
    );
    assert!(pmt.running, "the PMT keeps running");
    assert!(!oscam.drain(&mut client).is_empty(), "OSCam gets a CA_STOP");
    assert_eq!(client.next_event(), Some(CaEvent::Restart { pmt_id: 0 }));

    // the next pass sends a fresh CAPMT
    client.add_service(&svc, false, now).unwrap();
    let id2 = client.key_for_pmt(0).unwrap().id;
    assert!(client.keys[id2].enabled, "a new key after the restart");
    assert!(
        !oscam.drain(&mut client).is_empty(),
        "a fresh CAPMT after the restart"
    );

    // a second restart waits twice the grace
    client.keys[id2].stall_start = now - 4_000;
    client.keys[id2].created = now - 6_000;
    client.watchdog(0, now, &[pmt]);
    assert!(client.keys[id2].enabled);
    client.keys[id2].created = now - 11_000;
    client.watchdog(0, now, &[pmt]);
    assert!(!client.keys[id2].enabled);

    // a decrypt in between resets the back-off
    client.add_service(&svc, false, now).unwrap();
    let id3 = client.key_for_pmt(0).unwrap().id;
    assert!(
        client.keys[id3].enabled,
        "a new key after the second restart"
    );
    pmt.last_decrypt_ok = now - 4_500;
    client.keys[id3].stall_start = now - 4_000;
    client.keys[id3].created = now - 6_000;
    client.watchdog(0, now, &[pmt]);
    assert!(
        !client.keys[id3].enabled,
        "a decrypt in between resets the back-off"
    );
    oscam.drain(&mut client);

    // a started PMT has no stamps, so the client leaves it alone
    pmt.last_decrypt_ok = 0;
    pmt.last_scrambled_pkt = 0;
    client.add_service(&svc, false, now).unwrap();
    let id4 = client.key_for_pmt(0).unwrap().id;
    client.watchdog(0, now, &[pmt]);
    assert!(client.keys[id4].enabled);
    assert_eq!(
        (pmt.last_decrypt_ok, pmt.last_scrambled_pkt),
        (0, 0),
        "a started PMT has no stamps"
    );

    // a reused PMT slot has no stamps either
    client.remove_service(0, now);
    client.add_service(&svc, false, now).unwrap();
    let id5 = client.key_for_pmt(0).unwrap().id;
    client.watchdog(0, now, &[pmt]);
    assert!(client.keys[id5].enabled);
    oscam.drain(&mut client);
}

#[test]
fn capmt_bytes_for_parsed_pmt() {
    let (mut client, mut oscam) = setup(0);
    let svc = parsed_service(0, 0);
    assert_eq!(svc.sid, 0x64);
    assert_eq!(
        svc.streams,
        vec![
            StreamPid {
                stream_type: 2,
                pid: 0x101
            },
            StreamPid {
                stream_type: 5,
                pid: 0x105
            },
            StreamPid {
                stream_type: 0,
                pid: 0x100
            },
        ]
    );

    client.add_service(&svc, false, NOW).unwrap();
    let expected: Vec<u8> = vec![
        0x9F, 0x80, 0x32, 0x82, 0x00, 0x31, 0x03, 0x00, 0x64, 0x01, 0x00, 0x1C, 0x01, 0x83, 0x01,
        0x00, 0x84, 0x02, 0x00, 0x30, 0x86, 0x01, 0x00, 0x87, 0x01, 0x00, 0x09, 0x04, 0x0B, 0x00,
        // C++ copies the raw 16-bit pid; like upstream there are no 0xE0 bits.
        0x05, 0x73, 0x09, 0x06, 0x09, 0x8C, 0x06, 0x00, 0x01, 0x02, 0x02, 0x01, 0x01, 0x00, 0x00,
        0x05, 0x01, 0x05, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
    ];
    assert_eq!(oscam.drain(&mut client), expected);
}

#[test]
fn capmt_list_management_and_offset() {
    let (mut client, mut oscam) = setup(3);
    let svc = parsed_service(0, 0);
    client.add_service(&svc, false, NOW).unwrap();
    let out = oscam.drain(&mut client);
    assert_eq!(out[6], proto::CLM_ONLY);
    // Adapter (0x83) and both demux descriptors (0x86, 0x87) carry id + offset.
    assert_eq!((out[15], out[22], out[25]), (3, 3, 3));

    // A forced re-send with another live key uses CLM_ADD.
    client.services.insert(1, service(1, 0, 300, 49));
    let other = client.keys_add(0, 1, NOW).unwrap();
    client.add_service(&svc, false, NOW).unwrap();
    let out = oscam.drain(&mut client);
    assert_eq!(out[6], proto::CLM_ADD);
    client.keys_del(other, NOW);
    client.keys_del(client.key_for_pmt(0).unwrap().id, NOW);
    oscam.drain(&mut client);
}

#[test]
fn offset_applies_to_inbound_messages() {
    let (mut client, mut oscam) = setup(3);
    client
        .add_service(&service(0, 0, 100, 48), false, NOW)
        .unwrap();
    oscam.drain(&mut client);

    // CA_SET_PID is accepted and ignored like C++; the next frame still parses.
    let mut set_pid = proto::DVBAPI_CA_SET_PID.to_be_bytes().to_vec();
    set_pid.push(3);
    set_pid.extend_from_slice(&[0u8; 8]);
    oscam.feed(&mut client, &set_pid);

    oscam.feed(&mut client, &ca_set_descr(3, 0, [1; 8]));
    assert!(matches!(
        client.next_event(),
        Some(CaEvent::ControlWord { .. })
    ));
    oscam.feed(&mut client, &ca_set_descr(4, 0, [2; 8]));
    assert_eq!(
        client.next_event(),
        None,
        "a key without the offset is ignored"
    );

    oscam.feed(&mut client, &set_filter_bytes(3, 3, 0, 0x0500, 0, 0));
    let Some(CaEvent::SetFilter { id, adapter, .. }) = client.next_event() else {
        panic!("no filter event");
    };
    assert_eq!(adapter, 0);
    let ecm = [0x80, 0x70, 0x05, 1, 2, 3, 4, 5];
    client.deliver_section(id, &ecm, false, NOW);
    let data = oscam.drain(&mut client);
    assert_eq!(&data[..6], &[0xFF, 0xFF, 0x00, 0x00, 3, 0]);
    assert_eq!(&data[6..], &ecm);
}

#[test]
fn handshake_registers_and_refuses_old_oscam() {
    let (mut client, mut oscam) = setup(0);
    oscam.feed(&mut client, &server_info(11720, 5));
    assert_eq!(client.protocol_version(), proto::DVBAPI_PROTOCOL_VERSION);
    assert_eq!(client.oscam_version(), 11720);
    assert!(client.is_registered());
    assert_eq!(client.next_event(), Some(CaEvent::Ready));

    let (mut client, mut oscam) = setup(0);
    oscam.feed(&mut client, &server_info(11532, 2));
    assert!(!client.is_registered());
    assert_eq!(
        client.next_event(),
        Some(CaEvent::Unsupported { build: 11532 })
    );
}

#[test]
fn ecm_info_fills_the_key() {
    let (mut client, mut oscam) = setup(0);
    client
        .add_service(&service(0, 0, 100, 48), false, NOW)
        .unwrap();
    oscam.drain(&mut client);

    oscam.feed(&mut client, &ecm_info_bytes(0));
    let k = client.key_for_pmt(0).unwrap();
    assert_eq!((k.caid, k.info_pid, k.prid), (0x098C, 0x0600, 1));
    assert_eq!(k.ecm_time, 150);
    assert_eq!(k.cardsystem.len(), proto::ECM_INFO_STR_MAX);
    assert_eq!(k.reader, b"reader");
    assert_eq!(k.from, b"from");
    assert_eq!(k.protocol, b"proto");
    assert_eq!(k.hops, 2);
}

#[test]
fn filter_events_stop_and_emm_flag() {
    let (mut client, mut oscam) = setup(0);
    client
        .add_service(&service(0, 0, 100, 48), false, NOW)
        .unwrap();
    oscam.drain(&mut client);

    let msg = set_filter_bytes(0, 2, 7, 0x0500, 0x82, 0xFF);
    oscam.feed(&mut client, &msg);
    let Some(CaEvent::SetFilter {
        id,
        pmt_id,
        adapter,
        pid,
        filter,
        mask,
        emm,
    }) = client.next_event()
    else {
        panic!("no filter event");
    };
    assert_eq!((pmt_id, adapter, pid), (0, 0, 0x0500));
    assert_eq!(filter[0], 0x82);
    assert_eq!(mask[0], 0xFF);
    assert!(emm);

    // A repeated request keeps the existing filter (C++ only updates flags).
    oscam.feed(&mut client, &msg);
    assert_eq!(client.next_event(), None);

    // Host-order framing uses the same fields and is two bytes longer.
    oscam.feed(&mut client, &set_filter_host_order(0, 1, 9, 0x0600));
    let Some(CaEvent::SetFilter { pid, .. }) = client.next_event() else {
        panic!("no host-order filter event");
    };
    assert_eq!(pid, 0x0600);

    oscam.feed(&mut client, &dmx_stop_bytes(0, 2, 7, 0x0500));
    assert_eq!(client.next_event(), Some(CaEvent::StopFilter { id }));
    let k = client.key_for_pmt(0).unwrap();
    assert_eq!(k.ecms, 1);
    assert_eq!(k.last_dmx_stop, NOW);
    oscam.drain(&mut client);
}

#[test]
fn cw_events_csa_aes_and_modes() {
    let (mut client, mut oscam) = setup(0);
    client
        .add_service(&service(0, 0, 100, 48), false, NOW)
        .unwrap();
    oscam.drain(&mut client);

    let cw = [1, 2, 3, 6, 4, 5, 6, 15];
    oscam.feed(&mut client, &ca_set_descr(0, 1, cw));
    assert_eq!(
        client.next_event(),
        Some(CaEvent::ControlWord {
            pmt_id: 0,
            algo: CwAlgo::Csa,
            parity: 1,
            cw: cw.to_vec(),
            icam_ecm: 0,
        })
    );

    oscam.feed(&mut client, &ca_set_descr_mode(0, proto::CW_ALGO_AES128, 0));
    let aes = [7u8; 16];
    oscam.feed(&mut client, &ca_set_descr_aes(0, 0, aes));
    assert_eq!(
        client.next_event(),
        Some(CaEvent::ControlWord {
            pmt_id: 0,
            algo: CwAlgo::Aes128Ecb,
            parity: 0,
            cw: aes.to_vec(),
            icam_ecm: 0,
        })
    );

    oscam.feed(&mut client, &ca_set_descr_mode(0, proto::CW_ALGO_AES128, 1));
    oscam.feed(&mut client, &ca_set_descr_aes(0, 1, aes));
    let Some(CaEvent::ControlWord { algo, .. }) = client.next_event() else {
        panic!("no AES CBC event");
    };
    assert_eq!(algo, CwAlgo::Aes128Cbc);

    // Invalid parity and unknown keys are ignored.
    oscam.feed(&mut client, &ca_set_descr(0, 2, cw));
    oscam.feed(&mut client, &ca_set_descr(1, 0, cw));
    assert_eq!(client.next_event(), None);
    oscam.drain(&mut client);
}

#[test]
fn icam_ecm_names_the_cw_algorithm() {
    let (mut client, mut oscam) = setup(0);
    client
        .add_service(&service(0, 0, 100, 48), false, NOW)
        .unwrap();
    oscam.drain(&mut client);
    oscam.feed(&mut client, &set_filter_bytes(0, 0, 0, 0x1CB6, 0, 0));
    let id = filter_ids(&mut client)[&0x1CB6];

    client.deliver_section(id, &build_ecm(0x80, 4), false, NOW);
    let cw = [9u8; 8];
    oscam.feed(&mut client, &ca_set_descr(0, 0, cw));
    assert_eq!(
        client.next_event(),
        Some(CaEvent::ControlWord {
            pmt_id: 0,
            algo: CwAlgo::CsaIcam,
            parity: 0,
            cw: cw.to_vec(),
            icam_ecm: 4,
        })
    );
    oscam.drain(&mut client);
}

#[test]
fn partial_reads_are_buffered() {
    let (mut client, mut oscam) = setup(0);
    let info = server_info(11720, 2);
    for byte in &info {
        assert_eq!(client.next_event(), None);
        oscam.feed(&mut client, &[*byte]);
    }
    assert_eq!(client.next_event(), Some(CaEvent::Ready));
    client
        .add_service(&service(0, 0, 100, 48), false, NOW)
        .unwrap();
    oscam.drain(&mut client);

    // Two messages in one read, then one message split across two reads.
    let mut both = set_filter_bytes(0, 0, 0, 0x0500, 0, 0);
    both.extend_from_slice(&ca_set_descr(0, 0, [1; 8]));
    oscam.feed(&mut client, &both);
    assert!(matches!(
        client.next_event(),
        Some(CaEvent::SetFilter { .. })
    ));
    assert!(matches!(
        client.next_event(),
        Some(CaEvent::ControlWord { .. })
    ));

    let aes = ca_set_descr_aes(0, 1, [2; 16]);
    oscam.feed(&mut client, &aes[..10]);
    assert_eq!(client.next_event(), None);
    oscam.feed(&mut client, &aes[10..]);
    assert!(matches!(
        client.next_event(),
        Some(CaEvent::ControlWord { .. })
    ));
    oscam.drain(&mut client);
}

#[test]
fn unknown_opcode_drops_the_buffer() {
    let (mut client, mut oscam) = setup(0);
    oscam.feed(&mut client, &[0x12, 0x34, 0x56, 0x78, 0xAA]);
    assert_eq!(client.next_event(), None);
    oscam.feed(&mut client, &server_info(11720, 2));
    assert_eq!(client.next_event(), Some(CaEvent::Ready));
}

#[test]
fn reconnect_repeats_the_handshake() {
    let cfg = DvbapiConfig {
        host: "127.0.0.1".to_string(),
        port: 9000,
        ..DvbapiConfig::default()
    };
    let mut client = DvbapiClient::new(cfg);
    assert_eq!(client.poll(NOW, false), PollAction::None);
    assert_eq!(client.poll(NOW, true), PollAction::Connect);

    client.on_connected(NOW);
    assert!(client.is_enabled());
    assert_eq!(
        client.take_output().unwrap(),
        proto::client_info(proto::DVBAPI_PROTOCOL_VERSION, &client.config().client_name)
    );
    let svc = service(0, 0, 100, 48);
    client.add_service(&svc, false, NOW).unwrap();

    client.mark_close();
    assert!(client.close_requested());
    client.on_closed(NOW);
    assert_eq!(client.next_event(), Some(CaEvent::Closed));
    assert_eq!(
        client.add_service(&svc, false, NOW),
        Err(CaError::NotConnected)
    );

    assert_eq!(client.poll(NOW, true), PollAction::Connect);
    client.on_connected(NOW);
    assert!(
        client.take_output().is_some(),
        "CLIENT_INFO after reconnect"
    );
    assert!(client.add_service(&svc, false, NOW).is_ok());
}

#[test]
fn idle_timeout_keeps_the_connection() {
    let (mut client, mut oscam) = setup(0);
    register(&mut client, &mut oscam);
    // C++ dvbapi_timeout() always returns 0, so the 2 s socket timeout never
    // closes the connection.
    assert_eq!(client.poll(NOW + 2_001, true), PollAction::None);
    assert!(client.is_connected());
    assert!(client.is_enabled());
    assert!(oscam.drain(&mut client).is_empty());
}

#[test]
fn poll_restarts_a_key_without_filters() {
    let (mut client, mut oscam) = setup(0);
    client
        .add_service(&service(0, 0, 100, 48), false, NOW)
        .unwrap();
    oscam.drain(&mut client);
    assert_eq!(client.poll(NOW + 3_001, true), PollAction::None);
    assert!(client.key_for_pmt(0).is_none(), "idle key must be closed");
    assert_eq!(client.next_event(), Some(CaEvent::Restart { pmt_id: 0 }));
    assert!(!oscam.drain(&mut client).is_empty(), "OSCam gets a CA_STOP");
}

#[test]
fn ca_driver_trait_applies_diffs() {
    let (mut client, mut oscam) = setup(0);
    assert_eq!(client.capabilities().max_services, Some(MAX_KEYS));
    client.set_time(NOW);

    let diff = CapmtDiff {
        ops: vec![CapmtOp::Add(service(0, 0, 100, 48))],
    };
    client.apply(&diff).unwrap();
    assert!(client.key_for_pmt(0).is_some());

    let mut updated = service(0, 0, 200, 48);
    updated.streams.push(StreamPid {
        stream_type: 2,
        pid: 0x101,
    });
    let diff = CapmtDiff {
        ops: vec![CapmtOp::Update(updated)],
    };
    client.apply(&diff).unwrap();
    assert_eq!(client.key_for_pmt(0).unwrap().sid, 200);

    let diff = CapmtDiff {
        ops: vec![CapmtOp::Remove { pmt_id: 0 }],
    };
    client.apply(&diff).unwrap();
    assert!(client.key_for_pmt(0).is_none());
    oscam.drain(&mut client);
}

#[test]
fn config_from_options_matches_dash_o() {
    let mut opts = ms_config::Options::default();
    ms_config::parse_dvbapi_opt("~192.168.9.9:9999,2", &mut opts);
    let cfg = DvbapiConfig::from_options(&opts);
    assert_eq!(cfg.host, "192.168.9.9");
    assert_eq!(cfg.port, 9999);
    assert_eq!(cfg.offset, 2);
    assert!(cfg.pids_all_no_dec);
    assert!(cfg.network_mode());

    let mut opts = ms_config::Options::default();
    ms_config::parse_dvbapi_opt("/tmp/oscam.sock", &mut opts);
    let cfg = DvbapiConfig::from_options(&opts);
    assert_eq!(cfg.port, 9000);
    assert!(!cfg.network_mode());
}
