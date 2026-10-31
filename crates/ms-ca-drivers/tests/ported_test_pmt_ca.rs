//! CA dispatch tests ported from tests/test_pmt.cpp, run against the CAM path (`CaDevice`).

mod common;

use common::*;
use ms_ca::capmt::{CLM_ONLY, CLM_UPDATE, CMD_ID_OK_DESCRAMBLING};
use ms_ca::{CaCaps, CaDevice, ProgramTable, TablesResult};
use ms_ca_drivers::ddci::{CamInfo, Ddci, DdciDevice};
use ms_fakecam::CamConfig;

// Ported from tests/test_pmt.cpp:2594 (C++ reuses the CA entry; data is replaced)
#[test]
fn test_pmt_update_longer_ca_private_data() {
    let mut t = ProgramTable::new();
    let id = t.add(0, 100, 48);
    feed_pmt(&mut t, id, &build_pmt_ca_private(1, 0));
    assert_eq!(
        prog(&t, id).caids[0].private_data.len(),
        0,
        "v1 private data"
    );

    let sec = build_pmt_ca_private(2, 200);
    feed_pmt(&mut t, id, &sec);
    let p = prog(&t, id);
    assert_eq!(p.caids.len(), 1, "v2 CAIDs");
    assert_eq!(p.caids[0].private_data.len(), 200, "v2 private data");
    assert_eq!(
        p.caids[0].private_data.as_slice(),
        &sec[18..218],
        "v2 private data"
    );
}

// Ported from tests/test_pmt.cpp:2637 (reset_pmt_ca keeps the entry for reuse)
#[test]
fn test_pmt_add_caid_null_clears_reused_entry() {
    let mut t = ProgramTable::new();
    let id = t.add(0, 100, 48);
    t.get_mut(id).unwrap().add_caid(0x0B00, 0x0573, &[0xA5; 20]);
    assert_eq!(
        prog(&t, id).caids[0].private_data.len(),
        20,
        "filled private data"
    );

    feed_pmt(
        &mut t,
        id,
        &build_pmt(100, 1, 3301, 0x0B00, 0x0573, &[2], &[3301]),
    );
    let p = prog(&t, id);
    assert_eq!(p.caids.len(), 1, "CA registration should stay");
    assert_eq!(p.caids[0].private_data.len(), 0, "cleared private data");
}

// Ported from tests/test_pmt.cpp:3260 (C++ passes len -1; here a short CA descriptor)
#[test]
fn test_add_caid_rejects_negative_len() {
    let mut t = ProgramTable::new();
    let id = t.add(0, 100, 48);
    // A CA descriptor shorter than CAID+PID is malformed and must be skipped.
    let mut b = vec![
        0x02, 0xB0, 22, 0x00, 0x64, 0xC3, 0x00, 0x00, 0xEC, 0xE5, 0xF0, 0x04, 0x09, 0x02, 0x0B,
        0x00, 0x02, 0xEC, 0xE5, 0xF0, 0x00, 0, 0, 0, 0,
    ];
    let crc = ms_ts::crc::crc32_mpeg2(&b[..21]);
    b[21..].copy_from_slice(&crc.to_be_bytes());
    feed_pmt(&mut t, id, &b);
    assert_eq!(
        prog(&t, id).caids.len(),
        0,
        "malformed descriptor must be skipped"
    );

    feed_pmt(
        &mut t,
        id,
        &build_pmt(100, 1, 3301, 0x0B00, 0x0573, &[2], &[3301]),
    );
    assert_eq!(
        prog(&t, id).caids.len(),
        1,
        "empty private data still registers"
    );
}

// C++ `process_pmt` keeps `SPMT::pcr_pid` as written, even when it names a stream.
#[test]
fn apply_pmt_keeps_section_pcr_pid() {
    let mut t = ProgramTable::new();
    let id = t.add(0, 100, 48);
    feed_pmt(
        &mut t,
        id,
        &build_pmt(100, 1, 3301, 0x0B00, 0x0C00, &[2, 3], &[3301, 3401]),
    );
    assert_eq!(
        prog(&t, id).pcr_pid,
        3301,
        "the PCR pid naming a stream is kept"
    );
}

fn pids(c: &ms_fakecam::CaPmt) -> Vec<u16> {
    c.streams.iter().map(|s| s.pid).collect()
}

