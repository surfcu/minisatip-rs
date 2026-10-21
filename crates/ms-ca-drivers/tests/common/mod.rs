#![allow(dead_code)]

use std::sync::Arc;

use ms_ca::{CaCaps, Program, ProgramTable};
use ms_ca_drivers::en50221::{En50221Driver, TAG_CA_PMT, TpduLink};
use ms_fakecam::host::HostSim;
use ms_fakecam::record::Event;
use ms_fakecam::tags::CA_SUPPORT;
use ms_fakecam::{CaPmt, CamConfig, FakeCam, ManualClock};
use ms_psi::{CrcCheck, Pmt};

/// The fake CAM behind the CA device fd; the host side answers its polls.
pub struct CamLink {
    pub cam: FakeCam,
    pub host: HostSim,
}

impl TpduLink for CamLink {
    fn write_tpdu(&mut self, tpdu: &[u8]) -> bool {
        let ok = self.cam.handle_tpdu(tpdu).is_some();
        self.host.pump(&mut self.cam);
        ok
    }
}

/// An initialized fake CAM reached through the EN 50221 CA_PMT driver.
pub fn fake_cam(config: CamConfig, caps: CaCaps) -> En50221Driver<CamLink> {
    let clock = ManualClock::new(0);
    let mut cam = FakeCam::new(config, Arc::new(clock));
    let mut host = HostSim::new();
    host.connect(&mut cam);
    assert!(host.initialized, "CA_INFO reached the host");
    let session = host
        .sessions
        .iter()
        .find(|(_, r)| **r == CA_SUPPORT)
        .map(|(s, _)| *s);
    En50221Driver::new(CamLink { cam, host }, session, caps)
}

/// CA_PMTs the CAM received since the last call, decoded.
pub fn take_capmts(drv: &mut En50221Driver<CamLink>, seen: &mut usize) -> Vec<CaPmt> {
    let all: Vec<CaPmt> = drv
        .link
        .cam
        .recorder()
        .apdus()
        .into_iter()
        .filter_map(|r| match r.event {
            Event::Apdu { tag, body, .. } if tag == TAG_CA_PMT => {
                Some(CaPmt::parse(&body).expect("well-formed CA_PMT"))
            }
            _ => None,
        })
        .collect();
    let new = all[*seen..].to_vec();
    *seen = all.len();
    new
}

/// The recorded CA_PMT lines, without timestamps (APDU snapshot).
pub fn capmt_log(drv: &En50221Driver<CamLink>) -> String {
    drv.link
        .cam
        .recorder()
        .apdus()
        .iter()
        .map(|r| r.untimed())
        .filter(|l| l.contains("9F8032"))
        .collect::<Vec<_>>()
        .join("\n")
}

// Ported from tests/test_pmt.cpp:426
pub fn build_pmt(
    sid: u16,
    version: u8,
    pcr: u16,
    caid: u16,
    ecm: u16,
    types: &[u8],
    pids: &[u16],
) -> Vec<u8> {
    let pi_len = if caid != 0 { 6 } else { 0 };
    let len = 12 + pi_len + 5 * pids.len() + 4;
    let mut b = vec![0u8; len];
    b[0] = 0x02;
    b[1] = 0xB0 | (((len - 3) >> 8) & 0x0F) as u8;
    b[2] = (len - 3) as u8;
    b[3..5].copy_from_slice(&sid.to_be_bytes());
    b[5] = 0xC1 | ((version & 0x1F) << 1);
    b[8] = 0xE0 | ((pcr >> 8) & 0x1F) as u8;
    b[9] = pcr as u8;
    b[10] = 0xF0;
    b[11] = pi_len as u8;
    if caid != 0 {
        b[12..18].copy_from_slice(&[
            0x09,
            0x04,
            (caid >> 8) as u8,
            caid as u8,
            0xE0 | ((ecm >> 8) & 0x1F) as u8,
            ecm as u8,
        ]);
    }
    for (i, (&t, &p)) in types.iter().zip(pids).enumerate() {
        let o = 12 + pi_len + 5 * i;
        b[o..o + 5].copy_from_slice(&[t, 0xE0 | ((p >> 8) & 0x1F) as u8, p as u8, 0xF0, 0x00]);
    }
    let end = len - 4;
    let crc = ms_ts::crc::crc32_mpeg2(&b[..end]);
    b[end..].copy_from_slice(&crc.to_be_bytes());
    b
}

// Ported from tests/test_pmt.cpp:2570
pub fn build_pmt_ca_private(version: u8, priv_len: usize) -> Vec<u8> {
    let pi_len = 6 + priv_len;
    let len = 12 + pi_len + 5 + 4;
    let mut b = vec![0u8; len];
    b[0] = 0x02;
    b[1] = 0xB0 | (((len - 3) >> 8) & 0x0F) as u8;
    b[2] = (len - 3) as u8;
    b[4] = 100; // sid
    b[5] = 0xC1 | ((version & 0x1F) << 1);
    b[8] = 0xE0 | ((3301 >> 8) & 0x1F) as u8;
    b[9] = (3301 & 0xFF) as u8;
    b[10] = 0xF0 | ((pi_len >> 8) & 0x0F) as u8;
    b[11] = pi_len as u8;
    b[12] = 0x09;
    b[13] = (4 + priv_len) as u8;
    b[14] = 0x0B;
    b[15] = 0x00;
    b[16] = 0xE0 | ((0x0C00 >> 8) & 0x1F) as u8;
    b[17] = 0x00;
    for (i, byte) in b[18..18 + priv_len].iter_mut().enumerate() {
        *byte = (i + 1) as u8;
    }
    b[12 + pi_len] = 2;
    b[13 + pi_len] = 0xE0 | ((3301 >> 8) & 0x1F) as u8;
    b[14 + pi_len] = (3301 & 0xFF) as u8;
    b[15 + pi_len] = 0xF0;
    b[16 + pi_len] = 0x00;
    let end = len - 4;
    let crc = ms_ts::crc::crc32_mpeg2(&b[..end]);
    b[end..].copy_from_slice(&crc.to_be_bytes());
    b
}

/// `process_pmt` on a built section: parse with ms-psi and refresh the program.
pub fn feed_pmt(t: &mut ProgramTable, id: i32, section: &[u8]) {
    let pmt = Pmt::parse(section, CrcCheck::Verify).expect("PMT parses");
    t.get_mut(id).unwrap().apply_pmt(&pmt);
}

/// A scrambled program with one video and one audio stream.
pub fn av_program(
    t: &mut ProgramTable,
    adapter: u16,
    sid: u16,
    pmt_pid: u16,
    video: u16,
    audio: u16,
) -> i32 {
    let id = t.add(adapter, sid, pmt_pid);
    feed_pmt(
        t,
        id,
        &build_pmt(sid, 1, video, 0x0B00, 0x0573, &[2, 4], &[video, audio]),
    );
    id
}

pub fn prog(t: &ProgramTable, id: i32) -> Program {
    t.get(id).unwrap().clone()
}
