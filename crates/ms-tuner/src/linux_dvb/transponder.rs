//! C++ `transponder`/`diseqc` structs in their original units, plus LOF and SNR-map helpers.

use super::consts::*;
use crate::{DeliverySystem, PidSet, Polarization, TuneParams};

/// `#define DEFAULT_LOG LOG_DVB` (dvb.cpp:53).
const DEFAULT_LOG: u32 = ms_log::LOG_DVB;

/// `struct diseqc` (dvb.h:286): switch, LNB and timing parameters of one adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiseqcParams {
    /// `SWITCH_UNICABLE`, `SWITCH_JESS`, `SWITCH_SLAVE` or 0.
    pub switch_type: i32,
    /// Unicable/JESS user-band slot.
    pub uslot: i32,
    /// Unicable/JESS user-band frequency in MHz.
    pub ufreq: i32,
    /// Unicable PIN, used when 1..=255.
    pub pin: i32,
    /// Unicable: stay at 13 V while sending.
    pub only13v: i32,
    /// Skip DiSEqC when the position does not change.
    pub fast: i32,
    /// DiSEqC address (second message byte).
    pub addr: i32,
    /// Committed command repetitions.
    pub committed_no: i32,
    /// Uncommitted command repetitions.
    pub uncommitted_no: i32,
    /// `-q` delay before the first command, ms.
    pub before_cmd: i32,
    /// `-q` delay after the first command, ms.
    pub after_cmd: i32,
    /// `-q` delay after each repeated command, ms.
    pub after_repeated_cmd: i32,
    /// `-q` delay after the switch commands, ms.
    pub after_switch: i32,
    /// `-q` delay after the tone burst, ms.
    pub after_burst: i32,
    /// `-q` delay after setting the 22 kHz tone, ms.
    pub after_tone: i32,
    /// Low-band LOF in kHz.
    pub lnb_low: i32,
    /// High-band LOF in kHz.
    pub lnb_high: i32,
    /// Circular LOF in kHz (0 disables).
    pub lnb_circular: i32,
    /// Band switch frequency in kHz (0 disables).
    pub lnb_switch: i32,
}

impl Default for DiseqcParams {
    // adapter_alloc (adapter.cpp:80-105) with the opts.cpp defaults.
    fn default() -> Self {
        Self {
            switch_type: 0,
            uslot: 0,
            ufreq: 0,
            pin: 0,
            only13v: 0,
            fast: 0,
            addr: 0x10,
            committed_no: 1,
            uncommitted_no: 0,
            before_cmd: 15,
            after_cmd: 54,
            after_repeated_cmd: 15,
            after_switch: 15,
            after_burst: 15,
            after_tone: 0,
            lnb_low: 9_750_000,
            lnb_high: 10_600_000,
            lnb_circular: 10_750_000,
            lnb_switch: 11_700_000,
        }
    }
}

/// `struct transponder` (dvb.h:311) with C++ units: kHz, sym/s, Hz bandwidth.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DvbTransponder {
    pub sys: Option<i32>,
    pub freq: Option<i32>,
    pub inversion: Option<i32>,
    pub mtype: Option<i32>,
    pub fe: Option<i32>,
    pub hprate: Option<i32>,
    pub tmode: Option<i32>,
    pub gi: Option<i32>,
    pub bw: Option<i32>,
    pub sm: Option<i32>,
    pub t2id: Option<i32>,
    pub ro: Option<i32>,
    pub plts: Option<i32>,
    pub fec: Option<i32>,
    pub sr: Option<i32>,
    /// 1 = v, 2 = h, 3 = r, 4 = l (`fe_pol_map`).
    pub pol: Option<i32>,
    /// 1-based switch position (`src`).
    pub diseqc: Option<i32>,
    pub diseqc_param: DiseqcParams,
    pub c2tft: Option<i32>,
    pub ds: Option<i32>,
    pub plp_isi: Option<i32>,
    pub pls_mode: Option<i32>,
    pub pls_code: Option<i32>,
    pub pids: PidSet,
    pub x_pmt: Option<i32>,
}

