//! Signal scaling (`adapt_signal`, SNR calibration, v5/v3 selection) and lock recovery timing.

use super::consts::*;
use super::device::DvbDevice;

/// `ad->new_gs` value: neither statistics path has been selected yet.
pub const SIGNAL_UNPROBED: u8 = 0;
/// v5 statistics serve the poll (`NEW_SIGNAL`).
pub const NEW_SIGNAL: u8 = 1;
/// v3 ioctls serve the poll until the next tune (`OLD_SIGNAL`).
pub const OLD_SIGNAL: u8 = 2;

/// First retry delay after a tune (`LOCK_RETRY_INITIAL_MS`).
pub const LOCK_RETRY_INITIAL_MS: i64 = 6000;
/// Delay between later retries (`LOCK_RETRY_INTERVAL_MS`).
pub const LOCK_RETRY_INTERVAL_MS: i64 = 2000;
/// Retry cap (`LOCK_RETRY_MAX`).
pub const LOCK_RETRY_MAX: i32 = 5;

/// Raw 0..65535 readings before the 0..255 reduction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawSignal {
    pub status: i32,
    pub ber: u32,
    pub strength: u16,
    pub snr: u16,
    pub db: u16,
    pub status_valid: bool,
}

impl Default for RawSignal {
    fn default() -> Self {
        Self {
            status: 0,
            ber: 0,
            strength: 0,
            snr: 0,
            db: 65535,
            status_valid: false,
        }
    }
}

/// `SNR_CALIB_*` (dvb.cpp:2044-2048).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SnrCalib {
    /// Not probed yet.
    #[default]
    Unknown,
    /// No table entry, use the raw value.
    None,
    /// `Vuplus DVB-C NIM(BCM3158)`.
    Bcm3158Vu,
    /// Broadcom FBC frontends.
    BcmFbc,
    /// NIM FBC frontends.
    NimFbc,
}

/// `dvb_snr_calibration` (dvb.cpp:2055): match order follows Enigma2.
pub fn snr_calibration(name: &str) -> SnrCalib {
    if name.contains("Vuplus DVB-C NIM(BCM3158)") {
        return SnrCalib::Bcm3158Vu;
    }
    if [
        "BCM4506",
        "BCM4505",
        "BCM45208",
        "BCM45308",
        "BCM73625 (G3)",
        "BCM3158",
    ]
    .iter()
    .any(|n| name.contains(n))
    {
        return SnrCalib::BcmFbc;
    }
    if ["NIM(45208 FBC)", "NIM(45308 FBC)", "NIM(45308X FBC)"]
        .iter()
        .any(|n| name.contains(n))
    {
        return SnrCalib::NimFbc;
    }
    SnrCalib::None
}

const E2_SAT_MAX: i64 = 1600;

/// `dvb_calibrate_snr` (dvb.cpp:2070): raw `FE_READ_SNR` to 0..65535.
pub fn calibrate_snr(calib: SnrCalib, raw: u16) -> u16 {
    let snr = f64::from(raw);
    let ret: i64 = match calib {
        SnrCalib::Bcm3158Vu => (snr / 15.61) as i64,
        SnrCalib::BcmFbc => (i64::from(raw) * 100) >> 8,
        SnrCalib::NimFbc => {
            (if snr < 57500.0 {
                if snr < 52500.0 {
                    if snr < 32200.0 {
                        0.02640 * snr + 0.0
                    } else {
                        0.02604 * snr + 11.55125
                    }
                } else {
                    0.01310 * snr + 661.55172
                }
            } else {
                0.02826 * snr - 210.
            }) as i64
        }
        SnrCalib::Unknown | SnrCalib::None => return raw,
    };
    let ret = ret.max(0);
    if ret >= E2_SAT_MAX {
        return 65535;
    }
    (ret * 65535 / E2_SAT_MAX) as u16
}

/// `get_strength_decibels`: dBm/1000 to 0..65535.
pub fn strength_decibels(init: i64) -> i64 {
    ((init + 100 * 1000) as f64 * 65535.0 / (100.0 * 1000.0)) as i64
}

/// `get_strength_percent`.
pub fn strength_percent(init: i64) -> i64 {
    (init as f64 * 655.35) as i64
}

/// `get_snr_decibels`: returns the dB*10 value and the 0..65535 SNR.
pub fn snr_decibels(init: i64, db_snr_map: f32) -> (i64, i64) {
    let tf = (init as f64 * 65535.0 / f64::from(1000.0f32 * db_snr_map)) as f32;
    let snrd = if f64::from(tf) < 65535.0 {
        tf as i64
    } else {
        65535
    };
    ((init / 100).min(999), snrd)
}

/// `get_snr_percent`.
pub fn snr_percent(init: i64) -> i64 {
    (init as f64 * 655.35) as i64
}

