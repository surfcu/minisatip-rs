//! Per-adapter DVB state and the ports of `dvb_tune` / `dvb_get_signal`.

use super::consts::*;
use super::device::{DtvProperty, DvbDevice, error_parts};
use crate::names;

/// `#define DEFAULT_LOG LOG_DVB` (dvb.cpp:53).
const DEFAULT_LOG: u32 = ms_log::LOG_DVB;
use super::diseqc::{MasterView, SwitchOptions, setup_switch};
use super::signal::{
    LOCK_RETRY_INITIAL_MS, LOCK_RETRY_INTERVAL_MS, LOCK_RETRY_MAX, NEW_SIGNAL, OLD_SIGNAL,
    RawSignal, SIGNAL_UNPROBED, SnrCalib, adapt_signal, apply_multiplier, read_status, signal_v3,
    signal_v5,
};
use super::transponder::{DiseqcParams, DvbTransponder, db_snr_map};

/// Fields of the C++ `adapter` that `dvb.cpp` reads or writes.
///
/// The default value matches a zero-initialised C++ `adapter` as used by the
/// tests; the real backend starts from [`AdapterState::alloc`].
#[derive(Debug, Clone)]
pub struct AdapterState {
    pub id: i32,
    /// `/dev/dvb/adapterN` number (`ad->pa`), used by `dvb_name`'s log line.
    pub pa: i32,
    /// `frontendM` number (`ad->fn`), used by the demux log lines.
    pub fn_num: i32,
    /// Frontend fd (`ad->fe`); `<= 0` means no usable frontend.
    pub fe: i32,
    pub sid_cnt: i32,
    /// Master adapter id, `-1` when independent.
    pub master_source: i32,
    pub status: i32,
    pub ber: u32,
    pub strength: u16,
    pub snr: u16,
    pub db: u16,
    pub new_gs: u8,
    pub strength_multiplier: f32,
    pub snr_multiplier: f32,
    /// `TUNER_FORCE_*` bits.
    pub force_tuner_signal: u8,
    pub db_snr_map: f32,
    pub snr_calib: SnrCalib,
    pub strength_warned: bool,
    pub old_pol: i32,
    pub old_hiband: i32,
    pub old_diseqc: i32,
    pub diseqc_multi: i32,
    pub lock_retry_armed: bool,
    pub lock_retries: i32,
    pub lock_retry_time: i64,
    /// `ad->tune_time`: nonzero once a tune completed since the adapter opened.
    pub tune_time: i64,
    pub tp: DvbTransponder,
}

impl Default for AdapterState {
    fn default() -> Self {
        Self {
            id: 0,
            pa: 0,
            fn_num: 0,
            fe: 0,
            sid_cnt: 0,
            master_source: -1,
            status: 0,
            ber: 0,
            strength: 0,
            snr: 0,
            db: 0,
            new_gs: SIGNAL_UNPROBED,
            strength_multiplier: 0.0,
            snr_multiplier: 0.0,
            force_tuner_signal: TUNER_FORCE_NO,
            db_snr_map: 0.0,
            snr_calib: SnrCalib::Unknown,
            strength_warned: false,
            old_pol: -1,
            old_hiband: -1,
            old_diseqc: -1,
            diseqc_multi: -1,
            lock_retry_armed: false,
            lock_retries: 0,
            lock_retry_time: 0,
            tune_time: 0,
            tp: DvbTransponder::default(),
        }
    }
}

impl AdapterState {
    /// State of a freshly allocated adapter (`adapter_alloc`, adapter.cpp:76).
    pub fn alloc() -> Self {
        Self {
            strength_multiplier: 1.0,
            snr_multiplier: 1.0,
            db_snr_map: 1.0,
            ..Self::default()
        }
    }

    /// `adapter_alloc` with explicit DiSEqC/LNB parameters.
    pub fn with_diseqc(diseqc_param: DiseqcParams) -> Self {
        let mut state = Self::alloc();
        state.tp.diseqc_param = diseqc_param;
        state
    }

