#![allow(dead_code)]

use ms_fakecam::capmt::{CMD_OK_DESCRAMBLING, ca_descriptor};
use ms_fakecam::host::HostSim;
use ms_fakecam::{CaPmt, CaPmtStream, CamConfig, FakeCam, ListManagement, ManualClock};
use std::sync::Arc;

/// A connected CAM/host pair on a manual clock.
pub fn setup(config: CamConfig) -> (FakeCam, HostSim, ManualClock) {
    let clock = ManualClock::new(0);
    let mut cam = FakeCam::new(config, Arc::new(clock.clone()));
    let mut host = HostSim::new();
    host.connect(&mut cam);
    assert!(host.initialized, "CA_INFO reached the host");
    (cam, host, clock)
}

// From tests/test_ca.cpp:297 (CAID 0x0B00, ECM pid 0x573)
pub const CAID: u16 = 0x0B00;
pub const ECM: u16 = 0x573;

/// A CA_PMT shaped like `create_capmt`: ES-level command and CA descriptor.
pub fn capmt(lm: ListManagement, pn: u16, version: u8, pids: &[u16], cmd: u8) -> CaPmt {
    CaPmt {
        list_management: lm,
        program_number: pn,
        version,
        current_next: true,
        cmd_id: None,
        descriptors: Vec::new(),
        streams: pids
            .iter()
            .enumerate()
            .map(|(i, &pid)| CaPmtStream {
                stream_type: if i == 0 { 2 } else { 3 },
                pid,
                cmd_id: Some(cmd),
                descriptors: ca_descriptor(CAID, ECM, &[]),
            })
            .collect(),
    }
}

pub fn ok(lm: ListManagement, pn: u16, version: u8, pids: &[u16]) -> Vec<u8> {
    capmt(lm, pn, version, pids, CMD_OK_DESCRAMBLING).encode()
}

/// A scrambled (odd key) TS packet on `pid`.
pub fn scrambled(pid: u16, cc: u8) -> [u8; 188] {
    let mut p = [0xFFu8; 188];
    p[0] = 0x47;
    p[1] = (pid >> 8) as u8 & 0x1F;
    p[2] = pid as u8;
    p[3] = 0xC0 | 0x10 | (cc & 0x0F);
    p
}

/// Scrambling control of every packet in `ts`, by PID.
pub fn scrambling(ts: &[u8]) -> Vec<(u16, u8)> {
    ts.as_chunks::<188>()
        .0
        .iter()
        .map(|p| ((u16::from(p[1] & 0x1F) << 8) | u16::from(p[2]), p[3] >> 6))
        .collect()
}
