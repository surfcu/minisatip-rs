//! PSI helpers shared by the DDCI tables: `buffer_to_ts`, DVB strings, MJD.

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

/// `YMDtoMJD`, called with `tm_year` (years since 1900) as in C++.
pub fn ymd_to_mjd(y: i32, m: i32, d: i32) -> u16 {
    let l = i32::from(m < 3);
    (14956
        + d
        + ((f64::from(y - l) * 365.25) as i32)
        + ((f64::from(m + 1 + l * 12) * 30.6001) as i32)) as u16
}

/// Broken-down local time (`struct tm` fields used by `ddci_create_eit`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LocalTime {
    /// Years since 1900.
    pub year: i32,
    /// 1..=12.
    pub month: i32,
    pub mday: i32,
    pub hour: u8,
    pub min: u8,
    pub sec: u8,
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