/// `fe_delivery_system_t` number of a [`DeliverySystem`].
pub fn delsys_number(sys: DeliverySystem) -> Option<i32> {
    match sys {
        DeliverySystem::DvbS => Some(SYS_DVBS),
        DeliverySystem::DvbS2 => Some(SYS_DVBS2),
        DeliverySystem::DvbT => Some(SYS_DVBT),
        DeliverySystem::DvbT2 => Some(SYS_DVBT2),
        DeliverySystem::DvbC => Some(SYS_DVBC_ANNEX_A),
        DeliverySystem::DvbC2 => Some(SYS_DVBC2),
        DeliverySystem::Undefined => None,
        DeliverySystem::Other(n) => Some(n),
    }
}

impl DvbTransponder {
    /// Copies SAT>IP parameters (already kHz and sym/s) and applies `copy_dvb_parameters` defaults.
    pub fn from_params(p: &TuneParams, diseqc_param: DiseqcParams) -> Self {
        let pol = match p.pol {
            Polarization::Vertical => Some(1),
            Polarization::Horizontal => Some(2),
            Polarization::Right => Some(3),
            Polarization::Left => Some(4),
            // An explicit `pol=0` is the C++ optional value 0, not nullopt.
            Polarization::None => p.pol_specified.then_some(0),
        };
        let mut tp = Self {
            sys: delsys_number(p.msys),
            // TuneParams already uses the C++ transponder units: kHz.
            freq: (p.freq > 0).then_some(p.freq),
            inversion: p.specinv,
            mtype: p.mtype_specified.then_some(p.mtype as i32),
            fe: p.fe,
            tmode: p.tmode,
            gi: p.gi,
            bw: p.bw.map(|bw| i32::try_from(bw).unwrap_or(0)),
            sm: p.sm,
            t2id: p.t2id,
            ro: p.ro.map(i32::from),
            plts: p.plts.map(|on| if on { PILOT_ON } else { PILOT_OFF }),
            fec: p.fec_specified.then_some(i32::from(p.fec)),
            sr: (p.sr > 0).then(|| i32::try_from(p.sr).unwrap_or(i32::MAX)),
            pol,
            diseqc: (p.src_specified && p.src > 0).then_some(i32::from(p.src)),
            diseqc_param,
            c2tft: p.c2tft,
            ds: p.ds,
            plp_isi: p.plp,
            pls_mode: p.plsm,
            pls_code: p.plsc,
            pids: p.pids.clone(),
            x_pmt: p.x_pmt,
            hprate: None,
        };
        tp.apply_copy_defaults();
        tp
    }

    /// Defaults `copy_dvb_parameters` adds (dvb.cpp:453-460).
    pub fn apply_copy_defaults(&mut self) {
        if self.diseqc.unwrap_or(0) < 1 {
            self.diseqc = Some(1);
        }
        if self.sys == Some(SYS_DVBS2) && self.mtype.is_none() {
            self.mtype = Some(PSK_8);
        }
        if self.sys == Some(SYS_DVBS) && self.mtype.is_none() {
            self.mtype = Some(QPSK);
        }
    }

    /// Whether this is a satellite delivery system (DVB-S/S2).
    pub fn is_sat(&self) -> bool {
        matches!(self.sys, Some(SYS_DVBS | SYS_DVBS2))
    }
}

/// `get_lnb_hiband` (adapter.cpp:1294).
pub fn lnb_hiband(tp: &DvbTransponder, d: &DiseqcParams) -> i32 {
    if tp.pol.unwrap_or(0) > 2 && d.lnb_circular > 0 {
        return 0;
    }
    i32::from(d.lnb_switch > 0 && tp.freq.unwrap_or(0) > d.lnb_switch)
}

/// `get_lnb_int_freq` (adapter.cpp:1303): intermediate frequency in kHz.
pub fn lnb_int_freq(tp: &DvbTransponder, d: &DiseqcParams) -> i32 {
    let freq = tp.freq.unwrap_or(0);
    if tp.pol.unwrap_or(0) > 2 && d.lnb_circular > 0 {
        return freq.wrapping_sub(d.lnb_circular);
    }
    if d.lnb_switch > 0 && freq > d.lnb_switch {
        return freq.wrapping_sub(d.lnb_high);
    }
    freq.wrapping_sub(d.lnb_low).wrapping_abs()
}

