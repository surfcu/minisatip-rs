//! DDCI tests ported from tests/test_ddci.cpp.

use ms_ca::program::RawDescriptor;
use ms_ca::{Program, ProgramTable, SdtInfo, TablesResult};
use ms_ca_drivers::ddci::*;
use ms_psi::{CrcCheck, Pmt, Sdt};
use ms_ts::section::SectionAssembler;

// Ported from tests/test_ddci.cpp:70
fn ca_descriptor(caid: u16, capid: u16) -> RawDescriptor {
    let [c0, c1] = caid.to_be_bytes();
    let [p0, p1] = capid.to_be_bytes();
    RawDescriptor::from_bytes(&[0x09, 4, c0, c1, p0, p1])
}

// Ported from tests/test_ddci.cpp:80
fn create_pmt(
    t: &mut ProgramTable,
    ad: u16,
    sid: u16,
    pid1: u16,
    pid2: u16,
    caid1: u16,
    caid2: u16,
) -> i32 {
    let id = t.add(ad, sid, 1000);
    let p = t.get_mut(id).unwrap();
    p.pid = (id * 1000) as u16;
    p.add_stream(pid1, 2, false, true);
    p.add_stream(pid2, 6, true, false);
    p.add_caid(caid1, caid1, &[]);
    p.add_caid(caid2, caid2, &[]);
    // A CA descriptor on the second stream, to check it reaches the PMT
    p.streams[1].descriptors.push(ca_descriptor(0x0B00, 0x0573));
    id
}

fn cam(initialized: bool) -> CamInfo {
    CamInfo {
        enabled: true,
        initialized,
        ..CamInfo::default()
    }
}

/// A DDCI device as the tests build it: `{}`, enabled, slots memset to -1.
fn device(id: i32, max_channels: usize) -> DdciDevice {
    let mut d = DdciDevice::new(id, max_channels);
    d.clear_slots();
    d
}

fn prog(t: &ProgramTable, id: i32) -> Program {
    t.get(id).unwrap().clone()
}

fn channel(sid: u16, ddcis: &[u8], locked: bool) -> Channel {
    Channel {
        ddci: ddcis.to_vec(),
        sid,
        name: String::new(),
        locked,
    }
}

