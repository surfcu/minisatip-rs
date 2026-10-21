//! DDCI TS port: selected services are descrambled, with configurable start
//! latency, per-packet latency and the CI bandwidth limit.

mod common;

use ms_fakecam::host::{apdu_tpdu, tpdu};
use ms_fakecam::tags::T_DELETE_TC;
use ms_fakecam::{CamConfig, ListManagement};

const PID_A: u16 = 0x100;
const PID_B: u16 = 0x200;
const PID_C: u16 = 0x300;

fn select(cam: &mut ms_fakecam::FakeCam, host: &mut ms_fakecam::host::HostSim, pids: &[u16]) {
    let pmt = common::capmt(
        ListManagement::Only,
        0x1234,
        1,
        pids,
        ms_fakecam::capmt::CMD_OK_DESCRAMBLING,
    );
    host.send_capmt(cam, &pmt.encode());
}

// From tests/test_ddci.cpp:640 (test_ddci_process_ts): TS packets are
// identified by PID, and only the mapped for the CI are touched.
#[test]
fn clears_selected_pids_and_leaves_others_alone() {
    let (mut cam, mut host, _) = common::setup(CamConfig {
        max_services: 2,
        ..CamConfig::default()
    });
    select(&mut cam, &mut host, &[PID_A, PID_B]);

    let mut ts = Vec::new();
    ts.extend_from_slice(&common::scrambled(PID_A, 0));
    ts.extend_from_slice(&common::scrambled(PID_B, 1));
    ts.extend_from_slice(&common::scrambled(PID_C, 2));
    cam.write_ts(&ts);

    let out = cam.read_ts();
    assert_eq!(
        common::scrambling(&out),
        [(PID_A, 0), (PID_B, 0), (PID_C, 3)]
    );
    let stats = cam.ts_stats();
    assert_eq!(stats.packets_in, 3);
    assert_eq!(stats.packets_out, 3);
    assert_eq!(stats.descrambled, 2);
    assert_eq!(stats.still_scrambled, 1);
    assert_eq!(stats.dropped_bandwidth, 0);
}

#[test]
fn clear_packets_are_not_counted_as_scrambled() {
    let (mut cam, mut host, _) = common::setup(CamConfig::default());
    select(&mut cam, &mut host, &[PID_A]);
    let mut pkt = common::scrambled(PID_A, 0);
    pkt[3] = 0x10; // payload only, not scrambled
    cam.write_ts(&pkt);
    assert_eq!(common::scrambling(&cam.read_ts()), [(PID_A, 0)]);
    assert_eq!(cam.ts_stats().descrambled, 0);
    assert_eq!(cam.ts_stats().still_scrambled, 0);
}

#[test]
fn descramble_start_latency_delays_the_first_clear_packet() {
    let (mut cam, mut host, clock) = common::setup(CamConfig {
        descramble_latency_ms: 100,
        ..CamConfig::default()
    });
    select(&mut cam, &mut host, &[PID_A]);

    cam.write_ts(&common::scrambled(PID_A, 0));
    assert_eq!(common::scrambling(&cam.read_ts()), [(PID_A, 3)]);

    clock.advance(99);
    cam.write_ts(&common::scrambled(PID_A, 1));
    assert_eq!(common::scrambling(&cam.read_ts()), [(PID_A, 3)]);

    clock.advance(1);
    cam.write_ts(&common::scrambled(PID_A, 2));
    assert_eq!(common::scrambling(&cam.read_ts()), [(PID_A, 0)]);
    assert_eq!(cam.ts_stats().descrambled, 1);
}

#[test]
fn byte_latency_delays_every_packet() {
    let (mut cam, mut host, clock) = common::setup(CamConfig {
        ts_latency_ms: 50,
        ..CamConfig::default()
    });
    select(&mut cam, &mut host, &[PID_A]);

    cam.write_ts(&common::scrambled(PID_A, 0));
    assert!(cam.read_ts().is_empty());
    assert_eq!(cam.ts_pending(), 1);
    clock.advance(49);
    assert!(cam.read_ts().is_empty());
    clock.advance(1);
    assert_eq!(common::scrambling(&cam.read_ts()), [(PID_A, 0)]);
    assert_eq!(cam.ts_pending(), 0);
}