    /// `setup_switch` (dvb.cpp:1135) against `dev` (the master frontend for a slave).
    pub fn setup_switch<D: super::device::SecDevice + ?Sized>(
        &mut self,
        dev: &mut D,
        force: bool,
        master: Option<MasterView<'_>>,
    ) -> i32 {
        let mut own = super::diseqc::SwitchState {
            pol: self.old_pol,
            hiband: self.old_hiband,
            diseqc: self.old_diseqc,
        };
        let options = SwitchOptions {
            own_id: self.id,
            force,
            tuned_before: self.tune_time != 0,
            diseqc_multi: self.diseqc_multi,
        };
        let freq = setup_switch(dev, &self.tp, &mut own, master, options);
        self.old_pol = own.pol;
        self.old_hiband = own.hiband;
        self.old_diseqc = own.diseqc;
        freq
    }

    /// `dvb_tune` (dvb.cpp:1262) for an independent adapter.
    pub fn tune<D: DvbDevice + ?Sized>(&mut self, dev: &mut D, tp: &DvbTransponder) -> i32 {
        self.tune_with_master(dev, tp, None)
    }

    /// `dvb_tune` with the switch state of a master adapter for a slave.
    pub fn tune_with_master<D: DvbDevice + ?Sized>(
        &mut self,
        dev: &mut D,
        tp: &DvbTransponder,
        master: Option<MasterView<'_>>,
    ) -> i32 {
        self.new_gs = SIGNAL_UNPROBED;
        self.lock_retry_armed = false;
        self.lock_retries = 0;
        if self.fe < 0 {
            return -500;
        }
        self.tp = tp.clone();

        let clear_start = dev.now_ms();
        if let Err(error) = dev.set_properties(&[DtvProperty::new(DTV_CLEAR, 0)]) {
            let (errno, text) = error_parts(&error);
            ms_log::log!("dvb_tune: DTV_CLEAR failed {} {}, continuing", errno, text);
        }
        let bclear = dev.now_ms() - clear_start;

        let mut freq = tp.freq.unwrap_or(0);
        let mut props: Vec<DtvProperty> = Vec::with_capacity(20);
        let add = |props: &mut Vec<DtvProperty>, prop: DtvProperty| props.push(prop);
        match tp.sys.unwrap_or(SYS_UNDEFINED) {
            SYS_DVBS | SYS_DVBS2 => {
                let bpol = dev.now_ms();
                freq = self.setup_switch(dev, false, master);
                if !(MIN_FRQ_DVBS..=MAX_FRQ_DVBS).contains(&freq) {
                    ms_log::log!("Frequency {} is not within range ", freq);
                    return -403;
                }
                add(
                    &mut props,
                    DtvProperty::new(DTV_SYMBOL_RATE, tp.sr.unwrap_or(0)),
                );
                add(
                    &mut props,
                    DtvProperty::new(DTV_INNER_FEC, tp.fec.unwrap_or(FEC_AUTO)),
                );
                add(
                    &mut props,
                    DtvProperty::new(DTV_PILOT, tp.plts.unwrap_or(PILOT_AUTO)),
                );
                add(
                    &mut props,
                    DtvProperty::new(DTV_ROLLOFF, tp.ro.unwrap_or(ROLLOFF_AUTO)),
                );
                if let Some(plp) = tp.plp_isi {
                    add(&mut props, DtvProperty::new(DTV_STREAM_ID, plp));
                }
                if let Some(pls) = tp.pls_code {
                    add(
                        &mut props,
                        DtvProperty::new(DTV_SCRAMBLING_SEQUENCE_INDEX, pls),
                    );
                }
                ms_log::log!(
                    "tuning to {}({}) pol: {} ({}) sr:{} fec:{} delsys:{} mod:{} rolloff:{} pilot:{}, ts clear={}, ts pol={}",
                    tp.freq.unwrap_or(0),
                    freq,
                    names::fe_pol(tp.pol.unwrap_or(0)),
                    tp.pol.unwrap_or(0),
                    tp.sr.unwrap_or(0),
                    names::fe_fec(tp.fec.unwrap_or(FEC_AUTO)),
                    names::fe_delsys(tp.sys.unwrap_or(SYS_UNDEFINED)),
                    names::fe_modulation(tp.mtype.unwrap_or(QAM_AUTO)),
                    names::fe_rolloff(tp.ro.unwrap_or(ROLLOFF_AUTO)),
                    names::fe_pilot(tp.plts.unwrap_or(PILOT_AUTO)),
                    bclear,
                    bpol
                );
            }
            SYS_DVBT | SYS_DVBT2 => {
                if !(MIN_FRQ_DVBT..=MAX_FRQ_DVBT).contains(&freq) {
                    ms_log::log!("Frequency {} is not within range ", tp.freq.unwrap_or(0));
                    return -403;
                }
                freq *= 1000;
                add(
                    &mut props,
                    DtvProperty::new(DTV_BANDWIDTH_HZ, tp.bw.unwrap_or(8_000_000)),
                );
                add(
                    &mut props,
                    DtvProperty::new(DTV_CODE_RATE_HP, tp.fec.unwrap_or(FEC_AUTO)),
                );
                add(
                    &mut props,
                    DtvProperty::new(DTV_CODE_RATE_LP, tp.fec.unwrap_or(FEC_AUTO)),
                );
                add(
                    &mut props,
                    DtvProperty::new(DTV_GUARD_INTERVAL, tp.gi.unwrap_or(GUARD_INTERVAL_AUTO)),
                );
                add(
                    &mut props,
                    DtvProperty::new(
                        DTV_TRANSMISSION_MODE,
                        tp.tmode.unwrap_or(TRANSMISSION_MODE_AUTO),
                    ),
                );
                add(&mut props, DtvProperty::new(DTV_HIERARCHY, HIERARCHY_AUTO));
                if let Some(plp) = tp.plp_isi {
                    add(&mut props, DtvProperty::new(DTV_STREAM_ID, plp & 0xFF));
                }
                ms_log::log!(
                    "tuning to {} delsys: {} bw:{} inversion:{} mod:{} fec:{} guard:{} transmission: {}, ts clear = {}",
                    freq,
                    names::fe_delsys(tp.sys.unwrap_or(SYS_UNDEFINED)),
                    tp.bw.unwrap_or(8_000_000),
                    names::fe_inversion(tp.inversion.unwrap_or(INVERSION_AUTO)),
                    names::fe_modulation(tp.mtype.unwrap_or(QAM_AUTO)),
                    names::fe_fec(tp.fec.unwrap_or(FEC_AUTO)),
                    names::fe_guard_interval(tp.gi.unwrap_or(GUARD_INTERVAL_AUTO)),
                    names::fe_transmission_mode(tp.tmode.unwrap_or(TRANSMISSION_MODE_AUTO)),
                    bclear
                );
            }
            SYS_DVBC2 | SYS_DVBC_ANNEX_A => {
                if !(MIN_FRQ_DVBC..=MAX_FRQ_DVBC).contains(&freq) {
                    ms_log::log!("Frequency {} is not within range ", tp.freq.unwrap_or(0));
                    return -403;
                }
                freq *= 1000;
                add(
                    &mut props,
                    DtvProperty::new(DTV_SYMBOL_RATE, tp.sr.unwrap_or(0)),
                );
                if let Some(plp) = tp.plp_isi {
                    let stream_id = (plp & 0xFF) | ((tp.ds.unwrap_or(0) & 0xFF) << 8);
                    add(&mut props, DtvProperty::new(DTV_STREAM_ID, stream_id));
                }
                ms_log::log!(
                    "tuning to {} sr:{} specinv:{} delsys:{} mod:{} ts clear = {}",
                    freq,
                    tp.sr.unwrap_or(0),
                    names::fe_inversion(tp.inversion.unwrap_or(INVERSION_AUTO)),
                    names::fe_delsys(tp.sys.unwrap_or(SYS_UNDEFINED)),
                    names::fe_modulation(tp.mtype.unwrap_or(QAM_AUTO)),
                    bclear
                );
            }
            SYS_ATSC | SYS_DVBC_ANNEX_B => {
                if !(MIN_FRQ_DVBC..=MAX_FRQ_DVBC).contains(&freq) {
                    ms_log::log!("Frequency {} is not within range ", tp.freq.unwrap_or(0));
                    return -403;
                }
                freq *= 1000;
                ms_log::log!(
                    "tuning to {} delsys:{} mod:{} specinv:{} ts clear = {}",
                    freq,
                    names::fe_delsys(tp.sys.unwrap_or(SYS_UNDEFINED)),
                    names::fe_modulation(tp.mtype.unwrap_or(QAM_AUTO)),
                    names::fe_inversion(tp.inversion.unwrap_or(INVERSION_AUTO)),
                    bclear
                );
            }
            SYS_ISDBT => {
                if !(MIN_FRQ_DVBT..=MAX_FRQ_DVBT).contains(&freq) {
                    ms_log::log!("Frequency {} is not within range ", tp.freq.unwrap_or(0));
                    return -403;
                }
                freq *= 1000;
                add(
                    &mut props,
                    DtvProperty::new(DTV_BANDWIDTH_HZ, tp.bw.unwrap_or(8_000_000)),
                );
                add(&mut props, DtvProperty::new(DTV_ISDBT_PARTIAL_RECEPTION, 0));
                ms_log::log!(
                    "tuning to {} delsys: {} bw:{} inversion:{} , ts clear = {}",
                    freq,
                    names::fe_delsys(tp.sys.unwrap_or(SYS_UNDEFINED)),
                    tp.bw.unwrap_or(8_000_000),
                    names::fe_inversion(tp.inversion.unwrap_or(INVERSION_AUTO)),
                    bclear
                );
            }
            _ => {
                ms_log::log!(
                    "tuning to unknown delsys: {} freq {} ts clear = {}",
                    names::fe_delsys(tp.sys.unwrap_or(SYS_UNDEFINED)),
                    freq,
                    bclear
                );
            }
        }

        add(&mut props, DtvProperty::new(DTV_FREQUENCY, freq));
        add(
            &mut props,
            DtvProperty::new(DTV_INVERSION, tp.inversion.unwrap_or(INVERSION_AUTO)),
        );
        add(
            &mut props,
            DtvProperty::new(DTV_MODULATION, tp.mtype.unwrap_or(QAM_AUTO)),
        );
        add(
            &mut props,
            DtvProperty::new(DTV_DELIVERY_SYSTEM, tp.sys.unwrap_or(SYS_UNDEFINED)),
        );
        add(&mut props, DtvProperty::new(DTV_TUNE, 0));

        // Discard stale QPSK events.
        while dev.get_event().is_ok() {}

        // Two attempts, like dvb.cpp:1525-1529.
        if dev.set_properties(&props).is_err()
            && let Err(error) = dev.set_properties(&props)
        {
            let (errno, text) = error_parts(&error);
            ms_log::log!("dvb_tune: set property failed {} {}", errno, text);
            return -500;
        }

        let sys = tp.sys.unwrap_or(SYS_UNDEFINED);
        if sys == SYS_DVBS || sys == SYS_DVBS2 {
            self.lock_retry_time = dev.now_ms();
            self.lock_retry_armed = true;
        }
        self.db_snr_map = db_snr_map(&self.tp);
        self.tune_time = dev.now_ms();
        0
    }