// Ported from tests/test_pmt.cpp:3031 (DDCI + DVBCA, the CAM is ms-fakecam)
#[test]
fn test_pmt_update_e2e_single_capmt_update() {
    let mut t = ProgramTable::new();
    let mut x = Ddci::new();
    x.create_adapter(8);
    x.create_adapter(0);
    let mut d0 = DdciDevice::new(0, 1);
    d0.clear_slots();
    x.devices.insert(0, d0);
    x.cams.insert(
        8,
        CamInfo {
            enabled: true,
            initialized: true,
            multiple_pmt: 0,
            max_ca_pmt: 4,
        },
    );
    x.add_caid_mask(0, 0x0B00, 0xFFFF);
    let mut dev = CaDevice::new(8, 4, 0);
    dev.caid_filter.push((0x0B00, 0xFFFF));
    let caps = CaCaps {
        max_ca_pmt: 4,
        multiple_pmt: 0,
        ..CaCaps::default()
    };
    let mut drv = fake_cam(
        CamConfig {
            max_services: 4,
            ..CamConfig::default()
        },
        caps,
    );
    let mut seen = 0;
    take_capmts(&mut drv, &mut seen);

    let id = t.add(8, 7100, 1025);
    let types = [27, 4, 4, 4, 6, 6, 5];
    feed_pmt(
        &mut t,
        id,
        &build_pmt(
            7100,
            2,
            314,
            0x0B00,
            0x0C00,
            &types,
            &[314, 870, 873, 3127, 3128, 5100, 6403],
        ),
    );
    assert_eq!(
        dev.process_pmt(id, false, &t, &mut drv),
        TablesResult::Ok,
        "no stop on first add"
    );
    assert_eq!(x.process_pmt(8, &prog(&t, id), false), TablesResult::Ok);
    let c = take_capmts(&mut drv, &mut seen);
    assert_eq!(c.len(), 1, "one CAPMT expected");
    assert_eq!(
        c[0].list_management.to_byte(),
        CLM_ONLY,
        "first send is ONLY"
    );
    assert_eq!(
        c[0].program_cmd(),
        Some(CMD_ID_OK_DESCRAMBLING),
        "first send is OK"
    );
    assert_eq!(c[0].streams.len(), 4, "only AV streams go in the CAPMT");
    assert!(pids(&c[0]).contains(&3127), "3127 in first CAPMT");
    let log = capmt_log(&drv);
    assert_eq!(log.lines().count(), 1, "one CA_PMT APDU in the log");
    assert!(log.contains("9F8032 ca_pmt"), "logged APDU is CA_PMT");
    assert!(log.contains("lm=only"), "logged first CAPMT is ONLY");
    assert!(log.contains("es=0C37"), "logged CAPMT carries 3127");

    assert_eq!(
        x.devices[&0].pmt[0].id, id,
        "DDCI should hold the PMT on slot 0"
    );
    assert!(
        x.get_pid_mapping_allddci(8, 3127).is_some(),
        "3127 should map"
    );
    let kept: Vec<i32> = [314, 870, 873, 3128]
        .iter()
        .map(|&p| x.get_pid_mapping_allddci(8, p).unwrap().ddci_pid)
        .collect();
    assert!(
        !x.create_pmt_for_slot(0, 0, &prog(&t, id), 1500).is_empty(),
        "DDCI PMT expected"
    );
    let ver1 = x.devices[&0].pmt[0].ver;

    // Version bump: 3127 disappears, 3151 appears.
    feed_pmt(
        &mut t,
        id,
        &build_pmt(
            7100,
            3,
            314,
            0x0B00,
            0x0C00,
            &types,
            &[314, 870, 873, 3151, 3128, 5100, 6403],
        ),
    );
    assert_eq!(
        dev.process_pmt(id, true, &t, &mut drv),
        TablesResult::Ok,
        "no stop/start on update"
    );
    assert_eq!(x.process_pmt(8, &prog(&t, id), true), TablesResult::Ok);
    let c = take_capmts(&mut drv, &mut seen);
    assert_eq!(c.len(), 1, "single UPDATE");
    assert_eq!(
        c[0].list_management.to_byte(),
        CLM_UPDATE,
        "re-send is UPDATE"
    );
    assert_eq!(
        c[0].program_cmd(),
        Some(CMD_ID_OK_DESCRAMBLING),
        "update is OK"
    );
    assert!(!pids(&c[0]).contains(&3127), "3127 must be gone");
    assert!(pids(&c[0]).contains(&3151), "3151 must be present");
    let log = capmt_log(&drv);
    assert_eq!(log.lines().count(), 2, "two CA_PMT APDUs in the log");
    let last = log.lines().last().unwrap();
    assert!(last.contains("lm=update"), "logged update CAPMT is UPDATE");
    assert!(!last.contains("es=0C37"), "3127 gone from the logged CAPMT");
    assert_eq!(x.devices[&0].pmt[0].id, id, "DDCI should keep the slot");
    assert!(
        x.get_pid_mapping_allddci(8, 3127).is_none(),
        "3127 must unmap"
    );
    assert!(
        x.get_pid_mapping_allddci(8, 3151).is_some(),
        "3151 must map"
    );
    for (&p, &k) in [314, 870, 873, 3128].iter().zip(&kept) {
        assert_eq!(
            x.get_pid_mapping_allddci(8, p).unwrap().ddci_pid,
            k,
            "kept pids keep ddci pids"
        );
    }
    x.create_pmt_for_slot(0, 0, &prog(&t, id), 1500);
    assert_eq!(
        x.devices[&0].pmt[0].ver,
        (ver1 + 1) & 0xF,
        "DDCI PMT bumps once"
    );

    // The client subscribes 3151 late: re-parse and re-send, the identical CAPMT is skipped.
    feed_pmt(
        &mut t,
        id,
        &build_pmt(
            7100,
            3,
            314,
            0x0B00,
            0x0C00,
            &types,
            &[314, 870, 873, 3151, 3128, 5100, 6403],
        ),
    );
    assert_eq!(
        dev.process_pmt(id, true, &t, &mut drv),
        TablesResult::Ok,
        "no stop/start on pid add"
    );
    assert_eq!(x.process_pmt(8, &prog(&t, id), true), TablesResult::Ok);
    assert!(
        take_capmts(&mut drv, &mut seen).is_empty(),
        "identical re-send writes no CAPMT"
    );
    assert_eq!(
        capmt_log(&drv).lines().count(),
        2,
        "identical re-send logs no CAPMT"
    );
    x.create_pmt_for_slot(0, 0, &prog(&t, id), 1500);
    assert_eq!(
        x.devices[&0].pmt[0].ver,
        (ver1 + 1) & 0xF,
        "DDCI version stable"
    );
}
