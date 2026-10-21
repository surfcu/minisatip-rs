//! Signal scaling (`adapt_signal`, SNR calibration, v5/v3 selection) and lock recovery timing.

use super::consts::*;
use super::device::{DvbDevice, error_parts};

/// `#define DEFAULT_LOG LOG_DVB` (dvb.cpp:53).
const DEFAULT_LOG: u32 = ms_log::LOG_DVB;

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

impl SnrCalib {
    /// `SNR_CALIB_*` number used by the `dvb.cpp` log line.
    pub fn number(self) -> i32 {
        match self {
            Self::Unknown => 0,
            Self::None => 1,
            Self::Bcm3158Vu => 2,
            Self::BcmFbc => 3,
            Self::NimFbc => 4,
        }
    }
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
pub fn adapt_signal(id: i32, force: u8, db_snr_map: f32, raw: &mut RawSignal) {
    let strength_init = i64::from(raw.strength);
    let snr_init = i64::from(raw.snr);
    let mut strength_new = strength_init;
    let mut snr_new = snr_init;
    let db_init = i64::from(raw.db);
    let mut db_new = db_init;
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

    ms_log::logm!(
        "adapt_signal adapter {}: strength {} (raw: {}),snr {} (raw: {}), dB {} (raw: {})",
        id,
        raw.strength,
        strength_init,
        raw.snr,
        snr_init,
        raw.db,
        db_init
    );
}

/// `dvb_read_status` (dvb.cpp:2106).
pub fn read_status<D: DvbDevice + ?Sized>(
    dev: &mut D,
    id: i32,
    frontend_open: bool,
) -> Option<i32> {
    if !frontend_open {
        return None;
    }
    match dev.read_status() {
        Ok(status) => Some(status),
        Err(error) => {
            let fd = dev.log_fd();
            let (errno, text) = error_parts(&error);
            ms_log::log!(
                "ad {} ioctl fd {} FE_READ_STATUS failed, error {} ({})",
                id,
                fd,
                errno,
                text
            );
            None
        }
    }
}

/// `get_signal_new` (dvb.cpp:2186): `None` when v5 cannot serve the poll.
pub fn signal_v5<D: DvbDevice + ?Sized>(
    dev: &mut D,
    id: i32,
    frontend_open: bool,
    db_snr_map: f32,
) -> Option<RawSignal> {
    if !frontend_open {
        return None;
    }
    let stats = match dev.get_stats() {
        Ok(stats) => stats,
        Err(error) => {
            let fd = dev.log_fd();
            let (_, text) = error_parts(&error);
            ms_log::logm!("get_signal_new: unable to query frontend {}: {}", fd, text);
            return None;
        }
    };
    if stats.iter().all(|s| s.len == 0) {
        return None;
    }
    let mut raw = RawSignal::default();
    let mut strengthd = 0i64;
    let mut snrd = 0i64;
    let mut init_strength = 0i64;
    let mut init_snr = 0i64;
    let mut strength_s = "(none)";
    let mut snr_s = "(none)";
    match stats[0].scale {
        FE_SCALE_RELATIVE => {
            strength_s = "%";
            strengthd = stats[0].value as i64;
        }
        FE_SCALE_DECIBEL => {
            strength_s = "dBm";
            init_strength = stats[0].svalue();
            strengthd = strength_decibels(init_strength);
        }
        _ => {}
    }
    match stats[1].scale {
        FE_SCALE_RELATIVE => {
            snr_s = "%";
            snrd = stats[1].value as i64;
        }
        FE_SCALE_DECIBEL => {
            snr_s = "dB";
            init_snr = stats[1].svalue();
            let (db, snr) = snr_decibels(init_snr, db_snr_map);
            raw.db = db as i32 as u16;
            snrd = snr;
        }
        _ => {}
    }
    raw.ber = (stats[2].value & 0x7fff) as u32;
    match read_status(dev, id, frontend_open) {
        Some(status) => {
            raw.status = status;
            raw.status_valid = true;
        }
        None => raw.status = 0,
    }
    ms_log::logm!(
        "get_signal_new adapter {}: status {}, strength {} {} -> {}, snr {} {} -> {}, BER: {}, dB: {}",
        id,
        raw.status,
        init_strength,
        strength_s,
        strengthd,
        init_snr,
        snr_s,
        snrd,
        raw.ber,
        raw.db
    );
    raw.strength = strengthd as i32 as u16;
    raw.snr = snrd as i32 as u16;
    Some(raw)
}

/// `dvb_name` (dvb.cpp:1940): frontend name, with the C++ failure log.
fn dvb_name<D: DvbDevice + ?Sized>(dev: &mut D, aid: i32) -> String {
    match dev.get_info() {
        Ok(info) => info.name,
        Err(error) => {
            let fd = dev.log_fd();
            let (_, text) = error_parts(&error);
            ms_log::log!(
                "FE_GET_INFO failed for adapter {}, fd {}: {} ",
                aid,
                fd,
                text
            );
            String::new()
        }
    }
}

/// `get_signal` (dvb.cpp:2119): v3 ioctls plus SNR calibration.
pub fn signal_v3<D: DvbDevice + ?Sized>(
    dev: &mut D,
    id: i32,
    pa: i32,
    frontend_open: bool,
    snr_calib: &mut SnrCalib,
    strength_warned: &mut bool,
    force: u8,
) -> RawSignal {
    let mut raw = RawSignal::default();
    let Some(status) = read_status(dev, id, frontend_open) else {
        return raw;
    };
    raw.status = status;
    raw.status_valid = true;
    if status != 0 {
        match dev.read_ber() {
            Err(error) => {
                let fd = dev.log_fd();
                let (errno, text) = error_parts(&error);
                ms_log::logm!(
                    "ad {} ioctl fd {}, FE_READ_BER failed, error {} ({})",
                    id,
                    fd,
                    errno,
                    text
                );
            }
            Ok(ber) => raw.ber = ber,
        }
        match dev.read_signal_strength() {
            Err(error) => {
                let fd = dev.log_fd();
                let (errno, text) = error_parts(&error);
                ms_log::logm!(
                    "ad {} ioctl fd {} FE_READ_SIGNAL_STRENGTH failed, error {} ({})",
                    id,
                    fd,
                    errno,
                    text
                );
            }
            Ok(strength) => raw.strength = strength,
        }
        match dev.read_snr() {
            Err(error) => {
                let fd = dev.log_fd();
                let (errno, text) = error_parts(&error);
                ms_log::logm!(
                    "ad {} ioctl fd {} FE_READ_SNR failed, error {} ({})",
                    id,
                    fd,
                    errno,
                    text
                );
            }
            Ok(snr) => {
                raw.snr = snr;
                if force == TUNER_FORCE_NO {
                    if *snr_calib == SnrCalib::Unknown {
                        let name = dvb_name(dev, pa);
                        *snr_calib = snr_calibration(&name);
                        ms_log::log!(
                            "ad {} tuner '{}' uses SNR calibration {}",
                            id,
                            name,
                            snr_calib.number()
                        );
                    }
                    let raw_snr = raw.snr;
                    raw.snr = calibrate_snr(*snr_calib, snr);
                    if raw.snr != raw_snr {
                        ms_log::logm!(
                            "get_signal adapter {}: SNR calibrated {} -> {}",
                            id,
                            raw_snr,
                            raw.snr
                        );
                    }
                    if matches!(*snr_calib, SnrCalib::BcmFbc | SnrCalib::NimFbc)
                        && raw.strength == 0
                        && raw.snr > 0
                        && status & FE_HAS_LOCK != 0
                    {
                        if !*strength_warned {
                            *strength_warned = true;
                            ms_log::log!(
                                "ad {} reports no signal strength while locked, deriving the reported level from the SNR (display fallback, the hardware provides no AGC)",
                                id
                            );
                        }
                        raw.strength = raw.snr;
                    }
                }
            }
        }
    }
    ms_log::logm!(
        "get_signal adapter {}: status {}, strength {}, snr {}, BER: {}",
        id,
        raw.status,
        raw.strength,
        raw.snr,
        raw.ber
    );
    raw
}

/// `strength * multiplier` assigned to `uint16_t`; out-of-range saturates (DEV-070).
pub fn apply_multiplier(value: u16, multiplier: f32) -> u16 {
    (f32::from(value) * multiplier) as u16
}