    /// `dvb_get_signal` (dvb.cpp:2306) for an independent adapter.
    pub fn get_signal<D: DvbDevice + ?Sized>(&mut self, dev: &mut D) -> i32 {
        self.get_signal_with_master(dev, None)
    }

    /// `dvb_get_signal`; the master view is only used by a failed-lock resend.
    pub fn get_signal_with_master<D: DvbDevice + ?Sized>(
        &mut self,
        dev: &mut D,
        master: Option<MasterView<'_>>,
    ) -> i32 {
        let mut raw = RawSignal::default();
        let mut status_valid;
        let mut status = 0i32;
        if self.strength_multiplier != 0.0 || self.snr_multiplier != 0.0 {
            let frontend_open = self.fe > 0;
            let v5 = if self.new_gs != OLD_SIGNAL {
                signal_v5(dev, self.id, frontend_open, self.db_snr_map)
            } else {
                None
            };
            if let Some(v5) = v5 {
                self.new_gs = NEW_SIGNAL;
                raw = v5;
            } else {
                self.new_gs = OLD_SIGNAL;
                raw = signal_v3(
                    dev,
                    self.id,
                    self.pa,
                    frontend_open,
                    &mut self.snr_calib,
                    &mut self.strength_warned,
                    self.force_tuner_signal,
                );
                if self.force_tuner_signal != TUNER_FORCE_NO {
                    adapt_signal(self.id, self.force_tuner_signal, self.db_snr_map, &mut raw);
                }
            }
            status = raw.status;
            status_valid = raw.status_valid;
        } else {
            status_valid = false;
            if self.lock_retry_armed
                && let Some(read) = read_status(dev, self.id, self.fe > 0)
            {
                status = read;
                status_valid = true;
            }
            if !status_valid {
                status = 1;
            }
            ms_log::logm!("Signal metrics are not retrieved as both multipliers are 0");
        }

        let mut strength = raw.strength;
        if self.strength_multiplier != 0.0 {
            strength = apply_multiplier(strength, self.strength_multiplier);
        } else {
            strength = 65535;
        }
        let mut snr = raw.snr;
        if self.snr_multiplier != 0.0 {
            snr = apply_multiplier(snr, self.snr_multiplier);
        } else {
            snr = 65535;
        }
        strength >>= 8;
        snr >>= 8;
        self.strength = strength.min(255);
        self.snr = snr.min(255);
        self.db = raw.db;
        if status_valid {
            self.status = status;
        }
        self.ber = raw.ber;

        self.retry_lock(dev, master, status_valid);
        0
    }