// Ported from tests/test_ddci.cpp:102
#[test]
fn test_channels() {
    let dir = std::env::temp_dir().join(format!("ms-ddci-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("ddci.conf");
    let _ = std::fs::remove_file(&path);
    let mut x = Ddci::new();
    x.channels.insert(200, channel(200, &[1], false));
    x.channels.insert(300, channel(300, &[1, 2], false));
    x.channels.insert(400, channel(400, &[], false));
    x.save_channels(&path).unwrap();
    x.channels.clear();
    x.load_channels(&path).unwrap();
    assert_eq!(
        x.channels[&200].sid, 200,
        "Saved SID 200 not found in table"
    );
    assert_eq!(
        x.channels[&300].sid, 300,
        "Saved SID 300 not found in table"
    );
    assert_eq!(
        x.channels[&400].sid, 400,
        "Saved SID 400 not found in table"
    );
    assert_eq!(x.channels[&300].ddci, [1, 2]);
    assert_eq!(x.channels.len(), 3, "Expected two channels after loading");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Adapter 8 tuning, DDCI 0 with CAM 0 ready for CAID 0x100.
fn one_ddci(max_channels: usize) -> (Ddci, ProgramTable) {
    let mut x = Ddci::new();
    x.create_adapter(8);
    x.create_adapter(0);
    x.devices.insert(0, device(0, max_channels));
    x.cams.insert(0, cam(true));
    x.add_caid_mask(0, 0x100, 0xFFFF);
    (x, ProgramTable::new())
}

// Ported from tests/test_ddci.cpp:139
#[test]
fn test_ddci_channel_count() {
    let (mut x, mut t) = one_ddci(2);
    let pmt0 = create_pmt(&mut t, 8, 600, 601, 602, 0x100, 0x100);
    let noslot = create_pmt(&mut t, 8, 700, 701, 702, 0x100, 0x100);
    assert_eq!(
        x.process_pmt(8, &prog(&t, pmt0), false),
        TablesResult::Ok,
        "PMT 0 expected to get a DDCI slot"
    );
    assert_eq!(x.devices[&0].channels, 1, "expected 1 running channel");
    assert_eq!(
        x.process_pmt(8, &prog(&t, pmt0), true),
        TablesResult::Ok,
        "re-send of PMT 0 expected to succeed"
    );
    assert_eq!(
        x.process_pmt(8, &prog(&t, pmt0), true),
        TablesResult::Ok,
        "re-send of PMT 0 expected to succeed"
    );
    assert_eq!(
        x.devices[&0].channels, 1,
        "re-sends must not count as new channels"
    );
    t.get_mut(noslot).unwrap().pid = t.get(pmt0).unwrap().pid;
    x.del_pmt(8, &prog(&t, noslot));
    assert_eq!(
        x.devices[&0].channels, 1,
        "deleting a PMT without a slot must not free a channel"
    );
    x.del_pmt(8, &prog(&t, pmt0));
    assert_eq!(
        x.devices[&0].channels, 0,
        "expected 0 running channels after delete"
    );
}

// Ported from tests/test_ddci.cpp:198
fn section_pcr_pid(x: &mut Ddci, pmt: &Program) -> i32 {
    let psi = x.create_pmt_for_slot(0, 0, pmt, 512);
    (i32::from(psi[9] & 0x1F) << 8) | i32::from(psi[10])
}

fn mapped(x: &Ddci, ad: u16, pid: u16) -> bool {
    x.get_pid_mapping_allddci(ad, pid).is_some()
}

fn ddci_pid(x: &Ddci, ad: u16, pid: u16) -> i32 {
    x.get_pid_mapping_allddci(ad, pid).unwrap().ddci_pid
}

// Ported from tests/test_ddci.cpp:206
#[test]
fn test_update_resend_keeps_slot() {
    let (mut x, mut t) = one_ddci(1);
    let pmt0 = create_pmt(&mut t, 8, 600, 601, 602, 0x100, 0x100);
    t.get_mut(pmt0).unwrap().pcr_pid = 601;
    assert_eq!(
        x.process_pmt(8, &prog(&t, pmt0), false),
        TablesResult::Ok,
        "slot expected"
    );
    assert_eq!(x.devices[&0].channels, 1, "expected 1 running channel");
    assert!(mapped(&x, 8, 601), "601 should be mapped");
    assert!(mapped(&x, 8, 602), "602 should be mapped");
    assert_eq!(
        x.devices[&0].pmt[0].pcr_pid,
        ddci_pid(&x, 8, 601),
        "slot PCR should be the 601 mapping"
    );
    let pcr = x.devices[&0].pmt[0].pcr_pid;
    assert_eq!(
        section_pcr_pid(&mut x, &prog(&t, pmt0)),
        pcr,
        "section PCR should match the slot"
    );

    t.get_mut(pmt0).unwrap().remove_stream(602);
    assert_eq!(
        x.process_pmt(8, &prog(&t, pmt0), true),
        TablesResult::Ok,
        "re-send on the full device expected to succeed"
    );
    assert!(!mapped(&x, 8, 602), "602 should be unmapped");
    assert!(mapped(&x, 8, 601), "601 should stay mapped");
    assert!(mapped(&x, 8, 1), "CAT should stay mapped");
    assert!(mapped(&x, 8, 20), "TDT should stay mapped");
    assert!(mapped(&x, 8, 256), "ECM should stay mapped");
    let pmt_pid = t.get(pmt0).unwrap().pid;
    if pmt_pid < 8192 {
        assert!(mapped(&x, 8, pmt_pid), "PMT pid should stay mapped");
    }
    assert_eq!(x.devices[&0].pmt[0].id, pmt0, "slot should be kept");
    assert_eq!(
        x.devices[&0].channels, 1,
        "re-send must not count as new channel"
    );
    assert_eq!(
        x.devices[&0].pmt[0].pcr_pid,
        ddci_pid(&x, 8, 601),
        "re-send must keep the slot PCR pid"
    );
    let pcr = x.devices[&0].pmt[0].pcr_pid;
    assert_eq!(
        section_pcr_pid(&mut x, &prog(&t, pmt0)),
        pcr,
        "section PCR should match the slot"
    );

    // replace the first stream pid: the stream lookup misses, the slot scan must not
    let p = t.get_mut(pmt0).unwrap();
    p.remove_stream(601);
    p.add_stream(603, 2, false, true);
    p.pcr_pid = 603;
    assert_eq!(
        x.process_pmt(8, &prog(&t, pmt0), true),
        TablesResult::Ok,
        "re-send after replacing the first pid expected to succeed"
    );
    assert!(mapped(&x, 8, 603), "603 should be mapped");
    assert!(!mapped(&x, 8, 601), "601 should be unmapped");
    assert_eq!(x.devices[&0].pmt[0].id, pmt0, "slot should still be kept");
    assert_eq!(
        x.devices[&0].pmt[0].pcr_pid,
        ddci_pid(&x, 8, 603),
        "slot PCR should follow the new PCR pid"
    );
    let pcr = x.devices[&0].pmt[0].pcr_pid;
    assert_eq!(
        section_pcr_pid(&mut x, &prog(&t, pmt0)),
        pcr,
        "section PCR should match the slot"
    );

    // a pid shared by two PMTs stays mapped until its last user is gone
    let pmt1 = create_pmt(&mut t, 8, 701, 702, 703, 0x100, 0x100);
    x.add_pid_mapping_table(8, 603, pmt1, 0, false);
    t.get_mut(pmt0).unwrap().remove_stream(603);
    assert_eq!(
        x.process_pmt(8, &prog(&t, pmt0), true),
        TablesResult::Ok,
        "re-send dropping the shared pid expected to succeed"
    );
    assert!(mapped(&x, 8, 603), "shared 603 should stay mapped");
    x.del_pmt_mapping_table(0, 8, pmt1);
    assert!(
        !mapped(&x, 8, 603),
        "603 should be unmapped after its last user"
    );

    // CAT-listed EMM pids are never swept; an untracked pid is
    x.add_pid_mapping_table(8, 700, pmt0, 0, true);
    x.devices.get_mut(&0).unwrap().emm_pids.insert((8, 700));
    x.add_pid_mapping_table(8, 701, pmt0, 0, true);
    assert_eq!(
        x.process_pmt(8, &prog(&t, pmt0), true),
        TablesResult::Ok,
        "re-send with EMM expected to succeed"
    );
    assert!(mapped(&x, 8, 700), "EMM pid should stay mapped");
    assert!(!mapped(&x, 8, 701), "untracked pid should be swept");

    x.del_pmt(8, &prog(&t, pmt0));
    assert_eq!(
        x.devices[&0].channels, 0,
        "expected 0 running channels after delete"
    );
}

// Ported from tests/test_ddci.cpp:330
#[test]
fn test_ddci_update_version_follows_content() {
    let (mut x, mut t) = one_ddci(1);
    let pmt0 = create_pmt(&mut t, 8, 600, 601, 602, 0x100, 0x100);
    assert_eq!(
        x.process_pmt(8, &prog(&t, pmt0), false),
        TablesResult::Ok,
        "slot expected"
    );
    let prev = x.create_pmt_for_slot(0, 0, &prog(&t, pmt0), 1500);
    assert!(!prev.is_empty(), "PMT section expected");
    let ver = x.devices[&0].pmt[0].ver;
    assert_eq!(
        x.process_pmt(8, &prog(&t, pmt0), true),
        TablesResult::Ok,
        "identical re-send expected to succeed"
    );
    let psi = x.create_pmt_for_slot(0, 0, &prog(&t, pmt0), 1500);
    assert_eq!(
        psi.len(),
        prev.len(),
        "identical re-send must not resize the section"
    );
    assert_eq!(
        x.devices[&0].pmt[0].ver, ver,
        "identical re-send must keep the version"
    );
    assert_eq!(psi, prev, "identical re-send must keep the bytes");
    t.get_mut(pmt0).unwrap().remove_stream(602);
    assert_eq!(
        x.process_pmt(8, &prog(&t, pmt0), true),
        TablesResult::Ok,
        "changed re-send expected to succeed"
    );
    x.create_pmt_for_slot(0, 0, &prog(&t, pmt0), 1500);
    assert_eq!(
        x.devices[&0].pmt[0].ver,
        (ver + 1) & 0xF,
        "changed re-send must bump the version once"
    );
}

// Ported from tests/test_ddci.cpp:395
#[test]
fn test_add_del_pmt() {
    let mut x = Ddci::new();
    let mut t = ProgramTable::new();
    for a in [8, 0, 1] {
        x.create_adapter(a);
    }
    let pmt0 = create_pmt(&mut t, 8, 100, 101, 102, 0x100, 0x1800);
    let pmt1 = create_pmt(&mut t, 8, 200, 201, 202, 0x100, 0x500);
    let pmt2 = create_pmt(&mut t, 8, 300, 301, 302, 0x500, 0x100);
    let pmt3 = create_pmt(&mut t, 8, 400, 401, 402, 0x600, 0x601);
    let pmt4 = create_pmt(&mut t, 8, 500, 501, 502, 0x500, 0x500);
    x.devices.insert(0, device(0, 1));
    x.devices.insert(1, device(1, 1));
    // pmt4's saved channel has no DDCI, though its CAID matches
    let sid4 = t.get(pmt4).unwrap().sid;
    x.channels.insert(sid4, channel(sid4, &[], false));
    x.add_caid_mask(0, 0x100, 0xFFFF);
    x.add_caid_mask(1, 0x500, 0xFFFF);
    x.cams.insert(0, cam(false));
    x.cams.insert(1, cam(true));
    assert_eq!(
        x.process_pmt(8, &prog(&t, pmt3), false),
        TablesResult::Retry,
        "DDCI not ready, expected retry"
    );
    x.cams.get_mut(&0).unwrap().initialized = true;
    assert_eq!(
        x.process_pmt(8, &prog(&t, pmt3), false),
        TablesResult::NoRetry,
        "DDCI ready, expected no retry"
    );
    assert_eq!(
        x.process_pmt(8, &prog(&t, pmt4), false),
        TablesResult::NoRetry,
        "Channel not assigned to any DDCI adapter, expected no retry"
    );
    assert_eq!(
        x.process_pmt(8, &prog(&t, pmt0), false),
        TablesResult::Ok,
        "DDCI matching DD 0"
    );
    assert_eq!(x.devices[&0].pmt[0].id, 0, "PMT 0 using DDCI 0");
    assert_eq!(
        x.process_pmt(8, &prog(&t, pmt1), false),
        TablesResult::Ok,
        "DDCI matching DD 1"
    );
    assert_eq!(x.devices[&1].pmt[0].id, 1, "PMT 1 using DDCI 1");
    x.devices.get_mut(&0).unwrap().max_channels = 2;
    x.devices.get_mut(&1).unwrap().max_channels = 2;

    let sid2 = t.get(pmt2).unwrap().sid;
    x.channels.insert(sid2, channel(sid2, &[1], true));
    let p = t.get_mut(pmt2).unwrap();
    p.add_stream(0xFF, 2, false, true);
    p.add_caid(0x502, 0xFE, &[]);
    assert_eq!(
        x.process_pmt(8, &prog(&t, pmt2), false),
        TablesResult::Ok,
        "DDCI matching DD 0 for second PMT"
    );
    assert_eq!(x.devices[&1].pmt[1].id, 2, "PMT 2 using DDCI 1");

    let mut ec = 0;
    let mut pmt_pids = [0; 8];
    for k in 0..2 {
        for m in x.devices[&k].mapping.values() {
            ec += 1;
            for &e in &m.pmt {
                pmt_pids[e as usize] += 1;
            }
        }
    }
    assert!(ec > 3, "Deleted Pids from the previously added PMT");
    assert!(pmt_pids[0] > 0, "PMT 0 expected to have pids");
    assert!(pmt_pids[1] > 0, "PMT 1 expected to have pids");
    assert!(pmt_pids[2] > 0, "PMT 2 expected to have pids");
    assert!(
        mapped(&x, 8, 0xFF),
        "Newly added pid not found in mapping table"
    );
    assert!(
        mapped(&x, 8, 0xFE),
        "Newly added capid not found in mapping table"
    );

    x.del_pmt(8, &prog(&t, pmt1));
    x.del_pmt(8, &prog(&t, pmt0));
    x.del_pmt(8, &prog(&t, pmt2));
    let ec = x.devices[&0].mapping.len() + x.devices[&1].mapping.len();
    assert_eq!(ec, 0, "No pid should be enabled");
}

fn ts_packet(pid: u16) -> [u8; 188] {
    let mut p = [0u8; 188];
    p[0] = 0x47;
    set_pid_ts(&mut p, pid);
    p
}

// Ported from tests/test_ddci.cpp:505
#[test]
fn test_copy_ts_from_ddci() {
    let mut x = Ddci::new();
    x.devices.insert(0, DdciDevice::new(0, 0));
    x.create_adapter(1);
    x.create_adapter(0);
    let mut buf2 = vec![0u8; 188 * 10];
    buf2[0] = 0x47;
    set_pid_ts(&mut buf2, 0x1FFF);
    let mut ad = TsBuffer {
        id: 1,
        buf: buf2,
        rlen: 188,
    };
    let pid = 1000;
    x.add_pid_mapping_table(2, pid, 9, 0, false); // forcing mapping to a different pid
    x.add_pid_mapping_table(1, pid, 0, 0, false);
    let new_pid = ddci_pid(&x, 1, pid);
    assert_eq!(new_pid, 1001, "Unexpected pid found after conflict");
    x.push_from_cam(0, &ts_packet(new_pid as u16));
    let mut mapping = [0u16; 8192];
    mapping[new_pid as usize] = pid;
    x.push_ts_to_adapter(0, &mut ad, &mapping);
    assert_eq!(
        pid_from_ts(&ad.buf),
        pid,
        "found pid {} expected {pid}",
        pid_from_ts(&ad.buf)
    );
}

/// Captures what `writev` received (`xwritev`, `xwritev_last`).
#[derive(Default)]
struct CaptureWriter {
    calls: Vec<Vec<Vec<u8>>>,
}

impl CiWriter for CaptureWriter {
    fn writev(&mut self, iov: &[&[u8]]) -> isize {
        self.calls.push(iov.iter().map(|s| s.to_vec()).collect());
        iov.iter().map(|s| s.len()).sum::<usize>() as isize
    }
}

/// DDCI with id 2 in slot 0, adapters 0..=2, a buffer of unmapped pid 2121.
fn ts_fixture() -> (Ddci, TsBuffer) {
    let mut x = Ddci::new();
    x.devices.insert(0, DdciDevice::new(2, 0));
    for a in [0, 1, 2] {
        x.create_adapter(a);
    }
    let mut buf = vec![0u8; 188 * 10];
    for i in (0..buf.len()).step_by(188) {
        buf[i] = 0x47;
        set_pid_ts(&mut buf[i..], 2121);
    }
    let rlen = buf.len() - 188;
    (x, TsBuffer { id: 1, buf, rlen })
}

// Ported from tests/test_ddci.cpp:589
#[test]
fn test_ddci_process_ts_null_tail() {
    let (mut x, mut ad) = ts_fixture();
    let mut t = ProgramTable::new();
    x.add_pid_mapping_table(1, 1000, 0, 0, false);
    set_pid_ts(&mut ad.buf, 1000);
    let d = x.devices.get_mut(&0).unwrap();
    d.read_index[1] = 188;
    d.read_index[2] = 188;
    d.fifo.write_index = 188;
    let mut w = CaptureWriter::default();
    x.process_ts(&mut ad, 0, &mut t, &mut w);
    assert_eq!(w.calls.len(), 1, "no writev called");
    let last = w.calls[0].last().unwrap();
    assert_eq!(
        last.len(),
        188,
        "expected a single 188 byte packet last, got {}",
        last.len()
    );
    assert!(
        last[0] == 0x47 && pid_from_ts(last) == 8191,
        "last packet is not a null packet"
    );
    assert_eq!(
        last[3] & 0x30,
        0x10,
        "null packet must carry payload only, got {:02X}",
        last[3]
    );
}

// Ported from tests/test_ddci.cpp:640
#[test]
fn test_ddci_process_ts() {
    let (mut x, mut ad) = ts_fixture();
    let mut t = ProgramTable::new();
    x.add_pid_mapping_table(5, 1000, 0, 0, false);
    x.add_pid_mapping_table(5, 2000, 0, 0, false);
    let new_pid = x.add_pid_mapping_table(1, 1000, 0, 0, false) as u16;
    let new_pid2 = x.add_pid_mapping_table(1, 2000, 0, 0, false) as u16;
    set_pid_ts(&mut ad.buf, 1000);
    set_pid_ts(&mut ad.buf[2 * 188..], 2000);
    let d = x.devices.get_mut(&0).unwrap();
    // Prevents resetting read_index to write_index
    d.read_index[1] = 188;
    d.read_index[2] = 188;
    d.fifo.write_index = 188;
    let mut fifo = Vec::new();
    for p in [new_pid, new_pid2, new_pid2] {
        fifo.extend_from_slice(&ts_packet(p));
    }
    x.push_from_cam(0, &fifo);
    let mut w = CaptureWriter::default();
    x.process_ts(&mut ad, 0, &mut t, &mut w);
    assert_eq!(w.calls.len(), 1, "no writev called");
    let iov = &w.calls[0];
    assert!(
        iov.len() >= 2,
        "writev did not receive proper arguments, expected 2, got {}",
        iov.len()
    );
    assert_eq!(
        pid_from_ts(&iov[0]),
        new_pid,
        "writev did not receive proper TS"
    );
    assert_eq!(
        pid_from_ts(&ad.buf),
        1000,
        "expected pid 1000 in the adapter buffer"
    );
    assert_eq!(
        pid_from_ts(&ad.buf[2 * 188..]),
        2000,
        "expected pid 2000 in the adapter buffer"
    );
}

/// CI adapter 8 (DDCI 8, max 2 channels) with CAM 0 ready for CAID 0x100.
fn ci_fixture(tuners: &[u16]) -> (Ddci, ProgramTable) {
    let mut x = Ddci::new();
    x.create_adapter(8);
    for &a in tuners {
        x.create_adapter(a);
    }
    x.devices.insert(8, device(8, 2));
    x.add_caid_mask(8, 0x100, 0xFFFF);
    x.cams.insert(0, cam(true));
    (x, ProgramTable::new())
}

// Ported from tests/test_ddci.cpp:698
#[test]
fn test_psi_pids_survive_a_channel_change() {
    let (mut x, mut t) = ci_fixture(&[2, 4]);
    // post_tune() adds pid 0 as a default pid, so it outlives the DDCI mappings
    x.post_tune_ci(8);
    let pmt_a = create_pmt(&mut t, 2, 600, 601, 602, 0x100, 0x100);
    let pmt_b = create_pmt(&mut t, 4, 700, 701, 702, 0x100, 0x100);
    t.get_mut(pmt_a).unwrap().pid = 600;
    t.get_mut(pmt_b).unwrap().pid = 700;
    assert_eq!(
        x.process_pmt(2, &prog(&t, pmt_a), false),
        TablesResult::Ok,
        "the first PMT should be registered"
    );
    assert_eq!(
        x.process_pmt(4, &prog(&t, pmt_b), false),
        TablesResult::Ok,
        "the second PMT should be registered"
    );
    assert!(
        x.find_pid(8, 0).is_some(),
        "pid 0 should be on the CI adapter"
    );
    x.del_pmt(2, &prog(&t, pmt_a));
    assert!(
        x.find_pid(8, 0).is_some_and(|p| !p.deleted),
        "pid 0 was lost with the first PMT, the CI adapter can no longer read the PAT it generates"
    );
    x.del_pmt(4, &prog(&t, pmt_b));
}

fn assemble(packets: &[u8]) -> Option<Vec<u8>> {
    let mut a = SectionAssembler::new(true);
    let mut out = None;
    for p in packets.chunks(188) {
        if let Some(s) = a.assemble_packet(p) {
            out = Some(s.to_vec());
        }
    }
    out
}

// Ported from tests/test_ddci.cpp:765
#[test]
fn test_create_pat() {
    let mut x = Ddci::new();
    let mut t = ProgramTable::new();
    x.create_adapter(0);
    let pid = 4096;
    let pmt_id = t.add(0, 0x66, pid);
    x.devices.insert(0, DdciDevice::new(0, 1));
    x.add_pid_mapping_table(9, pid, 0, 0, false);
    let dpid = x.add_pid_mapping_table(0, pid, 0, 0, false);
    x.devices.get_mut(&0).unwrap().pmt[0].id = pmt_id;
    let psi = x.create_pat(0, &t);
    let mut packet = [0u8; 188];
    let mut cc = 1;
    buffer_to_ts(&mut packet, &psi, &mut cc, 0);
    assert!(assemble(&packet).is_some(), "assemble_packet failed");
    let new_sid = u16::from_be_bytes([packet[17], packet[18]]);
    let new_pid = u16::from_be_bytes([packet[19], packet[20]]) & 0x1FFF;
    assert_eq!(
        i32::from(new_pid),
        dpid,
        "PAT pid {new_pid} != mapping table pid {dpid}"
    );
    assert_eq!(new_sid, 0x66, "PAT sid {new_sid} != pmt sid");
}

// Ported from tests/test_ddci.cpp:806
#[test]
fn test_create_sdt() {
    let mut x = Ddci::new();
    let mut t = ProgramTable::new();
    x.create_adapter(0);
    x.devices.insert(0, device(0, 2));
    let id1 = t.add(0, 0x100, 4096);
    let id2 = t.add(0, 0x101, 4097);
    x.add_pid_mapping_table(0, 4096, id1, 0, false);
    x.add_pid_mapping_table(0, 4097, id2, 0, false);
    x.devices.get_mut(&0).unwrap().pmt[0].id = id1;
    x.devices.get_mut(&0).unwrap().pmt[1].id = id2;
    let sdt = x.create_sdt(0, &t);
    let mut packet = [0u8; 188];
    let mut cc = 1;
    buffer_to_ts(&mut packet, &sdt, &mut cc, 17);
    assert!(assemble(&packet).is_some(), "assemble_packet failed");
    for (off, sid) in [(16, 0x100), (21, 0x101)] {
        assert_eq!(
            u16::from_be_bytes([packet[off], packet[off + 1]]),
            sid,
            "SDT SID != {sid:#x}"
        );
        assert_eq!(packet[off + 3] >> 5, 4, "SDT SID running_status != 4");
        assert_ne!(packet[off + 3] & (1 << 4), 0, "SDT SID free_CA_mode != 1");
    }
}

/// Real SDT section of test_ddci.cpp:904: tsid 7900, onid 156.
fn real_sdt_section(sid1: u16, sid2: u16) -> Vec<u8> {
    let mut b = vec![0x42, 0xF0, 0x00];
    b.extend_from_slice(&7900u16.to_be_bytes());
    b.extend_from_slice(&[0xC1, 0, 0]);
    b.extend_from_slice(&156u16.to_be_bytes());
    b.push(0xFF);
    for (sid, eit, status, name) in [
        (sid1, 0x03, (4 << 5) | (1 << 4), "RTL HD"),
        (sid2, 0x00, 4 << 5, "Test SD"),
    ] {
        b.extend_from_slice(&sid.to_be_bytes());
        b.push(eit);
        b.push(status);
        let dlen = b.len();
        b.push(0);
        b.push(0x48);
        let sdlen = b.len();
        b.push(0);
        b.push(if sid == sid1 { 0x19 } else { 0x01 });
        b.push(4);
        b.extend_from_slice(b"Pyur");
        b.push(name.len() as u8);
        b.extend_from_slice(name.as_bytes());
        b[sdlen] = (b.len() - sdlen - 1) as u8;
        b[dlen] = (b.len() - dlen - 1) as u8;
    }
    let section_length = b.len() - 3 + 4;
    b[1] |= ((section_length >> 8) & 0x0F) as u8;
    b[2] = section_length as u8;
    let crc = ms_ts::crc::crc32_mpeg2(&b);
    b.extend_from_slice(&crc.to_be_bytes());
    b
}

/// `process_sdt` glue: copies parsed SDT data into the adapter's programs.
fn apply_sdt(t: &mut ProgramTable, adapter: u16, sdt: &Sdt) {
    for s in &sdt.services {
        let Some(id) = t.first_for_sid(adapter, s.service_id).map(|p| p.id) else {
            continue;
        };
        let p = t.get_mut(id).unwrap();
        p.sdt = Some(SdtInfo {
            service_type: s.service_type.unwrap_or(0),
            running_status: s.running_status,
            ca_mode: u8::from(s.free_ca_mode),
            eit_schedule: u8::from(s.eit_schedule),
            eit_pf: u8::from(s.eit_present_following),
            tsid: sdt.transport_stream_id,
            onid: sdt.original_network_id,
        });
        p.name =
            String::from_utf8_lossy(s.service_name.as_deref().unwrap_or_default()).into_owned();
        p.provider =
            String::from_utf8_lossy(s.provider_name.as_deref().unwrap_or_default()).into_owned();
    }
}

// Ported from tests/test_ddci.cpp:879 (process_sdt replaced by ms-psi parsing plus glue)
#[test]
fn test_create_sdt_with_real_data() {
    let mut x = Ddci::new();
    let mut t = ProgramTable::new();
    x.create_adapter(2);
    x.devices.insert(0, device(0, 2));
    let (sid1, sid2) = (0x100, 0x101);
    let id1 = t.add(2, sid1, 4096);
    let id2 = t.add(2, sid2, 4097);
    x.add_pid_mapping_table(2, 4096, id1, 0, false);
    x.add_pid_mapping_table(2, 4097, id2, 0, false);
    x.devices.get_mut(&0).unwrap().pmt[0].id = id1;
    x.devices.get_mut(&0).unwrap().pmt[1].id = id2;
    let section = real_sdt_section(sid1, sid2);
    let sdt = Sdt::parse(&section, CrcCheck::Verify).expect("process_sdt failed");
    apply_sdt(&mut t, 2, &sdt);
    let p1 = t.get(id1).unwrap();
    let s1 = p1.sdt.expect("PMT1 has no SDT data");
    assert_eq!(s1.service_type, 0x19, "PMT1 service_type != 0x19");
    assert_eq!(s1.running_status, 4, "PMT1 running_status != 4");
    assert_eq!(s1.ca_mode, 1, "PMT1 ca_mode != 1");
    assert!(
        s1.eit_schedule == 1 && s1.eit_pf == 1,
        "PMT1 EIT flags != 1"
    );
    assert!(
        s1.tsid == 7900 && s1.onid == 156,
        "PMT1 tsid/onid != 7900/156"
    );
    assert_eq!(p1.name, "RTL HD", "PMT1 name != RTL HD");

    let sdt = x.create_sdt(0, &t);
    assert_eq!(
        x.devices[&0].sdt_ver, 1,
        "SDT version not bumped on new data"
    );
    assert_eq!(
        x.devices[&0].ver, 0,
        "PAT version bumped by SDT-only change"
    );
    assert_eq!(
        u16::from_be_bytes([sdt[4], sdt[5]]),
        7900,
        "generated tsid != 7900"
    );
    assert_eq!(
        u16::from_be_bytes([sdt[9], sdt[10]]),
        156,
        "generated onid != 156"
    );
    let s = &sdt[12..];
    assert_eq!(u16::from_be_bytes([s[0], s[1]]), sid1, "generated SID 1");
    assert_eq!(s[2], 0x03, "generated EIT flags 1 != 0x03");
    assert!(
        (s[3] >> 5) == 4 && ((s[3] >> 4) & 1) == 1,
        "generated running/CA 1 != 4/1"
    );
    let dl1 = (usize::from(s[3] & 0xF) << 8) | usize::from(s[4]);
    assert!(
        dl1 > 0 && s[5] == 0x48,
        "generated service_descriptor 1 missing"
    );
    assert_eq!(s[7], 0x19, "generated service_type 1 != 0x19");
    let s = &s[5 + dl1..];
    assert_eq!(u16::from_be_bytes([s[0], s[1]]), sid2, "generated SID 2");
    assert_eq!(s[2], 0x00, "generated EIT flags 2 != 0x00");
    assert!(
        (s[3] >> 5) == 4 && ((s[3] >> 4) & 1) == 0,
        "generated running/CA 2 != 4/0"
    );
    let dl2 = (usize::from(s[3] & 0xF) << 8) | usize::from(s[4]);
    assert!(
        dl2 > 0 && s[5] == 0x48,
        "generated service_descriptor 2 missing"
    );
    let gen_len = (usize::from(sdt[2] & 0xF) << 8) | usize::from(sdt[3]);
    assert_eq!(
        gen_len + 3,
        sdt.len() - 1,
        "generated section_length mismatch"
    );
    let mut packet = [0u8; 188 * 4];
    let mut cc = 1;
    let n = buffer_to_ts(&mut packet, &sdt, &mut cc, 17);
    assert!(
        assemble(&packet[..n as usize]).is_some(),
        "generated SDT CRC check failed"
    );
    x.create_sdt(0, &t);
    assert_eq!(
        x.devices[&0].sdt_ver, 1,
        "SDT version bumped without changes"
    );
}

// Ported from tests/test_ddci.cpp:1028
#[test]
fn test_create_pmt() {
    let mut x = Ddci::new();
    let mut t = ProgramTable::new();
    x.devices.insert(0, DdciDevice::new(0, 0));
    let pid = 1023;
    let capid = 0x100;
    let dpid = x.add_pid_mapping_table(0, pid, 0, 0, false);
    let dcapid = x.add_pid_mapping_table(0, capid, 0, 0, false);
    let id = create_pmt(&mut t, 0, 0x66, pid, 0x55, 0x100, 0x500);
    x.devices.get_mut(&0).unwrap().pmt[0].id = id;
    let p = t.get_mut(id).unwrap();
    p.pid = pid - 1;
    p.version = 0;
    p.name = "TEST CHANNEL HD".into();
    let mut dp = DdciPmt {
        ver: 0,
        pcr_pid: 8191,
        ..DdciPmt::default()
    };
    let psi = x.create_pmt(0, &prog(&t, id), 188, &mut dp);
    let mut packet = [0u8; 188];
    let mut cc = 1;
    buffer_to_ts(&mut packet, &psi, &mut cc, 0x63);
    assert!(assemble(&packet).is_some(), "Assemble packet failed");
    let new_pid = u16::from_be_bytes([packet[30], packet[31]]) & 0x1FFF;
    let new_capid = u16::from_be_bytes([packet[21], packet[22]]) & 0x1FFF;
    assert_eq!(
        i32::from(new_pid),
        dpid,
        "PMT stream pid != mapping table pid"
    );
    assert_eq!(
        i32::from(new_capid),
        dcapid,
        "PMT PSI pid != mapping table pid"
    );
    assert_eq!(packet[38], 6, "es info length mismatch");
    assert_eq!(packet[39], 0x09, "descriptor type mismatch");
    assert_eq!(packet[40], 4, "descriptor length mismatch");
    assert_eq!(
        u16::from_be_bytes([packet[41], packet[42]]),
        0x0B00,
        "descriptor CA system mismatch"
    );
    assert_eq!(
        (u16::from(packet[43] & 0x1F) << 8) | u16::from(packet[44]),
        0x0573,
        "descriptor CA PID mismatch"
    );
    // process_pmt on the generated section: the ms-psi parser
    let parsed = Pmt::parse(&psi[1..], CrcCheck::Verify).expect("generated PMT parses");
    let src = t.get(id).unwrap();
    assert_eq!(
        src.streams.len(),
        parsed.streams.len(),
        "Number of streampids does not matches between generated PMT and read PMT"
    );
    assert_eq!(
        src.caids.len(),
        parsed.program_info.iter().filter(|d| d.is_ca()).count(),
        "Number of caids does not matches between generated PMT and read PMT"
    );
}

// Ported from tests/test_ddci.cpp:1120
#[test]
fn test_create_pmt_maps_es_ecm_pids() {
    let mut x = Ddci::new();
    let mut t = ProgramTable::new();
    x.devices.insert(0, DdciDevice::new(0, 0));
    let id = create_pmt(&mut t, 0, 0x66, 1023, 0x55, 0x100, 0x500);
    // occupy ddci pid 0x0573 from another adapter so the ECM pid remaps
    x.add_pid_mapping_table(1, 0x0573, 99, 0, false);
    let mapped = x.add_pid_mapping_table(0, 0x0573, id, 0, false);
    assert!(
        mapped >= 0 && mapped != 0x0573,
        "ECM pid should be remapped"
    );
    let mut dp = DdciPmt {
        ver: 0,
        pcr_pid: 8191,
        ..DdciPmt::default()
    };
    let psi = x.create_pmt(0, &prog(&t, id), 512, &mut dp);
    let b = &psi[1..];
    let pi_len = (usize::from(b[10] & 0xF) << 8) | usize::from(b[11]);
    let end = psi.len() - 1 - 4;
    let mut es = 12 + pi_len;
    let mut found = 0;
    while es + 5 <= end {
        let es_len = (usize::from(b[es + 3] & 0xF) << 8) | usize::from(b[es + 4]);
        let mut dd = es + 5;
        let dd_end = dd + es_len;
        while dd + 2 <= dd_end && dd + 2 + usize::from(b[dd + 1]) <= dd_end {
            if b[dd] == 0x09 && b[dd + 1] >= 4 {
                let capid = (i32::from(b[dd + 4] & 0x1F) << 8) | i32::from(b[dd + 5]);
                assert_eq!(capid, mapped, "ES ECM pid should be mapped");
                found += 1;
            }
            dd += 2 + usize::from(b[dd + 1]);
        }
        es = dd_end;
    }
    assert_eq!(found, 1, "expected one ES CA descriptor");
}

// Ported from tests/test_ddci.cpp:1168
#[test]
fn test_process_cat() {
    // Copied from Wireshark, PID 1 from 10934V @ 0.8W
    let cat = [
        0x01, 0xb0, 0x21, 0xff, 0xff, 0xc5, 0x00, 0x00, 0x09, 0x04, 0x0b, 0x00, 0xe0, 0x30, 0x09,
        0x04, 0x09, 0x3e, 0xe0, 0xc1, 0x09, 0x04, 0x09, 0x40, 0xe0, 0xc2, 0x09, 0x04, 0x18, 0x8a,
        0xe0, 0x31, 0xd9, 0x66, 0x34, 0x3b,
    ];
    let mut x = Ddci::new();
    x.devices.insert(0, DdciDevice::new(0, 0));
    x.process_cat(0, &cat, 0);
    for pid in [48, 193, 194, 49] {
        assert!(mapped(&x, 0, pid), "EMM PID {pid} not mapped");
        assert!(
            x.devices[&0].emm_pids.contains(&(0, pid)),
            "EMM PID {pid} not tracked"
        );
    }
}

// Ported from tests/test_ddci.cpp:1218
#[test]
fn test_ci_pmt_name_is_refreshed() {
    let (mut x, mut t) = ci_fixture(&[2]);
    x.now_ms = 10_000;
    let src = create_pmt(&mut t, 2, 1601, 601, 602, 0x100, 0x100);
    t.get_mut(src).unwrap().pid = 600;
    assert_eq!(
        x.process_pmt(2, &prog(&t, src), false),
        TablesResult::Ok,
        "the PMT should be registered"
    );
    let ci_pmt = create_pmt(&mut t, 8, 1601, 611, 612, 0x100, 0x100);
    t.get_mut(ci_pmt).unwrap().pid = 800;
    assert_eq!(
        t.first_for_sid(8, 1601).map(|p| p.id),
        Some(ci_pmt),
        "the CI-side PMT should be found by its sid"
    );
    let mut buf = [0u8; 4096];
    let age = |x: &mut Ddci| {
        let now = x.now_ms;
        for s in x.devices.get_mut(&8).unwrap().pmt.iter_mut().take(2) {
            s.last_pmt = now - 1000;
        }
    };
    age(&mut x);
    x.add_psi(8, &mut buf, 2, &mut t);
    assert!(
        t.get(ci_pmt).unwrap().name.is_empty(),
        "the CI-side PMT should stay unnamed while the source is unnamed"
    );
    let p = t.get_mut(src).unwrap();
    p.name = "beIN SPORTS 1".into();
    p.provider = "Digital Platform".into();
    age(&mut x);
    x.add_psi(8, &mut buf, 2, &mut t);
    assert_eq!(
        t.get(ci_pmt).unwrap().name,
        "beIN SPORTS 1",
        "the CI-side PMT should take the name of the source service"
    );
    assert_eq!(
        t.get(ci_pmt).unwrap().provider,
        "Digital Platform",
        "the CI-side PMT should take the provider as well"
    );
    x.del_pmt(2, &prog(&t, src));
}

// Ported from tests/test_ddci.cpp:1295
#[test]
fn test_ddci_add_psi_per_channel_pmt() {
    let (mut x, mut t) = ci_fixture(&[2, 3]);
    x.now_ms = 10_000;
    let pmt_a = create_pmt(&mut t, 2, 1701, 701, 702, 0x100, 0x100);
    let pmt_b = create_pmt(&mut t, 3, 1702, 711, 712, 0x100, 0x100);
    assert_eq!(
        x.process_pmt(2, &prog(&t, pmt_a), false),
        TablesResult::Ok,
        "adapter A program should register"
    );
    assert_eq!(
        x.process_pmt(3, &prog(&t, pmt_b), false),
        TablesResult::Ok,
        "adapter B program should register"
    );
    let d = &x.devices[&8];
    let ia = (0..2)
        .find(|&i| d.pmt[i].id == pmt_a)
        .expect("both programs should slot");
    let ib = (0..2)
        .find(|&i| d.pmt[i].id == pmt_b)
        .expect("both programs should slot");
    assert_ne!(ia, ib, "both programs should slot");
    let now = x.now_ms;
    let d = x.devices.get_mut(&8).unwrap();
    d.pmt[ia].cc = 5;
    d.pmt[ib].cc = 7;
    d.pmt[ia].last_pmt = now - 1000;
    d.pmt[ib].last_pmt = now - 1000;
    let mut buf = [0u8; 4096];
    x.add_psi(8, &mut buf, 2, &mut t);
    let cc = |x: &Ddci, i: usize| x.devices[&8].pmt[i].cc;
    assert!(
        cc(&x, ia) == 6 && cc(&x, ib) == 7,
        "A tick emits only the processed adapter's PMT"
    );
    x.add_psi(8, &mut buf, 2, &mut t);
    assert_eq!(
        cc(&x, ia),
        6,
        "an immediate re-tick is held back by the channel's own clock"
    );
    x.add_psi(8, &mut buf, 3, &mut t);
    assert!(
        cc(&x, ia) == 6 && cc(&x, ib) == 8,
        "A's emission does not delay B's own clock"
    );
}

// Ported from tests/test_ddci.cpp:1367
fn has_pid(buf: &[u8], len: isize, pid: u16) -> bool {
    let len = len.max(0) as usize;
    buf[..len]
        .as_chunks::<188>()
        .0
        .iter()
        .any(|p| p[0] == 0x47 && pid_from_ts(p) == pid)
}

// Ported from tests/test_ddci.cpp:1376
#[test]
fn test_channel_change_sends_pat_immediately() {
    let (mut x, mut t) = ci_fixture(&[2]);
    x.now_ms = 10_000;
    let pmt0 = create_pmt(&mut t, 2, 1801, 801, 802, 0x100, 0x100);
    let pmt1 = create_pmt(&mut t, 2, 1802, 803, 804, 0x100, 0x100);
    let mut buf = [0u8; 8192];
    assert_eq!(
        x.process_pmt(2, &prog(&t, pmt0), false),
        TablesResult::Ok,
        "PMT 0 should be registered"
    );
    x.add_psi(8, &mut buf, 2, &mut t);
    let len = x.add_psi(8, &mut buf, 2, &mut t);
    assert!(
        !has_pid(&buf, len, 0),
        "no PAT expected while the timer runs"
    );
    assert!(
        !has_pid(&buf, len, 17),
        "no SDT expected while the timer runs"
    );
    assert_eq!(
        x.process_pmt(2, &prog(&t, pmt1), false),
        TablesResult::Ok,
        "PMT 1 should be registered"
    );
    let len = x.add_psi(8, &mut buf, 2, &mut t);
    assert!(has_pid(&buf, len, 0), "PAT expected right after a join");
    assert!(has_pid(&buf, len, 17), "SDT expected right after a join");
    let len = x.add_psi(8, &mut buf, 2, &mut t);
    assert!(
        !has_pid(&buf, len, 0),
        "the immediate PAT is sent only once"
    );
    assert_eq!(
        x.process_pmt(2, &prog(&t, pmt1), true),
        TablesResult::Ok,
        "re-send of PMT 1 should succeed"
    );
    let len = x.add_psi(8, &mut buf, 2, &mut t);
    assert!(!has_pid(&buf, len, 0), "a re-send must not force a PAT");
    x.del_pmt(2, &prog(&t, pmt0));
    let len = x.add_psi(8, &mut buf, 2, &mut t);
    assert!(has_pid(&buf, len, 0), "PAT expected right after a leave");
    assert!(has_pid(&buf, len, 17), "SDT expected right after a leave");
    assert_eq!(
        x.process_pmt(2, &prog(&t, pmt0), false),
        TablesResult::Ok,
        "PMT 0 should register again"
    );
    let len = x.add_psi(8, &mut buf[..DVB_FRAME - 1], 2, &mut t);
    assert!(len <= 0, "nothing fits in a short buffer");
    let len = x.add_psi(8, &mut buf, 2, &mut t);
    assert!(has_pid(&buf, len, 0), "PAT expected after a short write");
    // a leave through the no-mapping sweep also sends PAT at once
    let mut moved = prog(&t, pmt0);
    moved.pid = 7777;
    x.del_pmt(2, &moved);
    x.del_pmt_mapping_table(8, 2, pmt0);
    let len = x.add_psi(8, &mut buf, 2, &mut t);
    assert!(
        has_pid(&buf, len, 0),
        "PAT expected after a no-mapping leave"
    );
    x.del_pmt(2, &prog(&t, pmt1));
}

// Ported from tests/test_ddci.cpp:1469
#[test]
fn test_ddci_full_device_retries() {
    let mut x = Ddci::new();
    let mut t = ProgramTable::new();
    x.create_adapter(8);
    let pmt0 = create_pmt(&mut t, 8, 600, 601, 602, 0x100, 0x100);
    let pmt1 = create_pmt(&mut t, 8, 700, 701, 702, 0x100, 0x100);
    x.devices.insert(0, device(0, 1));
    x.channels.insert(600, channel(600, &[0], true));
    x.channels.insert(700, channel(700, &[0], true));
    x.add_caid_mask(0, 0x100, 0xFFFF);
    x.cams.insert(0, cam(true));
    assert_eq!(
        x.process_pmt(8, &prog(&t, pmt0), false),
        TablesResult::Ok,
        "first PMT must take the only slot"
    );
    assert_eq!(
        x.process_pmt(8, &prog(&t, pmt1), false),
        TablesResult::Retry,
        "second PMT for a full device must retry, not burn the SID"
    );
}

fn two_ddcis(caid1: u16, max_channels: usize) -> Ddci {
    let mut x = Ddci::new();
    for a in [8, 0, 1] {
        x.create_adapter(a);
    }
    x.devices.insert(0, device(0, max_channels));
    x.devices.insert(1, device(1, max_channels));
    x.add_caid_mask(0, 0x100, 0xFFFF);
    x.add_caid_mask(1, caid1, 0xFFFF);
    x.cams.insert(0, cam(true));
    x.cams.insert(1, cam(true));
    x
}

fn set_ready(x: &mut Ddci, i: usize, ready: bool) {
    x.cams.get_mut(&i).unwrap().initialized = ready;
}

fn channels(x: &Ddci, i: usize) -> i32 {
    x.devices[&i].channels
}

// Ported from tests/test_ddci.cpp:1516
#[test]
fn test_initializing_ddci_does_not_block_others() {
    let mut x = two_ddcis(0x500, 2);
    let mut t = ProgramTable::new();
    let dd0 = create_pmt(&mut t, 8, 810, 811, 812, 0x100, 0x100);
    let dd1 = create_pmt(&mut t, 8, 820, 821, 822, 0x500, 0x500);
    let both = create_pmt(&mut t, 8, 840, 841, 842, 0x100, 0x500);
    let nomatch = create_pmt(&mut t, 8, 830, 831, 832, 0x600, 0x600);

    set_ready(&mut x, 0, false);
    assert_eq!(
        x.process_pmt(8, &prog(&t, dd1), false),
        TablesResult::Ok,
        "PMT matching the ready DDCI must not wait for DDCI 0"
    );
    assert_eq!(
        x.channels[&820].ddci.len(),
        1,
        "initializing DDCI must be excluded from candidates"
    );
    assert!(
        channels(&x, 1) == 1 && channels(&x, 0) == 0,
        "PMT must run on the ready DDCI 1"
    );
    assert_eq!(
        x.process_pmt(8, &prog(&t, both), false),
        TablesResult::Ok,
        "dual-CAID PMT must proceed on the ready DDCI"
    );
    assert_eq!(
        x.process_pmt(8, &prog(&t, dd0), false),
        TablesResult::Retry,
        "PMT matching only the initializing DDCI must retry"
    );
    x.del_pmt(8, &prog(&t, dd1));
    x.del_pmt(8, &prog(&t, both));
    assert!(
        channels(&x, 0) == 0 && channels(&x, 1) == 0,
        "expected 0 running channels after delete"
    );
    x.channels.clear();

    set_ready(&mut x, 0, true);
    set_ready(&mut x, 1, false);
    assert_eq!(
        x.process_pmt(8, &prog(&t, dd0), false),
        TablesResult::Ok,
        "PMT matching the ready DDCI must not wait for DDCI 1"
    );
    assert_eq!(
        x.channels[&810].ddci.len(),
        1,
        "initializing DDCI must be excluded from candidates"
    );
    assert!(
        channels(&x, 0) == 1 && channels(&x, 1) == 0,
        "PMT must run on the ready DDCI 0"
    );
    assert_eq!(
        x.process_pmt(8, &prog(&t, nomatch), false),
        TablesResult::Retry,
        "PMT with no ready match must retry while a DDCI initializes"
    );
    x.del_pmt(8, &prog(&t, dd0));
    assert!(
        channels(&x, 0) == 0 && channels(&x, 1) == 0,
        "expected 0 running channels after delete"
    );
    x.channels.clear();

    set_ready(&mut x, 1, true);
    assert_eq!(
        x.process_pmt(8, &prog(&t, nomatch), false),
        TablesResult::NoRetry,
        "PMT with no match must not retry once all DDCIs are ready"
    );
    x.channels.clear();

    // full ready DDCI + initializing DDCI: retry, then the late DDCI once ready
    x.devices.get_mut(&0).unwrap().max_channels = 1;
    x.devices.get_mut(&1).unwrap().max_channels = 1;
    set_ready(&mut x, 1, false);
    assert_eq!(
        x.process_pmt(8, &prog(&t, dd0), false),
        TablesResult::Ok,
        "setup: fill the only slot of DDCI 0"
    );
    assert_eq!(
        x.process_pmt(8, &prog(&t, both), false),
        TablesResult::Retry,
        "dual PMT must retry while its only ready match is full"
    );
    set_ready(&mut x, 1, true);
    assert_eq!(
        x.process_pmt(8, &prog(&t, both), false),
        TablesResult::Ok,
        "dual PMT must move to the late-ready DDCI"
    );
    assert_eq!(
        x.channels[&840].ddci.len(),
        2,
        "rebuilt candidates must include the late-ready DDCI"
    );
    assert_eq!(channels(&x, 1), 1, "dual PMT must run on DDCI 1");
    x.del_pmt(8, &prog(&t, both));
    x.del_pmt(8, &prog(&t, dd0));
    assert!(
        channels(&x, 0) == 0 && channels(&x, 1) == 0,
        "expected 0 running channels after delete"
    );
}

// Ported from tests/test_ddci.cpp:1630
#[test]
fn test_ddci_least_used_device() {
    let mut x = two_ddcis(0x100, 3);
    let mut t = ProgramTable::new();
    let p: Vec<i32> = (0..5)
        .map(|i| {
            create_pmt(
                &mut t,
                8,
                900 + 10 * i,
                901 + 10 * i,
                902 + 10 * i,
                0x100,
                0x100,
            )
        })
        .collect();
    assert_eq!(
        x.process_pmt(8, &prog(&t, p[0]), false),
        TablesResult::Ok,
        "first"
    );
    assert!(
        channels(&x, 0) == 1 && channels(&x, 1) == 0,
        "first goes to DDCI 0"
    );
    assert_eq!(
        x.process_pmt(8, &prog(&t, p[1]), false),
        TablesResult::Ok,
        "second"
    );
    assert!(
        channels(&x, 0) == 1 && channels(&x, 1) == 1,
        "second goes to the idle DDCI 1, not the first fit"
    );
    assert_eq!(
        x.process_pmt(8, &prog(&t, p[2]), false),
        TablesResult::Ok,
        "third"
    );
    assert!(
        channels(&x, 0) == 2 && channels(&x, 1) == 1,
        "a tie keeps the configured order"
    );
    x.del_pmt(8, &prog(&t, p[0]));
    x.del_pmt(8, &prog(&t, p[2]));
    assert_eq!(
        x.process_pmt(8, &prog(&t, p[3]), false),
        TablesResult::Ok,
        "fourth"
    );
    assert!(
        channels(&x, 0) == 1 && channels(&x, 1) == 1,
        "after leaves the emptier DDCI 0 is used again"
    );
    // listed for both DDCIs while DDCI 0 initializes: runs on DDCI 1
    x.del_pmt(8, &prog(&t, p[3]));
    let sid = t.get(p[4]).unwrap().sid;
    x.channels.insert(sid, channel(sid, &[0, 1], true));
    set_ready(&mut x, 0, false);
    assert_eq!(
        x.process_pmt(8, &prog(&t, p[4]), false),
        TablesResult::Ok,
        "the ready DDCI takes it"
    );
    assert!(
        channels(&x, 0) == 0 && channels(&x, 1) == 2,
        "it runs on DDCI 1"
    );
}