/// `adapt_signal` (dvb.cpp:1995): reshapes v3 readings for `-M`.
pub fn adapt_signal(force: u8, db_snr_map: f32, raw: &mut RawSignal) {
    let strength_init = i64::from(raw.strength);
    let snr_init = i64::from(raw.snr);
    let mut strength_new = strength_init;
    let mut snr_new = snr_init;
    let mut db_new = i64::from(raw.db);
    if force & TUNER_FORCE_STRENGTH_PERCENT != 0 {
        strength_new = strength_percent(strength_init);
    }
    if force & TUNER_FORCE_STRENGTH_DECIBEL != 0 {
        strength_new = strength_decibels(strength_init);
    }
    if force & TUNER_FORCE_SNR_PERCENT != 0 {
        db_new = 65535;
        snr_new = snr_percent(snr_init);
    }
    if force & TUNER_FORCE_SNR_DECIBEL != 0 {
        let (db, snr) = snr_decibels(snr_init, db_snr_map);
        db_new = db;
        snr_new = snr;
    }
    // `*x = (int)y` into uint16_t: truncate to int, then wrap.
    raw.strength = strength_new as i32 as u16;
    raw.snr = snr_new as i32 as u16;
    raw.db = db_new as i32 as u16;
}

/// `dvb_read_status` (dvb.cpp:2106).
pub fn read_status<D: DvbDevice + ?Sized>(dev: &mut D, frontend_open: bool) -> Option<i32> {
    if !frontend_open {
        return None;
    }
    dev.read_status().ok()
}

/// `get_signal_new` (dvb.cpp:2186): `None` when v5 cannot serve the poll.
pub fn signal_v5<D: DvbDevice + ?Sized>(
    dev: &mut D,
    frontend_open: bool,
    db_snr_map: f32,
) -> Option<RawSignal> {
    if !frontend_open {
        return None;
    }
    let stats = dev.get_stats().ok()?;
    if stats.iter().all(|s| s.len == 0) {
        return None;
    }
    let mut raw = RawSignal::default();
    let mut strengthd = 0i64;
    let mut snrd = 0i64;
    match stats[0].scale {
        FE_SCALE_RELATIVE => strengthd = stats[0].value as i64,
        FE_SCALE_DECIBEL => strengthd = strength_decibels(stats[0].svalue()),
        _ => {}
    }
    match stats[1].scale {
        FE_SCALE_RELATIVE => snrd = stats[1].value as i64,
        FE_SCALE_DECIBEL => {
            let (db, snr) = snr_decibels(stats[1].svalue(), db_snr_map);
            raw.db = db as i32 as u16;
            snrd = snr;
        }
        _ => {}
    }
    raw.ber = (stats[2].value & 0x7fff) as u32;
    match read_status(dev, frontend_open) {
        Some(status) => {
            raw.status = status;
            raw.status_valid = true;
        }
        None => raw.status = 0,
    }
    raw.strength = strengthd as i32 as u16;
    raw.snr = snrd as i32 as u16;
    Some(raw)
}

/// `get_signal` (dvb.cpp:2119): v3 ioctls plus SNR calibration.
pub fn signal_v3<D: DvbDevice + ?Sized>(
    dev: &mut D,
    frontend_open: bool,
    snr_calib: &mut SnrCalib,
    strength_warned: &mut bool,
    force: u8,
) -> RawSignal {
    let mut raw = RawSignal::default();
    let Some(status) = read_status(dev, frontend_open) else {
        return raw;
    };
    raw.status = status;
    raw.status_valid = true;
    if status == 0 {
        return raw;
    }
    if let Ok(ber) = dev.read_ber() {
        raw.ber = ber;
    }
    if let Ok(strength) = dev.read_signal_strength() {
        raw.strength = strength;
    }
    match dev.read_snr() {
        Err(_) => {}
        Ok(snr) => {
            raw.snr = snr;
            if force == TUNER_FORCE_NO {
                if *snr_calib == SnrCalib::Unknown {
                    let name = dev.get_info().map(|info| info.name).unwrap_or_default();
                    *snr_calib = snr_calibration(&name);
                }
                raw.snr = calibrate_snr(*snr_calib, snr);
                if matches!(*snr_calib, SnrCalib::BcmFbc | SnrCalib::NimFbc)
                    && raw.strength == 0
                    && raw.snr > 0
                    && status & FE_HAS_LOCK != 0
                {
                    *strength_warned = true;
                    raw.strength = raw.snr;
                }
            }
        }
    }
    raw
}

/// `strength * multiplier` assigned to `uint16_t`; out-of-range saturates (DEV-070).
pub fn apply_multiplier(value: u16, multiplier: f32) -> u16 {
    (f32::from(value) * multiplier) as u16
}
