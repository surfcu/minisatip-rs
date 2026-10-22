//! CAPMT building and packing tests ported from tests/test_ca.cpp.

use ms_ca::capmt::*;
use ms_ca::{
    CaDevice, CaDriver, CaPolicy, CaState, ProgramTable, Reconciler, RecordingDriver, TablesResult,
};

fn ctx(t: &ProgramTable) -> BuildCtx<'_> {
    BuildCtx {
        programs: t,
        caid_filter: &[],
    }
}

fn build(t: &ProgramTable, c: &Capmt, lm: u8, len: usize, cmd: u8) -> Vec<u8> {
    create_capmt(c, lm, len, cmd, false, true, ctx(t))
        .expect("create_capmt")
        .bytes
}

fn single(id: i32, version: i32, sid: i32) -> Capmt {
    Capmt::with_programs(id, [PMT_INVALID; 2], version, sid)
}

/// The global `d` of test_ca.cpp main: multiple_pmt 1, one CAPMT.
fn global_device() -> CaDevice {
    CaDevice {
        enabled: true,
        multiple_pmt: 1,
        max_ca_pmt: 1,
        ..CaDevice::default()
    }
}

fn device(max_ca_pmt: usize, multiple_pmt: usize) -> CaDevice {
    CaDevice {
        enabled: true,
        max_ca_pmt,
        multiple_pmt,
        ..CaDevice::default()
    }
}

// Ported from tests/test_ca.cpp:80
#[test]
fn test_multiple_pmt() {
    let mut t = ProgramTable::new();
    for sid in [100, 200, 300, 400] {
        t.add(0, sid, sid);
    }
    let mut d = global_device();
    d.add_pmt_to_capmt(&t, 0, 1);
    d.add_pmt_to_capmt(&t, 1, 1);
    assert_eq!(d.active_capmts(), 1, "expected number of capmt ahould be 1");
    d.remove_pmt_from_device(0);
    assert_eq!(d.enabled_pmts(), 1, "expected capmt should be 1");
    assert!(
        !pmt_id_is_valid(d.capmt[0].pmt_id) && d.capmt[0].other_id[0] == 1,
        "only the partner should be active, in its place"
    );
    assert_eq!(d.add_pmt_to_capmt(&t, 2, 0), None, "failed adding PMT");
}

// Ported from tests/test_ca.cpp:103
#[test]
fn test_create_capmt_not_selected() {
    let mut t = ProgramTable::new();
    let id = t.add(0, 0x100, 0x101);
    let p = t.get_mut(id).unwrap();
    p.add_caid(0x0B00, 0x573, &[]);
    p.add_stream(0x501, 2, false, true);
    p.add_stream(0x502, 3, true, false);
    let c = build(
        &t,
        &single(id, 2, 0x1234),
        CLM_UPDATE,
        1500,
        CMD_ID_NOT_SELECTED,
    );
    assert_eq!(c[0], CLM_UPDATE, "ca_pmt_list_management incorrect");
    assert_eq!(
        u16::from_be_bytes([c[1], c[2]]),
        0x1234,
        "program_number incorrect"
    );
    assert_eq!(
        c[11], CMD_ID_NOT_SELECTED,
        "stream PID 1 should be released"
    );
    assert_eq!(
        c[23], CMD_ID_NOT_SELECTED,
        "stream PID 2 should be released"
    );
}

// Ported from tests/test_ca.cpp:134
#[test]
fn test_capmt_release_on_last_pmt() {
    let mut t = ProgramTable::new();
    let mut dev = device(4, 0);
    dev.state = CaState::Initialized;
    // No CA session: the release is attempted but never written.
    let mut drv = RecordingDriver {
        no_session: true,
        ..RecordingDriver::default()
    };
    let first = t.add(0, 0x300, 0x301);
    let second = t.add(0, 0x400, 0x401);
    let c1 = dev.add_pmt_to_capmt(&t, first, 1).unwrap();
    let c2 = dev.add_pmt_to_capmt(&t, second, 1).unwrap();
    assert_ne!(c1, c2, "the two PMTs should be in different CAPMTs");
    let version = dev.capmt[c2].version;
    dev.del_pmt(second, &t, &mut drv);
    assert!(
        !pmt_id_is_valid(dev.capmt[c2].pmt_id),
        "the CAPMT should have been released"
    );
    assert_eq!(
        dev.capmt[c2].version,
        (version + 1) & 0xF,
        "the release should be sent as a new version of the program"
    );
    assert_eq!(
        dev.capmt[c1].pmt_id, first,
        "the other CAPMT should be left alone"
    );
}

