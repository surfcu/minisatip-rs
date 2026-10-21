//! `netcv_commit` tuning: SAT>IP parameters to libmcli `recv_tune` arguments.

use crate::tuner::{DeliverySystem, Polarization, TuneParams};

/// `fe_type_t` values used by libmcli.
pub const FE_QPSK: i32 = 0;
/// `FE_QAM`.
pub const FE_QAM: i32 = 1;
/// `FE_OFDM`.
pub const FE_OFDM: i32 = 2;
/// `FE_ATSC`.
pub const FE_ATSC: i32 = 3;
/// libmcli `FE_DVBS2` (`FE_ATSC + 1`).
pub const FE_DVBS2: i32 = FE_ATSC + 1;

/// libmcli S2 FEC numbering for 3/5 (`FEC_3_5` in defs.h).
pub const MCLI_FEC_3_5: i32 = 13;
/// libmcli S2 FEC numbering for 9/10.
pub const MCLI_FEC_9_10: i32 = 14;
/// libmcli `QPSK_S2` modulation, shifted into `fec_inner` bits 16..
pub const MCLI_QPSK_S2: i32 = 9;
/// libmcli `PSK8`.
pub const MCLI_PSK8: i32 = 10;

const FEC_NONE: i32 = 0;
pub(crate) const FEC_AUTO: i32 = 9;
const FEC_3_5: i32 = 10;
const FEC_9_10: i32 = 11;
const QPSK: i32 = 0;
const QAM_32: i32 = 2;
const QAM_256: i32 = 5;
pub(crate) const QAM_AUTO: i32 = 6;
pub(crate) const INVERSION_AUTO: i32 = 2;
pub(crate) const TRANSMISSION_MODE_AUTO: i32 = 2;
pub(crate) const GUARD_INTERVAL_AUTO: i32 = 4;
const HIERARCHY_NONE: i32 = 0;

/// `fe_bandwidth_t`.
pub const BANDWIDTH_8_MHZ: i32 = 0;
/// `BANDWIDTH_7_MHZ`.
pub const BANDWIDTH_7_MHZ: i32 = 1;
/// `BANDWIDTH_6_MHZ`.
pub const BANDWIDTH_6_MHZ: i32 = 2;
/// `BANDWIDTH_AUTO`.
pub const BANDWIDTH_AUTO: i32 = 3;
/// `BANDWIDTH_5_MHZ`.
pub const BANDWIDTH_5_MHZ: i32 = 4;
/// `BANDWIDTH_10_MHZ`.
pub const BANDWIDTH_10_MHZ: i32 = 5;
/// `BANDWIDTH_1_712_MHZ`.
pub const BANDWIDTH_1_712_MHZ: i32 = 6;

/// `fe_sec_voltage_t`: `SEC_VOLTAGE_13`.
pub const SEC_VOLTAGE_13: i32 = 0;
/// `SEC_VOLTAGE_18`.
pub const SEC_VOLTAGE_18: i32 = 1;
/// `SEC_VOLTAGE_OFF`.
pub const SEC_VOLTAGE_OFF: i32 = 2;

/// Default positions per `src` (19.2E, 13E, 28.2E, 5W), added to 1800.
const MAP_POS: [i32; 5] = [0, 192, 130, 282, -50];
/// Voltage per C++ polarisation number (none, v, h, r).
const MAP_POL: [i32; 4] = [0, SEC_VOLTAGE_13, SEC_VOLTAGE_18, SEC_VOLTAGE_OFF];

/// Union member of `struct dvb_frontend_parameters` that `netcv_commit` fills.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FrontendUnion {
    /// Nothing written (unsupported delivery system).
    #[default]
    None,
    /// `u.qpsk`.
    Qpsk {
        /// `symbol_rate`.
        symbol_rate: u32,
        /// `fec_inner`, with the S2 modulation in bits 16..
        fec_inner: i32,
    },
    /// `u.qam`.
    Qam {
        /// `symbol_rate`.
        symbol_rate: u32,
        /// `fec_inner`.
        fec_inner: i32,
        /// `modulation`.
        modulation: i32,
    },
    /// `u.ofdm`.
    Ofdm {
        /// `bandwidth` (`fe_bandwidth_t`).
        bandwidth: i32,
        /// `code_rate_HP`.
        code_rate_hp: i32,
        /// `code_rate_LP`.
        code_rate_lp: i32,
        /// `constellation`.
        constellation: i32,
        /// `transmission_mode`.
        transmission_mode: i32,
        /// `guard_interval`.
        guard_interval: i32,
        /// `hierarchy_information`.
        hierarchy: i32,
    },
}