/// `get_db_snr_map` (dvb.cpp:475): dB value that maps to 100 % SNR.
pub fn db_snr_map(tp: &DvbTransponder) -> f32 {
    let sys = tp.sys.unwrap_or(SYS_UNDEFINED);
    let mtype = tp.mtype.unwrap_or(QAM_AUTO);
    let fec = tp.fec.unwrap_or(FEC_AUTO);
    let top: i32 = match sys {
        // SYS_DVBC_ANNEX_AC and SYS_DVBC_ANNEX_B
        SYS_DVBC_ANNEX_A | SYS_DVBC_ANNEX_B => match mtype {
            QAM_256 => 340,
            QAM_64 => 300,
            _ => {
                ms_log::log!("get_db_snr_map -> DVB-C modulation SNR scale not tested!!");
                360
            }
        },
        SYS_DVBS => match mtype {
            QPSK => match fec {
                FEC_1_2 => 70,
                FEC_2_3 => 90,
                FEC_3_4 => 100,
                FEC_5_6 => 110,
                FEC_7_8 => 120,
                FEC_AUTO => 130,
                _ => {
                    ms_log::log!("get_db_snr_map -> DVB-S modulation SNR scale not tested!!");
                    130
                }
            },
            _ => {
                ms_log::log!("get_db_snr_map -> DVB-S modulation SNR scale unkown!!");
                800
            }
        },
        SYS_DVBS2 => match mtype {
            QPSK => match fec {
                FEC_1_2 => 90,
                FEC_2_3 => 110,
                FEC_3_4 => 120,
                FEC_4_5 => 130,
                FEC_5_6 => 120,
                FEC_8_9 => 130,
                FEC_9_10 => 135,
                FEC_AUTO => 130,
                _ => {
                    ms_log::log!("get_db_snr_map -> DVB-S2 modulation SNR scale not tested!!");
                    130
                }
            },
            PSK_8 => match fec {
                FEC_1_2 => 128,
                FEC_2_3 => 145,
                FEC_3_4 => 160,
                FEC_4_5 => 180,
                FEC_5_6 => 175,
                FEC_8_9 => 190,
                FEC_9_10 => 200,
                FEC_AUTO => 180,
                _ => {
                    ms_log::log!("get_db_snr_map -> DVB-S2 modulation SNR scale not tested!!");
                    180
                }
            },
            _ => {
                ms_log::log!("get_db_snr_map -> DVB-S2 modulation SNR scale unkown!!");
                800
            }
        },
        SYS_DVBT => match mtype {
            QPSK => match fec {
                FEC_1_2 => 41,
                FEC_2_3 => 61,
                FEC_3_4 => 72,
                FEC_5_6 => 85,
                FEC_7_8 => 92,
                _ => {
                    ms_log::log!("get_db_snr_map -> DVB-T modulation SNR scale not tested!!");
                    110
                }
            },
            QAM_16 => match fec {
                FEC_1_2 => 98,
                FEC_2_3 => 121,
                FEC_3_4 => 134,
                FEC_5_6 => 148,
                FEC_7_8 => 157,
                _ => {
                    ms_log::log!("get_db_snr_map -> DVB-T modulation SNR scale not tested!!");
                    180
                }
            },
            QAM_64 => match fec {
                FEC_1_2 => 140,
                FEC_2_3 => 199,
                FEC_3_4 => 249,
                FEC_5_6 => 213,
                FEC_7_8 => 220,
                _ => {
                    ms_log::log!("get_db_snr_map -> DVB-T modulation SNR scale not tested!!");
                    250
                }
            },
            _ => {
                ms_log::log!("get_db_snr_map -> DVB-T modulation SNR scale unkown!!");
                800
            }
        },
        SYS_DVBT2 => match mtype {
            QPSK => match fec {
                FEC_1_2 => 28,
                FEC_3_5 => 47,
                FEC_2_3 => 59,
                FEC_3_4 => 76,
                FEC_4_5 => 89,
                FEC_5_6 => 98,
                _ => {
                    ms_log::log!("get_db_snr_map -> DVB-T2 modulation SNR scale not tested!!");
                    110
                }
            },
            QAM_16 => match fec {
                FEC_1_2 => 81,
                FEC_3_5 => 101,
                FEC_2_3 => 115,
                FEC_3_4 => 133,
                FEC_4_5 => 147,
                FEC_5_6 => 156,
                _ => {
                    ms_log::log!("get_db_snr_map -> DVB-T2 modulation SNR scale not tested!!");
                    180
                }
            },
            QAM_64 => match fec {
                FEC_1_2 => 126,
                FEC_3_5 => 147,
                FEC_2_3 => 162,
                FEC_3_4 => 182,
                FEC_4_5 => 197,
                FEC_5_6 => 207,
                _ => {
                    ms_log::log!("get_db_snr_map -> DVB-T2 modulation SNR scale not tested!!");
                    230
                }
            },
            QAM_256 => match fec {
                FEC_1_2 => 165,
                FEC_3_5 => 190,
                FEC_2_3 => 206,
                FEC_3_4 => 229,
                FEC_4_5 => 246,
                FEC_5_6 => 256,
                _ => {
                    ms_log::log!("get_db_snr_map -> DVB-T2 modulation SNR scale not tested!!");
                    280
                }
            },
            _ => {
                ms_log::log!("get_db_snr_map -> DVB-T2 modulation SNR scale unkown!!");
                800
            }
        },
        _ => {
            ms_log::log!("get_db_snr_map -> Modulation SNR scale unkown!!");
            800
        }
    };
    let db_map = (f64::from(top) / 10.0) as f32;
    ms_log::logm!(
        "get_db_snr_map -> src={}, fe={}, msys={}, mtype={}, fec={}, db_map={:.6}",
        tp.diseqc.unwrap_or(0),
        tp.fe.unwrap_or(0),
        sys,
        mtype,
        fec,
        db_map
    );
    db_map
}