#[test]
fn packets_split_across_writes_are_reassembled() {
    let (mut cam, mut host, _) = common::setup(CamConfig::default());
    select(&mut cam, &mut host, &[PID_A]);
    let pkt = common::scrambled(PID_A, 0);
    cam.write_ts(&pkt[..100]);
    assert!(cam.read_ts().is_empty());
    assert_eq!(cam.ts_stats().packets_in, 0);
    cam.write_ts(&pkt[100..]);
    let out = cam.read_ts();
    assert_eq!(out.len(), 188);
    assert_eq!(common::scrambling(&out), [(PID_A, 0)]);
    assert_eq!(cam.ts_stats().packets_in, 1);
}

// A lost sync byte is skipped without swallowing the following packet.
#[test]
fn resync_skips_bytes_before_the_sync_byte() {
    let (mut cam, mut host, _) = common::setup(CamConfig::default());
    select(&mut cam, &mut host, &[PID_A]);
    let pkt = common::scrambled(PID_A, 0);
    cam.write_ts(&[0x00, 0x11]);
    cam.write_ts(&pkt);
    assert_eq!(cam.ts_stats().resync_bytes, 2);
    let out = cam.read_ts();
    assert_eq!(out.len(), 188);
    assert_eq!(common::scrambling(&out), [(PID_A, 0)]);
}

#[test]
fn ci_bandwidth_limit_drops_excess_packets() {
    // 188-byte packets, one per second.
    let bps = (188 * 8) as u64;
    let (mut cam, mut host, clock) = common::setup(CamConfig {
        ci_bandwidth_bps: Some(bps),
        bandwidth_burst_ms: 0,
        ..CamConfig::default()
    });
    select(&mut cam, &mut host, &[PID_A]);

    cam.write_ts(&common::scrambled(PID_A, 0));
    cam.write_ts(&common::scrambled(PID_A, 1));
    assert_eq!(common::scrambling(&cam.read_ts()), [(PID_A, 0)]);
    assert_eq!(cam.ts_stats().packets_in, 2);
    assert_eq!(cam.ts_stats().dropped_bandwidth, 1);

    // one packet-worth of tokens is refilled per second
    clock.advance(1000);
    cam.write_ts(&common::scrambled(PID_A, 2));
    assert_eq!(common::scrambling(&cam.read_ts()), [(PID_A, 0)]);
    assert_eq!(cam.ts_stats().dropped_bandwidth, 1);
}

#[test]
fn programs_over_the_service_limit_stay_scrambled() {
    let (mut cam, mut host, _) = common::setup(CamConfig {
        max_services: 1,
        ..CamConfig::default()
    });
    // First/More/Last: only the first program fits.
    for (lm, pid) in [
        (ListManagement::First, PID_A),
        (ListManagement::More, PID_B),
    ] {
        let pmt = common::capmt(lm, pid, 1, &[pid], ms_fakecam::capmt::CMD_OK_DESCRAMBLING);
        host.send_capmt(&mut cam, &pmt.encode());
    }
    let mut ts = Vec::new();
    ts.extend_from_slice(&common::scrambled(PID_A, 0));
    ts.extend_from_slice(&common::scrambled(PID_B, 0));
    cam.write_ts(&ts);
    assert_eq!(common::scrambling(&cam.read_ts()), [(PID_A, 0), (PID_B, 3)]);
}

#[test]
fn deleting_the_transport_connection_drops_pending_ts() {
    let (mut cam, mut host, _) = common::setup(CamConfig {
        ts_latency_ms: 100,
        ..CamConfig::default()
    });
    select(&mut cam, &mut host, &[PID_A]);
    cam.write_ts(&common::scrambled(PID_A, 0));
    assert_eq!(cam.ts_pending(), 1);

    let r = cam.handle_tpdu(&tpdu(T_DELETE_TC, &[])).unwrap();
    assert_eq!(&r[..5], [0, 1, 0x85, 1, 1]);
    assert_eq!(cam.ts_pending(), 0);
    assert!(cam.read_ts().is_empty());
}

#[test]
fn an_apdu_for_the_wrong_resource_is_ignored() {
    let (mut cam, _host, _) = common::setup(CamConfig::default());
    // session 1 is the resource manager; a CA_PMT there must be ignored
    cam.handle_tpdu(&apdu_tpdu(
        1,
        ms_fakecam::tags::TAG_CA_PMT,
        &[3, 0x12, 0x34, 2, 0, 0],
    ));
    assert!(cam.programs().programs().is_empty());
}