/// `struct dvb_frontend_parameters`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FrontendParams {
    /// kHz for QPSK, Hz otherwise.
    pub frequency: u32,
    /// `inversion`.
    pub inversion: i32,
    /// Delivery-specific part.
    pub u: FrontendUnion,
}

/// Arguments of `recv_tune(r, type, satpos, &sec, &fep, pids)` (pids is always empty).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct McliTune {
    /// `fe_type_t` (or [`FE_DVBS2`]).
    pub fe_type: i32,
    /// Orbital position in 0.1 degrees + 1800, or `0xfff` for cable.
    pub satpos: i32,
    /// `recv_sec_t::voltage`; the other SEC fields stay zero.
    pub voltage: i32,
    /// Frontend parameters.
    pub fep: FrontendParams,
}

fn pol_number(p: &TuneParams) -> i32 {
    match p.pol {
        Polarization::None => 0,
        Polarization::Vertical => 1,
        Polarization::Horizontal => 2,
        Polarization::Right => 3,
        Polarization::Left => 4,
    }
}

fn bandwidth(bw: i32) -> i32 {
    match bw {
        8_000_000 => BANDWIDTH_8_MHZ,
        7_000_000 => BANDWIDTH_7_MHZ,
        6_000_000 => BANDWIDTH_6_MHZ,
        0 => BANDWIDTH_AUTO,
        5_000_000 => BANDWIDTH_5_MHZ,
        10_000_000 => BANDWIDTH_10_MHZ,
        1_712_000 => BANDWIDTH_1_712_MHZ,
        _ => BANDWIDTH_AUTO,
    }
}

/// Values `netcv_commit` reads from `ad->tp` before choosing a delivery system
/// (netceiver.cpp:164-184). Shared by [`mcli_tune`] and the tuning log lines so
/// both use the same defaults and clamps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TuneInputs {
    /// Switch position, clamped to 0..=4.
    pub diseqc: i32,
    /// `fe_pol_map` number, clamped to 0..=3.
    pub pol: i32,
    /// Transponder frequency in kHz.
    pub freq: i32,
    /// Symbol rate in sym/s.
    pub sr: u32,
    /// `fe_code_rate_t`.
    pub fec: i32,
    /// `fe_modulation_t`.
    pub mtype: i32,
    /// Bandwidth in Hz.
    pub bw: i32,
    /// `fe_transmit_mode_t`.
    pub tmode: i32,
    /// `fe_guard_interval_t`.
    pub gi: i32,
    /// `fe_spectral_inversion_t`.
    pub inversion: i32,
    /// `hprate` as `tp->hprate.value_or(FEC_AUTO)`. `detect_dvb_parameters`
    /// never sets `hprate`, so this is always `FEC_AUTO`.
    pub hprate: i32,
    /// Stream/ISI id.
    pub sm: i32,
    /// T2 transponder id.
    pub t2id: i32,
}

/// Reads the `netcv_commit` tuning values with the C++ defaults.
pub(crate) fn tune_inputs(tp: &TuneParams) -> TuneInputs {
    let diseqc = if tp.src_specified {
        i32::from(tp.src)
    } else {
        0
    };
    let diseqc = if (0..=4).contains(&diseqc) { diseqc } else { 0 };
    let pol = pol_number(tp);
    let pol = if (0..=3).contains(&pol) { pol } else { 0 };
    TuneInputs {
        diseqc,
        pol,
        freq: tp.freq,
        sr: tp.sr,
        fec: if tp.fec_specified {
            i32::from(tp.fec)
        } else {
            FEC_AUTO
        },
        mtype: if tp.mtype_specified {
            tp.mtype as i32
        } else {
            QAM_AUTO
        },
        bw: tp.bw.map_or(8_000_000, |b| b as i32),
        tmode: tp.tmode.unwrap_or(TRANSMISSION_MODE_AUTO),
        gi: tp.gi.unwrap_or(GUARD_INTERVAL_AUTO),
        inversion: tp.specinv.unwrap_or(INVERSION_AUTO),
        hprate: FEC_AUTO,
        sm: tp.sm.unwrap_or(0),
        t2id: tp.t2id.unwrap_or(0),
    }
}