// Ported from tests/test_ca.cpp:166
#[test]
fn test_capmt_uses_empty_slots_first() {
    let mut t = ProgramTable::new();
    let mut dev = device(4, 1);
    let first = t.add(0, 500, 500);
    let second = t.add(0, 600, 600);
    assert_eq!(
        dev.add_pmt_to_capmt(&t, first, 1),
        Some(0),
        "the first PMT should use the first CAPMT"
    );
    assert_eq!(
        dev.add_pmt_to_capmt(&t, second, 1),
        Some(1),
        "the second PMT should use an empty CAPMT, not the one in use"
    );
    assert!(
        dev.capmt[0].pmt_id == first && !pmt_id_is_valid(dev.capmt[0].other_id[0]),
        "the first CAPMT should be left alone"
    );
    assert_eq!(
        dev.capmt[1].pmt_id, second,
        "the second CAPMT should be used"
    );
    let third = t.add(0, 700, 700);
    let fourth = t.add(0, 800, 800);
    let fifth = t.add(0, 900, 900);
    dev.add_pmt_to_capmt(&t, third, 1);
    dev.add_pmt_to_capmt(&t, fourth, 1);
    assert_eq!(
        dev.add_pmt_to_capmt(&t, fifth, 1),
        Some(0),
        "the fifth PMT should be packed in the first CAPMT once all of them are used"
    );
    assert_eq!(
        dev.capmt[0].other_id[0], fifth,
        "expected the fifth PMT packed"
    );
    assert_eq!(
        dev.add_pmt_to_capmt(&t, second, 1),
        Some(1),
        "an update should reuse the same CAPMT"
    );
}

// Ported from tests/test_ca.cpp:207
#[test]
fn test_create_capmt_single_clear() {
    let mut t = ProgramTable::new();
    let id = t.add(0, 0x100, 0x101);
    let p = t.get_mut(id).unwrap();
    p.add_stream(0x501, 2, false, true);
    p.add_stream(0x502, 3, true, false);
    let c = build(
        &t,
        &single(id, 1, 0x1234),
        CLM_ONLY,
        1500,
        CMD_ID_OK_DESCRAMBLING,
    );
    assert_eq!(c[0], CLM_ONLY, "ca_pmt_list_management incorrect");
    assert_eq!(
        u16::from_be_bytes([c[1], c[2]]),
        0x1234,
        "program_number incorrect"
    );
    assert_eq!((c[3] & 0x3E) >> 1, 1, "version incorrect");
    assert_eq!(c[3] & 1, 1, "current next indicator wrong");
    assert_eq!(
        u16::from_be_bytes([c[4], c[5]]),
        0,
        "program info length incorrect"
    );
    assert_eq!(c[6], 2, "stream PID 1 service type incorrect");
    assert_eq!(c[11], 3, "stream PID 2 service type incorrect");
}

// Ported from tests/test_ca.cpp:242
#[test]
fn test_capmt_content_hash_masks_version_and_listmgmt() {
    let mut t = ProgramTable::new();
    let id = t.add(0, 0x100, 0x101);
    let p = t.get_mut(id).unwrap();
    p.add_stream(0x501, 2, false, true);
    p.add_stream(0x502, 3, true, false);
    let s = single(id, 1, 0x1234);
    let c = build(&t, &s, CLM_ONLY, 1500, CMD_ID_OK_DESCRAMBLING);
    let hash = capmt_content_hash(&c);
    assert_ne!(hash, 0, "hash should be non-trivial");
    assert_eq!(capmt_content_hash(&c), hash, "hash should be stable");
    let mut other = c.clone();
    other[3] ^= 0x3E;
    assert_eq!(
        capmt_content_hash(&other),
        hash,
        "version-only change must not flip the hash"
    );
    let mut other = c.clone();
    other[3] ^= 0x01;
    assert_ne!(
        capmt_content_hash(&other),
        hash,
        "non-version bit change must flip the hash"
    );
    let mut other = c.clone();
    other[0] ^= 0xFF;
    assert_eq!(
        capmt_content_hash(&other),
        hash,
        "list management change must not flip the hash"
    );
    let u = build(&t, &s, CLM_UPDATE, 1500, CMD_ID_OK_DESCRAMBLING);
    assert_eq!(
        u.len(),
        c.len(),
        "ONLY and UPDATE builds must match in length"
    );
    assert_eq!(
        capmt_content_hash(&u),
        hash,
        "ONLY->UPDATE flip alone must not flip the hash"
    );
    let mut other = c.clone();
    *other.last_mut().unwrap() ^= 0xFF;
    assert_ne!(
        capmt_content_hash(&other),
        hash,
        "content change must flip the hash"
    );
}

// Ported from tests/test_ca.cpp:294
#[test]
fn test_create_capmt_single_pmt_scrambled() {
    let mut t = ProgramTable::new();
    let id = t.add(0, 0x100, 0x101);
    let p = t.get_mut(id).unwrap();
    p.add_caid(0x0B00, 0x573, &[]);
    p.add_stream(0x501, 2, false, true);
    p.add_stream(0x502, 3, true, false);
    let c = build(
        &t,
        &single(id, 1, 0x1234),
        CLM_ONLY,
        1500,
        CMD_ID_OK_DESCRAMBLING,
    );
    assert_eq!(
        u16::from_be_bytes([c[4], c[5]]),
        0,
        "program info length incorrect"
    );
    assert_eq!(c[6], 2, "stream PID 1 service type incorrect");
    assert_eq!(
        u16::from_be_bytes([c[9], c[10]]),
        7,
        "stream PID 1 ES info length incorrect"
    );
    assert_eq!(c[18], 3, "stream PID 2 service type incorrect");
    assert_eq!(
        u16::from_be_bytes([c[21], c[22]]),
        7,
        "stream PID 1 ES info length incorrect"
    );
}