/// Absolute source mapping (`--virtual-diseqc`, adapter.cpp:681-705 and 1798-1816).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbsoluteSources {
    table: [i8; MAX_SOURCES],
    enabled: bool,
}

impl Default for AbsoluteSources {
    fn default() -> Self {
        Self {
            table: [0; MAX_SOURCES],
            enabled: false,
        }
    }
}

impl AbsoluteSources {
    /// Applies one `src[-range]:adapter:pos` entry for this adapter (`absolute_table[src] = pos + cnt`).
    pub fn set(&mut self, src: i32, range: i32, pos: i32) {
        let max = MAX_SOURCES as i32;
        if !(0..max).contains(&src) || !(0..max).contains(&range) || !(0..15).contains(&pos) {
            return;
        }
        for (cnt, s) in (src..=range).enumerate() {
            self.table[s as usize] = (pos + cnt as i32 + 1) as i8;
        }
        self.enabled = true;
    }

    /// Marks the global `absolute_switch` as set (any adapter got an entry).
    pub fn enable(&mut self) {
        self.enabled = true;
    }

    /// `source_enabled_for_adapter`: whether this adapter can receive `tp`.
    pub fn source_enabled(&self, tp: &DvbTransponder) -> bool {
        if !self.enabled || !tp.is_sat() {
            return true;
        }
        let diseqc = tp.diseqc.unwrap_or(0);
        if diseqc <= 0 || diseqc > MAX_SOURCES as i32 {
            return true;
        }
        self.table[(diseqc - 1) as usize] != 0
    }

    /// `get_absolute_source_for_adapter`: the switch position this adapter uses for `src`.
    pub fn map(&self, src: i32, sys: i32) -> i32 {
        if !self.enabled || (sys != SYS_DVBS && sys != SYS_DVBS2) {
            return src;
        }
        if src <= 0 || src > MAX_SOURCES as i32 {
            return src;
        }
        i32::from(self.table[(src - 1) as usize])
    }
}
