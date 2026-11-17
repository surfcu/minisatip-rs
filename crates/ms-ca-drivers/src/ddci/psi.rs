//! PSI helpers shared by the DDCI tables: `buffer_to_ts`, DVB strings, `UTC_time`.

pub use ms_ts::buffer_to_ts;

/// `dvb_put_string`: UTF-8 with the 0x15 code table prefix, or an empty string.
pub fn dvb_put_string(out: &mut Vec<u8>, s: &str) {
    if s.is_empty() {
        out.push(0);
        return;
    }
    out.push((s.len() + 1) as u8);
    out.push(0x15);
    out.extend_from_slice(s.as_bytes());
}

/// EN 300 468 40-bit `UTC_time`: 16-bit MJD then 6-digit BCD `HHMMSS` (UTC).
pub fn utc_time_bcd(unix: u64) -> [u8; 5] {
    let bcd = |v: u64| (((v / 10) << 4) | (v % 10)) as u8;
    let [hi, lo] = ((unix / 86400).wrapping_add(40587) as u16).to_be_bytes();
    let t = unix % 86400;
    [hi, lo, bcd(t / 3600), bcd(t % 3600 / 60), bcd(t % 60)]
}

/// `set_pid_ts`.
pub fn set_pid_ts(b: &mut [u8], pid: u16) {
    let pid = pid & 0x1FFF;
    b[1] &= 0xE0;
    b[1] |= (pid >> 8) as u8 & 0x1F;
    b[2] = pid as u8;
}

/// `PID_FROM_TS`.
pub fn pid_from_ts(b: &[u8]) -> u16 {
    (u16::from(b[1] & 0x1F) << 8) | u16::from(b[2])
}