// Ported from tests/test_ca.cpp:327
#[test]
fn test_capmt_packs_three_pmts() {
    let mut t = ProgramTable::new();
    let mut dev = device(2, 2);
    let ids: Vec<i32> = (0..7).map(|i| t.add(0, 1000 + i, 1000 + i)).collect();
    for (i, &id) in ids.iter().take(6).enumerate() {
        assert_eq!(
            dev.add_pmt_to_capmt(&t, id, 2),
            Some(i % 2),
            "PMTs should alternate between the two CAPMTs"
        );
    }
    assert!(
        dev.capmt[0].pmt_id == ids[0] && dev.capmt[0].other_id == [ids[2], ids[4]],
        "first CAPMT should carry PMTs 0, 2 and 4"
    );
    assert_eq!(dev.capmt[1].pmt_count(), 3, "second CAPMT should be full");
    assert_eq!(dev.enabled_pmts(), 6, "expected 6 enabled PMTs");
    assert_eq!(
        dev.add_pmt_to_capmt(&t, ids[6], 2),
        None,
        "a seventh PMT should not fit"
    );
    dev.remove_pmt_from_device(ids[0]);
    assert!(
        !pmt_id_is_valid(dev.capmt[0].pmt_id) && dev.capmt[0].other_id == [ids[2], ids[4]],
        "partners should keep their places"
    );
    dev.remove_pmt_from_device(ids[3]);
    assert!(
        dev.capmt[1].pmt_id == ids[1]
            && !pmt_id_is_valid(dev.capmt[1].other_id[0])
            && dev.capmt[1].other_id[1] == ids[5],
        "middle partner should be removed in place"
    );
    assert_eq!(
        dev.add_pmt_to_capmt(&t, ids[6], 2),
        Some(0),
        "a freed place should be reused"
    );
    assert_eq!(
        dev.capmt[0].pmt_id, ids[6],
        "PMT 6 should fill the first place"
    );
}

// Ported from tests/test_ca.cpp:375
#[test]
fn test_capmt_single_star_packs_two() {
    let mut t = ProgramTable::new();
    let mut dev = device(1, 1);
    let a = t.add(0, 1100, 1100);
    let b = t.add(0, 1101, 1101);
    let c = t.add(0, 1102, 1102);
    assert_eq!(dev.add_pmt_to_capmt(&t, a, 1), Some(0), "first PMT");
    assert_eq!(dev.add_pmt_to_capmt(&t, b, 1), Some(0), "packed PMT");
    assert_eq!(
        dev.add_pmt_to_capmt(&t, c, 1),
        None,
        "a third PMT should not fit with a single '*'"
    );
}

// Ported from tests/test_ca.cpp:393
#[test]
fn test_create_capmt_three_pmts() {
    let mut t = ProgramTable::new();
    let mut p = [0; 3];
    for (i, id) in p.iter_mut().enumerate() {
        let i = i as u16;
        *id = t.add(0, 0x300 + i, 0x310 + i);
        let pmt = t.get_mut(*id).unwrap();
        pmt.add_caid(0x0664, 0x700 + i, &[]);
        pmt.add_stream(0x800 + i * 0x10, 2, false, true);
        pmt.add_stream(0x801 + i * 0x10, 3, true, false);
    }
    let s = Capmt::with_programs(p[0], [p[1], p[2]], 1, 0x1234);
    let c = build(&t, &s, CLM_ONLY, 1500, CMD_ID_OK_DESCRAMBLING);
    assert_eq!(c.len(), 6 + 6 * 12, "expected 6 streams in the CAPMT");
    for i in 0..6 {
        let pid = (u16::from(c[6 + i * 12 + 1] & 0x1F) << 8) | u16::from(c[6 + i * 12 + 2]);
        let ecm = (u16::from(c[6 + i * 12 + 10] & 0x1F) << 8) | u16::from(c[6 + i * 12 + 11]);
        assert_eq!(
            pid as usize,
            0x800 + (i / 2) * 0x10 + i % 2,
            "stream pid order"
        );
        assert_eq!(
            ecm as usize,
            0x700 + i / 2,
            "each stream keeps its own ECM pid"
        );
    }
}