    /// `dvb_lock_retry_due` (dvb.cpp:2263).
    pub fn lock_retry_due(&self, now: i64) -> bool {
        let sys = self.tp.sys.unwrap_or(SYS_UNDEFINED);
        if !self.lock_retry_armed
            || self.fe <= 0
            || self.sid_cnt <= 0
            || self.status & FE_HAS_LOCK != 0
        {
            return false;
        }
        if sys != SYS_DVBS && sys != SYS_DVBS2 {
            return false;
        }
        let switch_type = self.tp.diseqc_param.switch_type;
        let single_cable = switch_type == SWITCH_JESS || switch_type == SWITCH_UNICABLE;
        if (!single_cable && self.master_source >= 0) || switch_type == SWITCH_SLAVE {
            return false;
        }
        if self.lock_retries >= LOCK_RETRY_MAX {
            return false;
        }
        let delay = if self.lock_retries == 0 {
            LOCK_RETRY_INITIAL_MS
        } else {
            LOCK_RETRY_INTERVAL_MS
        };
        now - self.lock_retry_time >= delay
    }

    /// `dvb_lock_retry_update` (dvb.cpp:2283): true when the switch must be resent.
    pub fn lock_retry_update(&mut self, now: i64, status_valid: bool) -> bool {
        if !status_valid {
            return false;
        }
        if self.status & FE_HAS_LOCK != 0 {
            self.lock_retries = 0;
            self.lock_retry_time = now;
            return false;
        }
        if !self.lock_retry_due(now) {
            return false;
        }
        self.lock_retries += 1;
        self.lock_retry_time = now;
        true
    }

    /// `dvb_retry_lock` (dvb.cpp:2298).
    pub fn retry_lock<D: DvbDevice + ?Sized>(
        &mut self,
        dev: &mut D,
        master: Option<MasterView<'_>>,
        status_valid: bool,
    ) {
        let now = dev.now_ms();
        if !self.lock_retry_update(now, status_valid) {
            return;
        }
        ms_log::log!(
            "ad {} has no lock, resending the switch commands (retry {})",
            self.id,
            self.lock_retries
        );
        self.setup_switch(dev, true, master);
    }
}