/// The `want_tune` branch of `netcv_commit` (netceiver.cpp:163-344).
pub fn mcli_tune(tp: &TuneParams) -> McliTune {
    let inputs = tune_inputs(tp);
    let TuneInputs {
        diseqc,
        pol,
        freq,
        sr,
        fec,
        mtype,
        bw,
        tmode,
        gi,
        ..
    } = inputs;
    let mut out = McliTune::default();
    match tp.msys {
        DeliverySystem::DvbS | DeliverySystem::DvbS2 => {
            out.satpos = 1800 + MAP_POS[diseqc as usize];
            out.voltage = MAP_POL[pol as usize];
            let fec_inner = if tp.msys == DeliverySystem::DvbS {
                out.fe_type = FE_QPSK;
                fec
            } else {
                out.fe_type = FE_DVBS2;
                let fec = match fec {
                    FEC_3_5 => MCLI_FEC_3_5,
                    FEC_9_10 => MCLI_FEC_9_10,
                    other => other,
                };
                let modulation = if mtype != 0 { MCLI_PSK8 } else { MCLI_QPSK_S2 };
                fec | (modulation << 16)
            };
            out.fep = FrontendParams {
                frequency: freq as u32,
                inversion: INVERSION_AUTO,
                u: FrontendUnion::Qpsk {
                    symbol_rate: sr,
                    fec_inner,
                },
            };
        }
        DeliverySystem::DvbC => {
            out.satpos = 0xfff;
            out.fe_type = FE_QAM;
            out.fep = FrontendParams {
                frequency: freq.wrapping_mul(1000) as u32,
                inversion: INVERSION_AUTO,
                u: FrontendUnion::Qam {
                    symbol_rate: sr,
                    fec_inner: FEC_NONE,
                    modulation: if mtype == QPSK { QAM_256 } else { mtype },
                },
            };
        }
        DeliverySystem::DvbT => {
            out.fe_type = FE_OFDM;
            out.fep = FrontendParams {
                frequency: freq.wrapping_mul(1000) as u32,
                inversion: INVERSION_AUTO,
                u: FrontendUnion::Ofdm {
                    bandwidth: bandwidth(bw),
                    code_rate_hp: FEC_AUTO,
                    code_rate_lp: FEC_AUTO,
                    constellation: match mtype {
                        0 => QAM_AUTO,
                        6 => QAM_32,
                        other => other,
                    },
                    transmission_mode: tmode,
                    guard_interval: gi,
                    hierarchy: HIERARCHY_NONE,
                },
            };
        }
        // C++ leaves m_sec/m_fep uninitialised here and tunes with type 0 anyway.
        _ => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sat(msys: DeliverySystem) -> TuneParams {
        TuneParams {
            freq: 11_362_000,
            msys,
            sr: 22_000_000,
            pol: Polarization::Horizontal,
            pol_specified: true,
            src: 1,
            src_specified: true,
            fec: 2,
            fec_specified: true,
            ..TuneParams::default()
        }
    }

    #[test]
    fn dvbs_uses_satpos_voltage_and_plain_fec() {
        let t = mcli_tune(&sat(DeliverySystem::DvbS));
        assert_eq!(t.fe_type, FE_QPSK);
        assert_eq!(t.satpos, 1800 + 192);
        assert_eq!(t.voltage, SEC_VOLTAGE_18);
        assert_eq!(
            t.fep,
            FrontendParams {
                frequency: 11_362_000,
                inversion: INVERSION_AUTO,
                u: FrontendUnion::Qpsk {
                    symbol_rate: 22_000_000,
                    fec_inner: 2
                }
            }
        );
    }

    #[test]
    fn dvbs2_renumbers_fec_and_packs_modulation() {
        let mut p = sat(DeliverySystem::DvbS2);
        p.fec = FEC_3_5 as u8;
        p.mtype = 0;
        p.mtype_specified = true;
        let t = mcli_tune(&p);
        assert_eq!(t.fe_type, FE_DVBS2);
        assert_eq!(
            t.fep.u,
            FrontendUnion::Qpsk {
                symbol_rate: 22_000_000,
                fec_inner: MCLI_FEC_3_5 | (MCLI_QPSK_S2 << 16)
            }
        );
        // Unspecified mtype is QAM_AUTO (non-zero): PSK8.
        p.fec = FEC_9_10 as u8;
        p.mtype_specified = false;
        assert_eq!(
            mcli_tune(&p).fep.u,
            FrontendUnion::Qpsk {
                symbol_rate: 22_000_000,
                fec_inner: MCLI_FEC_9_10 | (MCLI_PSK8 << 16)
            }
        );
        p.fec_specified = false;
        assert_eq!(
            mcli_tune(&p).fep.u,
            FrontendUnion::Qpsk {
                symbol_rate: 22_000_000,
                fec_inner: FEC_AUTO | (MCLI_PSK8 << 16)
            }
        );
    }

    #[test]
    fn source_and_polarisation_maps_clamp_like_cpp() {
        let mut p = sat(DeliverySystem::DvbS);
        for (src, pos) in [(0, 1800), (2, 1930), (3, 2082), (4, 1750), (5, 1800)] {
            p.src = src;
            assert_eq!(mcli_tune(&p).satpos, pos, "src={src}");
        }
        p.src_specified = false;
        p.src = 3;
        assert_eq!(mcli_tune(&p).satpos, 1800);
        for (pol, v) in [
            (Polarization::None, 0),
            (Polarization::Vertical, SEC_VOLTAGE_13),
            (Polarization::Horizontal, SEC_VOLTAGE_18),
            (Polarization::Right, SEC_VOLTAGE_OFF),
            (Polarization::Left, 0),
        ] {
            p.pol = pol;
            assert_eq!(mcli_tune(&p).voltage, v, "{pol:?}");
        }
    }

    #[test]
    fn dvbc_scales_frequency_and_forces_qam256_for_qpsk() {
        let mut p = TuneParams {
            freq: 346_000,
            msys: DeliverySystem::DvbC,
            sr: 6_900_000,
            mtype: QPSK as u32,
            mtype_specified: true,
            ..TuneParams::default()
        };
        let t = mcli_tune(&p);
        assert_eq!((t.fe_type, t.satpos, t.voltage), (FE_QAM, 0xfff, 0));
        assert_eq!(t.fep.frequency, 346_000_000);
        assert_eq!(
            t.fep.u,
            FrontendUnion::Qam {
                symbol_rate: 6_900_000,
                fec_inner: FEC_NONE,
                modulation: QAM_256
            }
        );
        p.mtype = 3;
        assert!(matches!(
            mcli_tune(&p).fep.u,
            FrontendUnion::Qam { modulation: 3, .. }
        ));
        p.mtype_specified = false;
        assert!(matches!(
            mcli_tune(&p).fep.u,
            FrontendUnion::Qam {
                modulation: QAM_AUTO,
                ..
            }
        ));
    }

    #[test]
    fn dvbt_maps_bandwidth_and_constellation_quirks() {
        let mut p = TuneParams {
            freq: 506_000,
            msys: DeliverySystem::DvbT,
            ..TuneParams::default()
        };
        let t = mcli_tune(&p);
        assert_eq!((t.fe_type, t.satpos), (FE_OFDM, 0));
        assert_eq!(t.fep.frequency, 506_000_000);
        assert_eq!(
            t.fep.u,
            FrontendUnion::Ofdm {
                bandwidth: BANDWIDTH_8_MHZ,
                code_rate_hp: FEC_AUTO,
                code_rate_lp: FEC_AUTO,
                // Unspecified mtype is 6, which C++ maps to QAM_32.
                constellation: QAM_32,
                transmission_mode: TRANSMISSION_MODE_AUTO,
                guard_interval: GUARD_INTERVAL_AUTO,
                hierarchy: HIERARCHY_NONE
            }
        );
        for (bw, expected) in [
            (7_000_000, BANDWIDTH_7_MHZ),
            (6_000_000, BANDWIDTH_6_MHZ),
            (0, BANDWIDTH_AUTO),
            (5_000_000, BANDWIDTH_5_MHZ),
            (10_000_000, BANDWIDTH_10_MHZ),
            (1_712_000, BANDWIDTH_1_712_MHZ),
            (1_234, BANDWIDTH_AUTO),
        ] {
            p.bw = Some(bw);
            assert!(
                matches!(mcli_tune(&p).fep.u, FrontendUnion::Ofdm { bandwidth, .. } if bandwidth == expected),
                "bw={bw}"
            );
        }
        p.mtype = 0;
        p.mtype_specified = true;
        p.tmode = Some(1);
        p.gi = Some(2);
        let FrontendUnion::Ofdm {
            constellation,
            transmission_mode,
            guard_interval,
            ..
        } = mcli_tune(&p).fep.u
        else {
            panic!("ofdm expected");
        };
        assert_eq!(
            (constellation, transmission_mode, guard_interval),
            (QAM_AUTO, 1, 2)
        );
    }

    #[test]
    fn unsupported_delivery_system_tunes_type_zero() {
        let p = TuneParams {
            freq: 600_000,
            msys: DeliverySystem::DvbT2,
            ..TuneParams::default()
        };
        assert_eq!(mcli_tune(&p), McliTune::default());
    }
}