// Ported from tests/test_ca.cpp:423
#[test]
fn test_create_capmt_multiple_pmt_scrambled() {
    let mut t = ProgramTable::new();
    let id = t.add(0, 0x100, 0x101);
    let p = t.get_mut(id).unwrap();
    p.add_caid(0x0B00, 0x573, &[]);
    p.add_stream(0x501, 2, false, true);
    p.add_stream(0x502, 3, true, false);
    let other = t.add(0, 0x200, 0x201);
    let o = t.get_mut(other).unwrap();
    o.add_caid(0x0B01, 0xABC, &[]);
    o.add_stream(0x601, 2, false, true);
    o.add_stream(0x602, 3, true, false);
    let s = Capmt::with_programs(id, [other, PMT_INVALID], 1, 0x1234);
    let c = build(&t, &s, CLM_ONLY, 1500, CMD_ID_OK_DESCRAMBLING);
    assert_eq!(
        u16::from_be_bytes([c[4], c[5]]),
        0,
        "program info length incorrect"
    );
    assert_eq!(c[6], 2, "stream PID 1 service type incorrect");
    assert_eq!(
        u16::from_be_bytes([c[9], c[10]]),
        7,
        "stream PID 1 ES info length incorrect"
    );
    assert_eq!(c[18], 3, "stream PID 2 service type incorrect");
    assert_eq!(
        u16::from_be_bytes([c[21], c[22]]),
        7,
        "stream PID 1 ES info length incorrect"
    );
    assert_eq!(c[30], 2, "stream PID 3 service type incorrect");
    assert_eq!(
        u16::from_be_bytes([c[33], c[34]]),
        7,
        "stream PID 3 ES info length incorrect"
    );
    assert_eq!(c[42], 3, "stream PID 3 service type incorrect");
    assert_eq!(
        u16::from_be_bytes([c[45], c[46]]),
        7,
        "stream PID 4 ES info length incorrect"
    );
}

// Ported from tests/test_ca.cpp:468
#[test]
fn test_create_capmt_different_listmgmt() {
    let mut t = ProgramTable::new();
    let id = t.add(0, 0x100, 0x101);
    t.get_mut(id).unwrap().add_stream(0x501, 2, false, true);
    let s = single(id, 1, 0x1234);
    for lm in [CLM_FIRST, CLM_LAST, CLM_ADD, CLM_UPDATE, CLM_MORE] {
        let c = build(&t, &s, lm, 1500, CMD_ID_OK_DESCRAMBLING);
        assert!(!c.is_empty(), "create_capmt failed for {lm}");
        assert_eq!(c[0], lm, "listmgmt should be {lm}");
    }
}

// Ported from tests/test_ca.cpp:514
#[test]
fn test_create_capmt_different_cmd_ids() {
    let mut t = ProgramTable::new();
    let id = t.add(0, 0x100, 0x101);
    let p = t.get_mut(id).unwrap();
    p.add_caid(0x0B00, 0x573, &[]);
    p.add_stream(0x501, 2, false, true);
    let s = single(id, 1, 0x1234);
    for cmd in [CMD_ID_OK_MMI, CMD_ID_QUERY, CMD_ID_NOT_SELECTED] {
        let c = build(&t, &s, CLM_ONLY, 1500, cmd);
        assert_eq!(c[11], cmd, "cmd_id should be {cmd}");
    }
}

// Ported from tests/test_ca.cpp:551 (C++ returns 1, Rust an error)
#[test]
fn test_create_capmt_invalid_pmt() {
    let t = ProgramTable::new();
    let r = create_capmt(
        &single(9999, 1, 0x1234),
        CLM_ONLY,
        1500,
        CMD_ID_OK_DESCRAMBLING,
        false,
        true,
        ctx(&t),
    );
    assert_eq!(
        r,
        Err(CapmtError::PmtNotFound(9999)),
        "create_capmt should fail for invalid PMT"
    );
}

// Ported from tests/test_ca.cpp:566
#[test]
fn test_create_capmt_max_version() {
    let mut t = ProgramTable::new();
    let id = t.add(0, 0x100, 0x101);
    t.get_mut(id).unwrap().add_stream(0x501, 2, false, true);
    let c = build(
        &t,
        &single(id, 15, 0x1234),
        CLM_ONLY,
        1500,
        CMD_ID_OK_DESCRAMBLING,
    );
    assert_eq!((c[3] & 0x3E) >> 1, 15, "version should be 15");
}

// Ported from tests/test_ca.cpp:587
#[test]
fn test_create_capmt_large_sid() {
    let mut t = ProgramTable::new();
    let id = t.add(0, 0x100, 0x101);
    t.get_mut(id).unwrap().add_stream(0x501, 2, false, true);
    let c = build(
        &t,
        &single(id, 1, 0xFFFF),
        CLM_ONLY,
        1500,
        CMD_ID_OK_DESCRAMBLING,
    );
    assert_eq!(
        u16::from_be_bytes([c[1], c[2]]),
        0xFFFF,
        "SID should be 0xFFFF"
    );
}

