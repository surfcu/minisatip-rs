//! RTCP sender report, SDES and SAT>IP `SES1` APP packet, ported from `send_rtcp`.

use std::time::{SystemTime, UNIX_EPOCH};

/// Seconds between the NTP epoch (1900) and the Unix epoch.
pub const NTP_UNIX_OFFSET: u64 = 2_208_988_800;
/// Size of the C++ `rtcp_buf`.
const RTCP_BUF_LEN: usize = 1600;

/// Values copied into one RTCP compound packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RtcpInfo {
    /// Stream SSRC.
    pub ssrc: u32,
    /// Wall-clock time used for the NTP seconds word.
    pub wall: SystemTime,
    /// Current tick in ms; C++ stores it where the NTP fraction belongs.
    pub ctime_ms: i64,
    /// Tick of the last data flush in ms (`sid->wtime`), sent as RTP timestamp.
    pub wtime_ms: i64,
    /// Sender packet count (`sid->sp`: successful flushes).
    pub packets: u32,
    /// Sender octet count (`sid->sb`).
    pub octets: u32,
}

/// NTP seconds word for `wall`, truncated to 32 bits like `(int)(ntp >> 32)`.
pub fn ntp_seconds(wall: SystemTime) -> u32 {
    let secs = wall
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    secs.wrapping_add(NTP_UNIX_OFFSET) as u32
}

/// Builds SR + SDES + APP(`SES1`) for `description`; `interleaved` adds `$\x01len`.
pub fn build_rtcp(info: &RtcpInfo, description: &str, interleaved: bool) -> Vec<u8> {
    let mut la = description.len();
    if la > RTCP_BUF_LEN - 78 {
        la = RTCP_BUF_LEN - 80;
    }
    let len = (la + 16).div_ceil(4) * 4;
    let total_len = len + 52;

    let mut rtcp = vec![0u8; total_len];
    rtcp[0] = 0x80;
    rtcp[1] = 0xC8;
    rtcp[3] = 6;
    put32(&mut rtcp, 4, info.ssrc);
    put32(&mut rtcp, 8, ntp_seconds(info.wall));
    put32(&mut rtcp, 12, info.ctime_ms as u32);
    put32(&mut rtcp, 16, info.wtime_ms as u32);
    put32(&mut rtcp, 20, info.packets);
    put32(&mut rtcp, 24, info.octets);
    rtcp[28] = 0x81;
    rtcp[29] = 0xCA;
    rtcp[31] = 5;
    put32(&mut rtcp, 32, info.ssrc);
    rtcp[36] = 1;
    rtcp[37] = 10;
    rtcp[38..47].copy_from_slice(b"minisatip");
    rtcp[52] = 0x80;
    rtcp[53] = 0xCC;
    rtcp[54..56].copy_from_slice(&(((la + 16).div_ceil(4) - 1) as u16).to_be_bytes());
    put32(&mut rtcp, 56, info.ssrc);
    rtcp[60..64].copy_from_slice(b"SES1");
    rtcp[66..68].copy_from_slice(&(la as u16).to_be_bytes());
    rtcp[68..68 + la].copy_from_slice(&description.as_bytes()[..la]);

    if !interleaved {
        return rtcp;
    }
    let mut framed = Vec::with_capacity(total_len + 4);
    framed.extend_from_slice(&[0x24, 1]);
    framed.extend_from_slice(&(total_len as u16).to_be_bytes());
    framed.extend_from_slice(&rtcp);
    framed
}

fn put32(buf: &mut [u8], at: usize, value: u32) {
    buf[at..at + 4].copy_from_slice(&value.to_be_bytes());
}