fn two_big_programs(t: &mut ProgramTable, caids: u16, streams: u16) -> (i32, i32) {
    let id = t.add(0, 0x100, 0x101);
    let other = t.add(0, 0x200, 0x201);
    for (pid_base, caid_base, ecm_base, es_base) in
        [(id, 0x0B00, 0x570, 0x500), (other, 0x0C00, 0x670, 0x600)]
    {
        let p = t.get_mut(pid_base).unwrap();
        for i in 0..caids {
            p.add_caid(caid_base + i, ecm_base + i, &[]);
        }
        for i in 0..streams {
            let video = i % 2 == 0;
            p.add_stream(es_base + i, if video { 2 } else { 3 }, !video, video);
        }
    }
    (id, other)
}

// Ported from tests/test_ca.cpp:608
#[test]
fn test_create_capmt_both_pmt_and_other_with_caids() {
    let mut t = ProgramTable::new();
    let (id, other) = two_big_programs(&mut t, 8, 2);
    let s = Capmt::with_programs(id, [other, PMT_INVALID], 1, 0x1234);
    let c = build(&t, &s, CLM_ONLY, 1500, CMD_ID_OK_DESCRAMBLING);
    assert!(!c.is_empty() && c.len() < 1500, "CAPMT exceeds 1500 bytes");
    assert_eq!(c[0], CLM_ONLY, "ca_pmt_list_management incorrect");
    assert_eq!(
        u16::from_be_bytes([c[1], c[2]]),
        0x1234,
        "program_number incorrect"
    );
}

// Ported from tests/test_ca.cpp:649
#[test]
fn test_create_capmt_both_pmt_and_other_many_streams() {
    let mut t = ProgramTable::new();
    let (id, other) = two_big_programs(&mut t, 10, 8);
    let s = Capmt::with_programs(id, [other, PMT_INVALID], 5, 0xABCD);
    let c = build(&t, &s, CLM_ONLY, 1500, CMD_ID_OK_DESCRAMBLING);
    assert!(
        !c.is_empty() && c.len() < 1500,
        "CAPMT must be smaller than 1500 bytes"
    );
}

// Ported from tests/test_ca.cpp:689
#[test]
fn test_create_capmt_size_near_limit() {
    let mut t = ProgramTable::new();
    let (id, other) = two_big_programs(&mut t, 10, 11);
    let s = Capmt::with_programs(id, [other, PMT_INVALID], 1, 0x9999);
    let c = build(&t, &s, CLM_ONLY, 1500, CMD_ID_OK_DESCRAMBLING);
    assert!(c.len() < 1500, "CAPMT must be smaller than 1500 bytes");
    assert!(c.len() > 1200, "CAPMT should be close to 1500 bytes limit");
}

// Ported from tests/test_ca.cpp:735
#[test]
fn test_create_capmt_respects_length() {
    let mut t = ProgramTable::new();
    let id = t.add(0, 0x100, 0x101);
    let p = t.get_mut(id).unwrap();
    let private = [0xABu8; 64];
    for i in 0..ms_ca::program::MAX_CAID as u16 {
        p.add_caid(0x0B00 + i, 0x570 + i, &private);
    }
    for i in 0..20 {
        p.add_stream(0x500 + i, 2, false, true);
    }
    let c = build(
        &t,
        &single(id, 1, 0x9999),
        CLM_ONLY,
        40,
        CMD_ID_OK_DESCRAMBLING,
    );
    assert!(
        !c.is_empty() && c.len() <= 40,
        "CAPMT must respect its length"
    );
}

/// The packing CAM of test_ca.cpp:2020: 3 programs per CAPMT, one CAPMT.
struct PackingCam {
    t: ProgramTable,
    dev: CaDevice,
    drv: RecordingDriver,
    next_pid: u16,
}

impl PackingCam {
    // Ported from tests/test_ca.cpp:2029
    fn open() -> Self {
        let mut t = ProgramTable::new();
        t.track_requested_pids();
        let mut dev = CaDevice::new(3, 1, 2);
        dev.state = CaState::Initialized;
        Self {
            t,
            dev,
            drv: RecordingDriver::default(),
            next_pid: 0x1000,
        }
    }

    // Ported from tests/test_ca.cpp:2072
    fn program(&mut self, sid: u16) -> i32 {
        let id = self.t.add(3, sid, sid);
        for video in [true, false] {
            let pid = self.next_pid;
            self.next_pid += 1;
            let p = self.t.get_mut(id).unwrap();
            if video {
                p.add_caid(0x0B00, 0x573, &[]);
            }
            p.add_stream(pid, if video { 2 } else { 3 }, !video, video);
            self.t.request_pid(3, pid);
        }
        id
    }

    fn join(&mut self, id: i32, update: bool) -> TablesResult {
        self.dev.process_pmt(id, update, &self.t, &mut self.drv)
    }

    fn leave(&mut self, id: i32) {
        self.dev.del_pmt(id, &self.t, &mut self.drv);
    }

    // Ported from tests/test_ca.cpp:2089
    fn read_capmts(&mut self) -> (usize, Option<Vec<u8>>) {
        let sent = self.drv.take();
        (sent.len(), sent.last().cloned())
    }

    fn first_pid(&self, id: i32) -> u16 {
        self.t.get(id).unwrap().streams[0].pid
    }
}

// Ported from tests/test_ca.cpp:2114
fn capmt_lists_pid(c: &[u8], pid: u16) -> bool {
    let mut i = 6;
    while i + 4 < c.len() {
        if (u16::from(c[i + 1] & 0x1F) << 8 | u16::from(c[i + 2])) == pid {
            return true;
        }
        i += 5 + ((usize::from(c[i + 3] & 0x0F) << 8) | usize::from(c[i + 4]));
    }
    false
}

// Ported from tests/test_ca.cpp:2124 (the ca_timeout poll has no Rust counterpart)
#[test]
fn test_capmt_joins_send_at_once() {
    let mut cam = PackingCam::open();
    let p1 = cam.program(0x2001);
    let p2 = cam.program(0x2002);
    let p3 = cam.program(0x2003);
    assert_eq!(cam.join(p1, false), TablesResult::Ok, "first join");
    assert_eq!(cam.read_capmts().0, 1, "a fresh CAPMT is sent at once");
    assert_eq!(
        cam.dev.capmt[0].sid, 0x2001,
        "a fresh CAPMT takes the real sid"
    );
    assert_eq!(cam.join(p2, false), TablesResult::Ok, "second join");
    assert_eq!(cam.read_capmts().0, 1, "a partner join is sent at once");
    assert_eq!(cam.join(p3, false), TablesResult::Ok, "third join");
    assert_eq!(cam.read_capmts().0, 1, "each join is its own send");
    assert_eq!(
        cam.dev.capmt[0].sid, 0x2001,
        "partners keep the running program number"
    );
    assert_eq!(cam.read_capmts().0, 0, "the timeout sends no CAPMT");
}

// Ported from tests/test_ca.cpp:2148
#[test]
fn test_capmt_partner_leave_is_lazy() {
    let mut cam = PackingCam::open();
    let p1 = cam.program(0x2101);
    let p2 = cam.program(0x2102);
    let p3 = cam.program(0x2103);
    cam.join(p1, false);
    cam.join(p2, false);
    cam.join(p3, false);
    let (n, before) = cam.read_capmts();
    assert_eq!(n, 3, "setup sends three CAPMTs");
    let version = cam.dev.capmt[0].version;
    cam.leave(p2);
    assert_eq!(cam.read_capmts().0, 0, "a partner leave sends nothing");
    assert_eq!(cam.dev.capmt[0].version, version, "version untouched");
    assert_eq!(cam.join(p1, true), TablesResult::Ok, "no-op update");
    assert_eq!(
        cam.read_capmts().0,
        0,
        "the left streams still count, so nothing changed"
    );
    assert!(
        capmt_lists_pid(&before.unwrap(), cam.first_pid(p2)),
        "the CAM still runs the left program"
    );
}

// Ported from tests/test_ca.cpp:2174
#[test]
fn test_capmt_zap_is_one_send() {
    let mut cam = PackingCam::open();
    let p1 = cam.program(0x2201);
    let p2 = cam.program(0x2202);
    let p3 = cam.program(0x2203);
    let p4 = cam.program(0x2204);
    cam.join(p1, false);
    cam.join(p2, false);
    cam.join(p3, false);
    cam.read_capmts();
    cam.leave(p2);
    cam.join(p4, false);
    assert_eq!(
        cam.dev.capmt[0].other_id[0], p4,
        "the new program takes the left one's place"
    );
    let (n, last) = cam.read_capmts();
    let last = last.unwrap();
    assert_eq!(n, 1, "the zap is a single CAPMT");
    assert!(
        !capmt_lists_pid(&last, cam.first_pid(p2)),
        "the left program is dropped by that send"
    );
    assert!(
        capmt_lists_pid(&last, cam.first_pid(p1))
            && capmt_lists_pid(&last, cam.first_pid(p3))
            && capmt_lists_pid(&last, cam.first_pid(p4)),
        "the running programs and the new one are listed"
    );
    assert_eq!(cam.dev.capmt[0].sid, 0x2201, "program number kept");
}

// Ported from tests/test_ca.cpp:2202
#[test]
fn test_capmt_first_leave_keeps_program() {
    let mut cam = PackingCam::open();
    let p1 = cam.program(0x2301);
    let p2 = cam.program(0x2302);
    let p3 = cam.program(0x2303);
    cam.join(p1, false);
    cam.join(p2, false);
    cam.read_capmts();
    cam.leave(p1);
    assert_eq!(cam.read_capmts().0, 0, "the first program leaves lazily");
    let c = &cam.dev.capmt[0];
    assert!(
        !pmt_id_is_valid(c.pmt_id) && c.other_id[0] == p2,
        "the partner keeps its place"
    );
    assert_eq!(
        cam.join(p2, true),
        TablesResult::Ok,
        "an update without the first program in place"
    );
    assert_eq!(cam.read_capmts().0, 0, "and it changes nothing");
    cam.join(p3, false);
    assert_eq!(cam.read_capmts().0, 1, "a join rewrites once");
    assert_eq!(
        cam.dev.capmt[0].pmt_id, p3,
        "the join fills the first place"
    );
    assert_eq!(cam.dev.capmt[0].sid, 0x2301, "the program number is kept");
}

// Ported from tests/test_ca.cpp:2229
#[test]
fn test_capmt_last_leave_drops_kept() {
    let mut cam = PackingCam::open();
    let p1 = cam.program(0x2501);
    let p2 = cam.program(0x2502);
    cam.join(p1, false);
    cam.join(p2, false);
    cam.read_capmts();
    cam.leave(p2);
    cam.leave(p1);
    let (n, last) = cam.read_capmts();
    let last = last.unwrap();
    assert_eq!(n, 1, "the release is sent at once");
    assert!(
        capmt_lists_pid(&last, cam.first_pid(p1)) && !capmt_lists_pid(&last, cam.first_pid(p2)),
        "only the last program is released"
    );
    assert_eq!(last[11], CMD_ID_NOT_SELECTED, "as not selected");
    assert_eq!(cam.dev.capmt[0].pmt_count(), 0, "the CAPMT is free");
    assert!(
        cam.dev.capmt[0].sent_es.iter().all(Vec::is_empty),
        "nothing is kept for the next channel"
    );
}

// Ported from tests/test_ca.cpp:2254
#[test]
fn test_capmt_rejoin_takes_kept_place() {
    let mut cam = PackingCam::open();
    cam.dev.max_ca_pmt = 2;
    let p: Vec<i32> = (0..6).map(|i| cam.program(0x2801 + i)).collect();
    for &id in &p[..5] {
        assert_eq!(cam.join(id, false), TablesResult::Ok, "join");
    }
    let (c0, c1) = (&cam.dev.capmt[0], &cam.dev.capmt[1]);
    assert!(
        c0.has_pmt(p[2]) && c0.pmt_count() == 3 && c1.has_pmt(p[3]) && c1.pmt_count() == 2,
        "PMTs 0, 2, 4 in the first CAPMT, 1 and 3 in the second"
    );
    cam.read_capmts();
    cam.leave(p[3]);
    cam.leave(p[2]);
    assert_eq!(cam.read_capmts().0, 0, "both leave lazily");
    assert!(
        cam.dev.capmt[1].pmt_count() < cam.dev.capmt[0].pmt_count(),
        "the second CAPMT is now the least loaded"
    );
    assert_eq!(cam.join(p[2], false), TablesResult::Ok, "rejoin");
    assert!(cam.dev.capmt[0].has_pmt(p[2]), "back in its own CAPMT");
    assert!(
        !cam.dev.capmt[1].has_pmt(p[2]),
        "not in the least loaded one"
    );
    assert_eq!(
        cam.read_capmts().0,
        0,
        "the CAPMT is unchanged, nothing sent"
    );
    assert_eq!(cam.join(p[5], false), TablesResult::Ok, "new program");
    assert!(
        cam.dev.capmt[1].has_pmt(p[5]),
        "it goes to the least loaded"
    );
    let (n, last) = cam.read_capmts();
    assert_eq!(n, 1, "and rewrites that CAPMT once");
    assert!(
        !capmt_lists_pid(&last.unwrap(), cam.first_pid(p[3])),
        "dropping the left program it replaced"
    );
}

// Ported from tests/test_ca.cpp:2291 (minisatip PR #1518)
// Guards against over-suppression: a kept-place rejoin whose CAPMT changed
// (renumbered here) must still send once.
#[test]
fn test_capmt_changed_rejoin_still_sends() {
    let mut cam = PackingCam::open();
    let p1 = cam.program(0x2a01);
    let p2 = cam.program(0x2a02);
    cam.join(p1, false);
    cam.join(p2, false);
    cam.read_capmts();
    cam.leave(p2);
    assert_eq!(cam.read_capmts().0, 0, "the partner leaves lazily");

    cam.dev.capmt[0].sid = 0x2a0f;
    assert_eq!(cam.join(p2, false), TablesResult::Ok, "rejoin");
    assert_eq!(cam.dev.capmt[0].other_id[0], p2, "back in its kept place");
    let (n, last) = cam.read_capmts();
    assert_eq!(n, 1, "the changed CAPMT is sent once");
    let last = last.unwrap();
    assert_eq!(
        u16::from(last[1]) << 8 | u16::from(last[2]),
        0x2a0f,
        "with the new number"
    );
    assert!(
        capmt_lists_pid(&last, cam.first_pid(p1)) && capmt_lists_pid(&last, cam.first_pid(p2)),
        "both programs listed"
    );
}

// Ported from tests/test_ca.cpp:2292
#[test]
fn test_capmt_zap_back_keeps_places() {
    let mut cam = PackingCam::open();
    cam.dev.max_ca_pmt = 2;
    let ch1 = cam.program(0x2901);
    let ch2 = cam.program(0x2902);
    let ch3 = cam.program(0x2903);
    let ch4 = cam.program(0x2904);
    cam.join(ch1, false);
    cam.join(ch2, false);
    cam.join(ch3, false);
    let (c0, c1) = (&cam.dev.capmt[0], &cam.dev.capmt[1]);
    assert!(
        c0.has_pmt(ch1) && c0.has_pmt(ch3) && c1.has_pmt(ch2) && c1.pmt_count() == 1,
        "CAPMT 0 carries channels 1 and 3, CAPMT 1 channel 2"
    );
    cam.read_capmts();
    cam.leave(ch3);
    cam.join(ch4, false);
    assert!(
        cam.dev.capmt[1].has_pmt(ch4),
        "on a tie the CAPMT without kept places takes channel 4"
    );
    let (n, last) = cam.read_capmts();
    let last = last.unwrap();
    assert_eq!(n, 1, "the first zap is one send");
    assert!(
        capmt_lists_pid(&last, cam.first_pid(ch2)) && capmt_lists_pid(&last, cam.first_pid(ch4)),
        "on CAPMT 1, next to channel 2"
    );
    for _ in 0..2 {
        cam.leave(ch4);
        cam.join(ch3, false);
        assert!(cam.dev.capmt[0].has_pmt(ch3), "channel 3 back in CAPMT 0");
        cam.leave(ch3);
        cam.join(ch4, false);
        assert!(cam.dev.capmt[1].has_pmt(ch4), "channel 4 back in CAPMT 1");
        assert_eq!(cam.read_capmts().0, 0, "zapping back sends nothing");
    }
}

/// A CA that counts how often a PMT is sent to and released on it (`fake_ca_op` of test_ca.cpp:860).
#[derive(Default)]
struct ReleaseCountingCa {
    add_calls: usize,
    del_calls: usize,
}

impl CaPolicy for ReleaseCountingCa {
    fn join(
        &mut self,
        _dev: &mut CaDevice,
        _pmt_id: i32,
        _update: bool,
        _programs: &ProgramTable,
        _driver: &mut dyn CaDriver,
    ) -> TablesResult {
        self.add_calls += 1;
        TablesResult::Ok
    }

    fn leave(
        &mut self,
        _dev: &mut CaDevice,
        _pmt_id: i32,
        _programs: &ProgramTable,
        _driver: &mut dyn CaDriver,
    ) {
        self.del_calls += 1;
    }
}

// Ported from tests/test_ca.cpp:884 (no add_ca/del_ca: the reconciler owns its CA, so
// "add_ca did not publish the op" becomes "the reconciled CA got the PMT")
//
// pmt_add_caid() clears ca_mask to force a re-send. A PMT stopped before that
// re-send happens must still be released on the CA that holds it (#1442)
#[test]
fn test_close_pmt_after_caid_update() {
    let mut t = ProgramTable::new();
    let mut drv = RecordingDriver::default();
    let mut ca = Reconciler::new(ReleaseCountingCa::default(), CaDevice::new(5, 1, 0));

    let id = t.add(5, 0x2000, 0x2001);
    assert!(id >= 0, "could not create the PMT");
    t.get_mut(id).unwrap().add_caid(0x0664, 0x1F06, &[]);
    ca.reconcile(&[id], &t, &mut drv);
    assert_eq!(ca.policy.add_calls, 1, "add_ca did not publish the op");
    assert!(ca.is_sent(id, &t), "PMT was not sent to the CA");

    // a new CA descriptor turns up, the PMT is due for a re-send ...
    t.get_mut(id).unwrap().add_caid(0x06EE, 0x1F07, &[]);
    assert!(
        !ca.is_sent(id, &t),
        "pmt_add_caid() expected to clear ca_mask"
    );

    // ... but it is stopped before the re-send
    ca.policy.del_calls = 0;
    ca.reconcile(&[], &t, &mut drv);
    assert_eq!(
        ca.policy.del_calls, 1,
        "the CA holding the PMT was not told that it stopped"
    );
    assert!(
        ca.actual().is_empty(),
        "the CA registration was not cleared"
    );

    // nothing left to release
    ca.reconcile(&[], &t, &mut drv);
    assert_eq!(ca.policy.del_calls, 1, "the PMT was released twice");

    // The literal `ca_mask = 0` (`invalidate`) must be released the same way.
    ca.reconcile(&[id], &t, &mut drv);
    ca.invalidate(id);
    assert!(
        !ca.is_sent(id, &t),
        "invalidate() expected to clear ca_mask"
    );
    ca.reconcile(&[], &t, &mut drv);
    assert_eq!(
        ca.policy.del_calls, 2,
        "an invalidated PMT was not released"
    );
    assert!(
        ca.actual().is_empty(),
        "the CA registration was not cleared"
    );

    t.del(id);
}
