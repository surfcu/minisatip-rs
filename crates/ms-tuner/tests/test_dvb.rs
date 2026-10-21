//! Ports of `tests/test_dvb.cpp` plus backend tests against a recording fake device.
#![cfg(feature = "linux-dvb")]

use std::io;

use ms_tuner::linux_dvb::*;
use ms_tuner::{
    DeliverySystem, PID_ALL, PidSet, Polarization, TsBuf, TuneParams, Tuner, TunerError,
};

#[derive(Debug, Clone, PartialEq, Eq)]
enum Call {
    SetProperties(Vec<DtvProperty>),
    GetStats,
    EnumDelsys,
    GetInfo,
    GetEvent,
    ReadStatus,
    ReadBer,
    ReadSignalStrength,
    ReadSnr,
    SetTone(Tone),
    SetVoltage(Voltage),
    SendMasterCmd(DiseqcMessage),
    SendBurst(MiniCmd),
    Sleep(i32),
    Wakeup(Voltage),
    OpenDemux,
    CloseDemux(DemuxFd),
    SetPesFilter(DemuxFd, u16, PesOutput),
    AddPid(DemuxFd, u16),
    RemovePid(DemuxFd, u16),
    DemuxStop(DemuxFd),
    SetBufferSize(DemuxFd, i32),
    SetDemuxSource(i32),
}

struct FakeDevice {
    calls: Vec<Call>,
    now: i64,
    set_properties_fail: bool,
    events: usize,
    stats: Option<[DtvStat; 3]>,
    info: String,
    status: Option<i32>,
    ber: Option<u32>,
    strength: Option<u16>,
    snr: Option<u16>,
    stream: Vec<u8>,
    stream_pos: usize,
    next_fd: u32,
}

impl FakeDevice {
    fn new() -> Self {
        Self {
            calls: Vec::new(),
            now: 0,
            set_properties_fail: false,
            events: 0,
            stats: None,
            info: "Mock FE".to_string(),
            status: None,
            ber: None,
            strength: None,
            snr: None,
            stream: Vec::new(),
            stream_pos: 0,
            next_fd: 100,
        }
    }

    fn stats(len: u8, strength_scale: u8, snr_scale: u8, strength: u64, snr: u64) -> [DtvStat; 3] {
        [
            DtvStat {
                len,
                scale: strength_scale,
                value: strength,
            },
            DtvStat {
                len,
                scale: snr_scale,
                value: snr,
            },
            DtvStat {
                len,
                scale: FE_SCALE_COUNTER,
                value: 0,
            },
        ]
    }

    fn v3(&mut self, status: i32, ber: u32, strength: u16, snr: u16) {
        self.status = Some(status);
        self.ber = Some(ber);
        self.strength = Some(strength);
        self.snr = Some(snr);
    }

    fn count(&self, f: impl Fn(&Call) -> bool) -> usize {
        self.calls.iter().filter(|call| f(call)).count()
    }

    fn last_properties(&self) -> &[DtvProperty] {
        self.calls
            .iter()
            .rev()
            .find_map(|call| match call {
                Call::SetProperties(props) => Some(props.as_slice()),
                _ => None,
            })
            .expect("a tune sets properties")
    }

    fn master_cmds(&self) -> Vec<DiseqcMessage> {
        self.calls
            .iter()
            .filter_map(|call| match call {
                Call::SendMasterCmd(cmd) => Some(*cmd),
                _ => None,
            })
            .collect()
    }
}

impl Default for FakeDevice {
    fn default() -> Self {
        Self::new()
    }
}

impl SecDevice for FakeDevice {
    fn set_tone(&mut self, tone: Tone) -> io::Result<()> {
        self.calls.push(Call::SetTone(tone));
        Ok(())
    }

    fn set_voltage(&mut self, voltage: Voltage) -> io::Result<()> {
        self.calls.push(Call::SetVoltage(voltage));
        Ok(())
    }

    fn send_master_cmd(&mut self, cmd: &DiseqcMessage) -> io::Result<()> {
        self.calls.push(Call::SendMasterCmd(*cmd));
        Ok(())
    }

    fn send_burst(&mut self, burst: MiniCmd) -> io::Result<()> {
        self.calls.push(Call::SendBurst(burst));
        Ok(())
    }

    fn sleep_ms(&mut self, ms: i32) {
        self.calls.push(Call::Sleep(ms));
    }

    fn wakeup(&mut self, voltage: Voltage) {
        self.calls.push(Call::Wakeup(voltage));
    }
}

impl DvbDevice for FakeDevice {
    fn frontend_open(&self) -> bool {
        true
    }

    fn set_properties(&mut self, props: &[DtvProperty]) -> io::Result<()> {
        self.calls.push(Call::SetProperties(props.to_vec()));
        if self.set_properties_fail {
            Err(io::Error::from_raw_os_error(22))
        } else {
            Ok(())
        }
    }

    fn get_stats(&mut self) -> io::Result<[DtvStat; 3]> {
        self.calls.push(Call::GetStats);
        self.stats.ok_or_else(|| io::Error::from_raw_os_error(22))
    }

    fn enum_delsys(&mut self) -> io::Result<Vec<u32>> {
        self.calls.push(Call::EnumDelsys);
        Ok(vec![SYS_DVBS2 as u32])
    }

    fn get_info(&mut self) -> io::Result<FrontendInfo> {
        self.calls.push(Call::GetInfo);
        Ok(FrontendInfo {
            name: self.info.clone(),
            fe_type: 0,
            caps: 0,
        })
    }

    fn get_event(&mut self) -> io::Result<()> {
        self.calls.push(Call::GetEvent);
        if self.events > 0 {
            self.events -= 1;
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(11))
        }
    }

    fn read_status(&mut self) -> io::Result<i32> {
        self.calls.push(Call::ReadStatus);
        self.status.ok_or_else(|| io::Error::from_raw_os_error(22))
    }

    fn read_ber(&mut self) -> io::Result<u32> {
        self.calls.push(Call::ReadBer);
        self.ber.ok_or_else(|| io::Error::from_raw_os_error(22))
    }

    fn read_signal_strength(&mut self) -> io::Result<u16> {
        self.calls.push(Call::ReadSignalStrength);
        self.strength
            .ok_or_else(|| io::Error::from_raw_os_error(22))
    }

    fn read_snr(&mut self) -> io::Result<u16> {
        self.calls.push(Call::ReadSnr);
        self.snr.ok_or_else(|| io::Error::from_raw_os_error(22))
    }

    fn stream_fd(&self) -> DemuxFd {
        DemuxFd(42)
    }

    fn open_demux(&mut self) -> io::Result<DemuxFd> {
        self.calls.push(Call::OpenDemux);
        let fd = DemuxFd(self.next_fd);
        self.next_fd += 1;
        Ok(fd)
    }

    fn close_demux(&mut self, fd: DemuxFd) {
        self.calls.push(Call::CloseDemux(fd));
    }

    fn set_pes_filter(&mut self, fd: DemuxFd, pid: u16, output: PesOutput) -> io::Result<()> {
        self.calls.push(Call::SetPesFilter(fd, pid, output));
        Ok(())
    }

    fn add_pid(&mut self, fd: DemuxFd, pid: u16) -> io::Result<()> {
        self.calls.push(Call::AddPid(fd, pid));
        Ok(())
    }

    fn remove_pid(&mut self, fd: DemuxFd, pid: u16) -> io::Result<()> {
        self.calls.push(Call::RemovePid(fd, pid));
        Ok(())
    }

    fn demux_stop(&mut self, fd: DemuxFd) -> io::Result<()> {
        self.calls.push(Call::DemuxStop(fd));
        Ok(())
    }

    fn set_buffer_size(&mut self, fd: DemuxFd, size: i32) -> io::Result<()> {
        self.calls.push(Call::SetBufferSize(fd, size));
        Ok(())
    }

    fn set_demux_source(&mut self, source: i32) -> io::Result<()> {
        self.calls.push(Call::SetDemuxSource(source));
        Ok(())
    }

    fn read_stream(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let available = self.stream.len() - self.stream_pos;
        let count = available.min(buf.len());
        buf[..count].copy_from_slice(&self.stream[self.stream_pos..self.stream_pos + count]);
        self.stream_pos += count;
        Ok(count)
    }

    fn now_ms(&self) -> i64 {
        self.now
    }
}

fn default_state() -> AdapterState {
    AdapterState::default()
}

fn sat_transponder(freq_khz: i32) -> DvbTransponder {
    DvbTransponder {
        sys: Some(SYS_DVBS2),
        freq: Some(freq_khz),
        pol: Some(2),
        diseqc: Some(1),
        sr: Some(22_000_000),
        ..DvbTransponder::default()
    }
}

#[test]
fn lof_conversion_matches_cpp() {
    let mut tp = sat_transponder(11_500_000);
    tp.pol = Some(1);
    assert_eq!(lnb_hiband(&tp, &tp.diseqc_param), 0);
    assert_eq!(lnb_int_freq(&tp, &tp.diseqc_param), 1_750_000);

    tp.freq = Some(12_000_000);
    assert_eq!(lnb_hiband(&tp, &tp.diseqc_param), 1);
    assert_eq!(lnb_int_freq(&tp, &tp.diseqc_param), 1_400_000);

    // Circular polarity uses the circular LOF regardless of the band switch.
    tp.freq = Some(11_000_000);
    tp.pol = Some(3);
    assert_eq!(lnb_hiband(&tp, &tp.diseqc_param), 0);
    assert_eq!(lnb_int_freq(&tp, &tp.diseqc_param), 250_000);

    // A zero switch/circular LOF disables that branch (abs() on low band).
    tp.diseqc_param.lnb_switch = 0;
    tp.diseqc_param.lnb_circular = 0;
    tp.freq = Some(12_000_000);
    assert_eq!(lnb_hiband(&tp, &tp.diseqc_param), 0);
    assert_eq!(lnb_int_freq(&tp, &tp.diseqc_param), 2_250_000);
}

// Ported from tests/test_dvb.cpp:151
#[test]
fn test_dvb_lock_recovery_eligibility() {
    let mut state = default_state();
    state.fe = 5;
    state.sid_cnt = 1;
    state.status = 0;
    state.master_source = -1;
    state.tp.sys = Some(SYS_DVBS2);

    assert!(
        !state.lock_retry_due(6000),
        "an unsuccessful tune never arms retries"
    );
    state.lock_retry_armed = true;
    assert!(
        !state.lock_retry_due(5999),
        "first retry waits through normal cold acquisition"
    );
    assert!(
        state.lock_retry_due(6000),
        "first retry is due at the exact 6s boundary"
    );

    state.status = FE_HAS_LOCK;
    assert!(!state.lock_retry_due(6000), "no retry once locked");
    state.status = FE_HAS_SIGNAL | FE_HAS_CARRIER;
    assert!(
        state.lock_retry_due(6000),
        "a carrier without lock still retries"
    );
    state.status = FE_TIMEDOUT;
    assert!(
        state.lock_retry_due(6000),
        "a frontend timeout without lock still retries"
    );
    state.status = 0;

    state.sid_cnt = 0;
    assert!(!state.lock_retry_due(6000), "no retry with no viewers");
    state.sid_cnt = 1;

    state.lock_retries = 5;
    assert!(!state.lock_retry_due(6000), "retries give up after the cap");
    state.lock_retries = 0;

    state.master_source = 0;
    assert!(
        !state.lock_retry_due(6000),
        "conventional slaves never resend the switch"
    );
    state.tp.diseqc_param.switch_type = SWITCH_JESS;
    assert!(
        state.lock_retry_due(6000),
        "JESS slaves retry their slot-specific command"
    );
    state.tp.diseqc_param.switch_type = SWITCH_UNICABLE;
    assert!(
        state.lock_retry_due(6000),
        "Unicable slaves retry their slot-specific command"
    );
    state.tp.diseqc_param.switch_type = 0;
    state.master_source = -1;

    state.tp.diseqc_param.switch_type = SWITCH_SLAVE;
    assert!(
        !state.lock_retry_due(6000),
        "slave switch type never resends"
    );
    state.tp.diseqc_param.switch_type = 0;

    state.tp.diseqc_param.fast = 1;
    assert!(
        state.lock_retry_due(6000),
        "fast diseqc mode still permits forced recovery"
    );
    state.tp.diseqc_param.fast = 0;

    state.tp.sys = Some(SYS_DVBT);
    assert!(!state.lock_retry_due(6000), "no resend for non-SAT systems");
    state.tp.sys = None;
    assert!(
        !state.lock_retry_due(6000),
        "no resend when the delivery system is omitted"
    );
    state.tp.sys = Some(SYS_DVBS);
    assert!(state.lock_retry_due(6000), "DVB-S retries like DVB-S2");

    state.fe = 0;
    assert!(!state.lock_retry_due(6000), "frontend zero cannot retry");
    state.fe = -1;
    assert!(
        !state.lock_retry_due(6000),
        "negative frontend cannot retry"
    );
}

// Ported from tests/test_dvb.cpp:221
#[test]
fn test_dvb_lock_recovery_state_transitions() {
    let mut state = default_state();
    state.fe = 5;
    state.sid_cnt = 1;
    state.master_source = -1;
    state.tp.sys = Some(SYS_DVBS2);
    state.lock_retry_armed = true;
    state.lock_retries = 3;
    state.lock_retry_time = 1000;
    state.status = FE_HAS_LOCK;

    assert!(
        !state.lock_retry_update(10000, true),
        "a lock never requests a resend"
    );
    assert!(
        state.lock_retries == 0 && state.lock_retry_time == 10000,
        "a lock resets retries and starts a new loss window"
    );

    state.status = FE_HAS_SIGNAL | FE_HAS_CARRIER;
    assert!(
        !state.lock_retry_update(15999, true),
        "a lock loss gets the full initial delay"
    );
    assert!(
        state.lock_retry_update(16000, true),
        "a continuous 6s lock loss requests a resend"
    );
    assert!(
        state.lock_retries == 1 && state.lock_retry_time == 16000,
        "a resend advances the retry state"
    );
    assert!(
        !state.lock_retry_update(17999, true),
        "subsequent retries remain spaced by 2s"
    );
    assert!(
        state.lock_retry_update(18000, true),
        "the next retry is due at the 2s boundary"
    );

    state.lock_retries = 4;
    assert!(
        !state.lock_retry_update(19999, true),
        "the fifth retry waits for the full interval"
    );
    assert!(
        state.lock_retry_update(20000, true),
        "the fifth retry is permitted at the boundary"
    );
    assert!(
        state.lock_retries == 5 && state.lock_retry_time == 20000,
        "the fifth retry reaches the cap"
    );
    assert!(
        !state.lock_retry_update(22000, true),
        "a sixth retry is rejected and the cap persists"
    );

    assert!(
        !state.lock_retry_update(23000, false),
        "an unknown status never requests a resend"
    );
    assert!(
        state.lock_retries == 5 && state.lock_retry_time == 20000,
        "an unknown status preserves the retry cap and timing"
    );
}

// Ported from tests/test_dvb.cpp:266
#[test]
fn test_dvb_lock_recovery_rejects_failed_tunes() {
    let mut dev = FakeDevice::new();
    let mut state = default_state();
    state.fe = 5;
    state.master_source = -1;
    state.old_pol = 1;
    state.old_hiband = 1;
    state.old_diseqc = 0;

    let mut tp = DvbTransponder {
        sys: Some(SYS_DVBS2),
        freq: Some(15_000_000),
        pol: Some(2),
        diseqc: Some(1),
        ..DvbTransponder::default()
    };
    tp.diseqc_param.lnb_low = 9_750_000;
    tp.diseqc_param.lnb_high = 10_600_000;
    tp.diseqc_param.lnb_switch = 11_700_000;
    state.tp = tp;

    state.lock_retry_armed = true;
    let current = state.tp.clone();
    let invalid_frequency_result = state.tune(&mut dev, &current);
    let armed_after_invalid_frequency = state.lock_retry_armed;

    state.tp.freq = Some(11_000_000);
    state.old_hiband = 0;
    state.lock_retry_armed = true;
    dev.set_properties_fail = true;
    let current = state.tp.clone();
    let failed_ioctl_result = state.tune(&mut dev, &current);
    let armed_after_failed_ioctl = state.lock_retry_armed;

    state.fe = -1;
    state.lock_retry_armed = true;
    let current = state.tp.clone();
    let missing_frontend_result = state.tune(&mut dev, &current);
    let armed_without_frontend = state.lock_retry_armed;

    assert_eq!(
        invalid_frequency_result, -403,
        "out-of-range SAT frequency must fail tuning"
    );
    assert!(
        !armed_after_invalid_frequency,
        "out-of-range SAT frequency must not arm retries"
    );
    assert_eq!(
        failed_ioctl_result, -500,
        "failed frontend ioctl must fail tuning"
    );
    assert!(
        !armed_after_failed_ioctl,
        "failed frontend ioctl must not arm retries"
    );
    assert_eq!(
        missing_frontend_result, -500,
        "missing frontend must fail tuning"
    );
    assert!(
        !armed_without_frontend,
        "missing frontend must clear stale retry state"
    );
}

// Ported from tests/test_dvb.cpp:328
#[test]
fn test_dvb_lock_recovery_without_signal_metrics() {
    let mut dev = FakeDevice::new();
    let mut state = default_state();
    state.lock_retry_armed = true;
    state.lock_retries = 5;
    state.lock_retry_time = 1;
    state.status = FE_HAS_LOCK;

    assert_eq!(state.get_signal(&mut dev), 0);
    assert_eq!(
        state.status, FE_HAS_LOCK,
        "an invalid status read preserves the last frontend status"
    );
    assert!(
        state.lock_retries == 5 && state.lock_retry_time == 1,
        "disabled metrics preserve exhausted retry state"
    );
}

// Ported from tests/test_dvb.cpp:344
#[test]
fn test_dvb_lock_recovery_forced_switch_resend() {
    let mut dev = FakeDevice::new();
    let mut state = default_state();
    state.fe = -1;
    state.master_source = -1;
    state.old_pol = 1;
    state.old_hiband = 0;
    state.old_diseqc = 0;
    state.tp = sat_transponder(11_000_000);
    state.tp.diseqc_param.lnb_low = 9_750_000;
    state.tp.diseqc_param.lnb_high = 10_600_000;
    state.tp.diseqc_param.lnb_switch = 11_700_000;

    let _ = state.setup_switch(&mut dev, false, None);
    let no_op_calls = dev.count(|call| matches!(call, Call::Wakeup(_)));
    let _ = state.setup_switch(&mut dev, true, None);

    assert_eq!(
        no_op_calls, 0,
        "an exact switch-state match remains a no-op"
    );
    assert_eq!(
        dev.count(|call| matches!(call, Call::Wakeup(_))),
        1,
        "lock recovery forces one switch resend"
    );
    assert!(
        state.old_pol == 1 && state.old_hiband == 0 && state.old_diseqc == 0,
        "forcing a resend keeps the cached switch state valid"
    );
}

// Ported from tests/test_dvb.cpp:459
#[test]
fn test_dvb_signal_v5_fallback() {
    let mut dev = FakeDevice::new();

    let mut state = default_state();
    state.fe = 42;
    state.strength_multiplier = 1.0;
    state.snr_multiplier = 1.0;

    dev.stats = Some(FakeDevice::stats(
        1,
        FE_SCALE_NOT_AVAILABLE,
        FE_SCALE_NOT_AVAILABLE,
        0,
        0,
    ));
    dev.status = Some(0);
    state.get_signal(&mut dev);
    assert_eq!(
        state.new_gs, NEW_SIGNAL,
        "entries with N/A scales must use the v5 path"
    );
    assert!(
        state.strength == 0 && state.snr == 0,
        "N/A scales read as zero"
    );

    dev.stats = Some(FakeDevice::stats(
        1,
        FE_SCALE_RELATIVE,
        FE_SCALE_RELATIVE,
        0x8000,
        0x4000,
    ));
    dev.status = Some(FE_HAS_LOCK);
    state.get_signal(&mut dev);
    assert_eq!(
        state.new_gs, NEW_SIGNAL,
        "v5 must serve as soon as stats are usable"
    );
    assert!(
        state.strength == 0x80 && state.snr == 0x40,
        "v5 stats must reach the adapter"
    );

    dev.stats = Some(FakeDevice::stats(
        1,
        FE_SCALE_NOT_AVAILABLE,
        FE_SCALE_RELATIVE,
        0,
        0x4000,
    ));
    state.get_signal(&mut dev);
    assert_eq!(state.new_gs, NEW_SIGNAL, "one good stat keeps the v5 path");
    assert!(
        state.strength == 0 && state.snr == 0x40,
        "per-stat scales must not fall back to v3"
    );

    dev.stats = Some(FakeDevice::stats(0, 0, 0, 0, 0));
    dev.v3(FE_HAS_LOCK, 0, 0x4000, 0x2000);
    state.get_signal(&mut dev);
    assert_eq!(
        state.new_gs, OLD_SIGNAL,
        "missing entries must fall back to v3"
    );
    assert!(
        state.strength == 0x40 && state.snr == 0x20,
        "v3 values must reach the adapter"
    );

    dev.stats = Some(FakeDevice::stats(
        1,
        FE_SCALE_RELATIVE,
        FE_SCALE_RELATIVE,
        0x8000,
        0x4000,
    ));
    state.get_signal(&mut dev);
    assert_eq!(
        state.new_gs, OLD_SIGNAL,
        "v3 must stick until the next tune"
    );
    assert!(
        state.strength == 0x40 && state.snr == 0x20,
        "stuck v3 must not see the healed v5 stats"
    );

    let tp = state.tp.clone();
    state.tune(&mut dev, &tp);
    assert_eq!(
        state.new_gs, SIGNAL_UNPROBED,
        "a retune must re-probe the signal path"
    );
    state.get_signal(&mut dev);
    assert_eq!(
        state.new_gs, NEW_SIGNAL,
        "v5 must serve again after the retune"
    );
    assert!(
        state.strength == 0x80 && state.snr == 0x40,
        "v5 stats must reach the adapter"
    );

    let mut other = default_state();
    other.fe = 42;
    other.strength_multiplier = 1.0;
    other.snr_multiplier = 1.0;
    dev.stats = None;
    other.get_signal(&mut dev);
    assert_eq!(
        other.new_gs, OLD_SIGNAL,
        "a failed properties query must fall back to v3"
    );
    assert!(
        other.strength == 0x40 && other.snr == 0x20,
        "v3 values must flow despite failing v5"
    );

    let mut forced = default_state();
    forced.fe = 42;
    forced.strength_multiplier = 1.0;
    forced.snr_multiplier = 1.0;
    forced.force_tuner_signal = TUNER_FORCE_STRENGTH_PERCENT;
    dev.stats = Some(FakeDevice::stats(0, 0, 0, 0, 0));
    forced.get_signal(&mut dev);
    assert_eq!(
        forced.new_gs, OLD_SIGNAL,
        "forced display must use v3 while v5 idles"
    );
    state.force_tuner_signal = TUNER_FORCE_STRENGTH_PERCENT;
    dev.stats = Some(FakeDevice::stats(
        1,
        FE_SCALE_RELATIVE,
        FE_SCALE_RELATIVE,
        0x8000,
        0x4000,
    ));
    state.get_signal(&mut dev);
    assert_eq!(
        state.new_gs, NEW_SIGNAL,
        "forced display must use v5 when usable"
    );
    assert!(
        state.strength == 0x80 && state.snr == 0x40,
        "forcing must not reshape v5 values"
    );
}

fn prop(cmd: u32, data: i32) -> DtvProperty {
    DtvProperty::new(cmd, data)
}

fn state_with_switch(pol: i32, hiband: i32, diseqc: i32) -> AdapterState {
    AdapterState {
        fe: 1,
        old_pol: pol,
        old_hiband: hiband,
        old_diseqc: diseqc,
        strength_multiplier: 1.0,
        snr_multiplier: 1.0,
        ..AdapterState::default()
    }
}

#[test]
fn dvb_tune_dvbs2_property_sequence() {
    let mut dev = FakeDevice::new();
    let mut state = state_with_switch(1, 0, 0);
    let params = TuneParams {
        freq: 11_500,
        pol: Polarization::Horizontal,
        msys: DeliverySystem::DvbS2,
        sr: 22_000,
        fec: 2,
        fec_specified: true,
        mtype: 0,
        mtype_specified: true,
        plts: Some(false),
        ro: Some(0),
        plp: Some(5),
        plsc: Some(42),
        ..TuneParams::default()
    };
    let tp = DvbTransponder::from_params(&params, state.tp.diseqc_param);
    assert_eq!(state.tune(&mut dev, &tp), 0);

    assert_eq!(
        dev.calls.first(),
        Some(&Call::SetProperties(vec![prop(DTV_CLEAR, 0)]))
    );
    assert_eq!(
        dev.last_properties(),
        &[
            prop(DTV_SYMBOL_RATE, 22_000_000),
            prop(DTV_INNER_FEC, 2),
            prop(DTV_PILOT, 1),
            prop(DTV_ROLLOFF, 0),
            prop(DTV_STREAM_ID, 5),
            prop(DTV_SCRAMBLING_SEQUENCE_INDEX, 42),
            prop(DTV_FREQUENCY, 1_750_000),
            prop(DTV_INVERSION, 2),
            prop(DTV_MODULATION, 0),
            prop(DTV_DELIVERY_SYSTEM, SYS_DVBS2),
            prop(DTV_TUNE, 0),
        ]
    );
}

#[test]
fn dvb_tune_dvbt2_property_sequence() {
    let mut dev = FakeDevice::new();
    let mut state = state_with_switch(1, 0, 0);
    let params = TuneParams {
        freq: 474,
        msys: DeliverySystem::DvbT2,
        bw: Some(8_000_000),
        fec: 3,
        fec_specified: true,
        gi: Some(2),
        tmode: Some(1),
        mtype: 3,
        mtype_specified: true,
        plp: Some(300),
        ..TuneParams::default()
    };
    let tp = DvbTransponder::from_params(&params, state.tp.diseqc_param);
    assert_eq!(state.tune(&mut dev, &tp), 0);
    assert_eq!(
        dev.last_properties(),
        &[
            prop(DTV_BANDWIDTH_HZ, 8_000_000),
            prop(DTV_CODE_RATE_HP, 3),
            prop(DTV_CODE_RATE_LP, 3),
            prop(DTV_GUARD_INTERVAL, 2),
            prop(DTV_TRANSMISSION_MODE, 1),
            prop(DTV_HIERARCHY, 4),
            prop(DTV_STREAM_ID, 44),
            prop(DTV_FREQUENCY, 474_000_000),
            prop(DTV_INVERSION, 2),
            prop(DTV_MODULATION, 3),
            prop(DTV_DELIVERY_SYSTEM, SYS_DVBT2),
            prop(DTV_TUNE, 0),
        ]
    );
}

#[test]
fn dvb_tune_dvbc_property_sequence() {
    let mut dev = FakeDevice::new();
    let mut state = state_with_switch(1, 0, 0);
    let params = TuneParams {
        freq: 474,
        msys: DeliverySystem::DvbC,
        sr: 6_900,
        plp: Some(1),
        ds: Some(2),
        mtype: 5,
        mtype_specified: true,
        ..TuneParams::default()
    };
    let tp = DvbTransponder::from_params(&params, state.tp.diseqc_param);
    assert_eq!(state.tune(&mut dev, &tp), 0);
    assert_eq!(
        dev.last_properties(),
        &[
            prop(DTV_SYMBOL_RATE, 6_900_000),
            prop(DTV_STREAM_ID, 513),
            prop(DTV_FREQUENCY, 474_000_000),
            prop(DTV_INVERSION, 2),
            prop(DTV_MODULATION, 5),
            prop(DTV_DELIVERY_SYSTEM, SYS_DVBC_ANNEX_A),
            prop(DTV_TUNE, 0),
        ]
    );
}

#[test]
fn dvb_tune_atsc_and_isdbt_property_sequences() {
    let mut dev = FakeDevice::new();
    let mut state = state_with_switch(1, 0, 0);
    let atsc = TuneParams {
        freq: 474,
        msys: DeliverySystem::Other(SYS_ATSC),
        ..TuneParams::default()
    };
    let tp = DvbTransponder::from_params(&atsc, state.tp.diseqc_param);
    assert_eq!(state.tune(&mut dev, &tp), 0);
    assert_eq!(
        dev.last_properties(),
        &[
            prop(DTV_FREQUENCY, 474_000_000),
            prop(DTV_INVERSION, 2),
            prop(DTV_MODULATION, 6),
            prop(DTV_DELIVERY_SYSTEM, SYS_ATSC),
            prop(DTV_TUNE, 0),
        ]
    );

    let isdbt = TuneParams {
        freq: 474,
        msys: DeliverySystem::Other(SYS_ISDBT),
        bw: Some(6_000_000),
        ..TuneParams::default()
    };
    let tp = DvbTransponder::from_params(&isdbt, state.tp.diseqc_param);
    assert_eq!(state.tune(&mut dev, &tp), 0);
    assert_eq!(
        dev.last_properties(),
        &[
            prop(DTV_BANDWIDTH_HZ, 6_000_000),
            prop(DTV_ISDBT_PARTIAL_RECEPTION, 0),
            prop(DTV_FREQUENCY, 474_000_000),
            prop(DTV_INVERSION, 2),
            prop(DTV_MODULATION, 6),
            prop(DTV_DELIVERY_SYSTEM, SYS_ISDBT),
            prop(DTV_TUNE, 0),
        ]
    );
}

#[test]
fn dvb_tune_rejects_out_of_range_frequencies() {
    let cases = [
        (DeliverySystem::DvbS2, 1_u32, "SAT intermediate frequency"),
        (DeliverySystem::DvbT2, 1_000, "DVB-T frequency"),
        (DeliverySystem::DvbC, 1_000, "DVB-C frequency"),
        (DeliverySystem::Other(SYS_ATSC), 1_000, "ATSC frequency"),
        (DeliverySystem::Other(SYS_ISDBT), 1_000, "ISDB-T frequency"),
    ];
    for (msys, freq, name) in cases {
        let mut dev = FakeDevice::new();
        let mut state = state_with_switch(1, 0, 0);
        let params = TuneParams {
            freq,
            msys,
            ..TuneParams::default()
        };
        let tp = DvbTransponder::from_params(&params, state.tp.diseqc_param);
        assert_eq!(state.tune(&mut dev, &tp), -403, "{name} must be rejected");
        assert!(!state.lock_retry_armed, "{name} must not arm lock retries");
        assert!(
            dev.count(|call| matches!(call, Call::GetEvent)) == 0,
            "{name} must fail before FE_SET_PROPERTY"
        );
    }
}

#[test]
fn diseqc_10_sequence_and_timing() {
    let mut dev = FakeDevice::new();
    let params = DiseqcParams::default();
    send_diseqc(&mut dev, 1, true, 1, 0, &params, -1);

    let committed = DiseqcMessage {
        msg: [0xe0, 0x10, 0x38, 0xf6, 0, 0],
        len: 4,
    };
    assert_eq!(
        dev.calls,
        vec![
            Call::Wakeup(Voltage::V18),
            Call::SetTone(Tone::Off),
            Call::SetVoltage(Voltage::V18),
            Call::Sleep(15),
            Call::SendMasterCmd(committed),
            Call::Sleep(54),
            // The empty uncommitted pass still sleeps `before_cmd` (dvb.cpp:1019).
            Call::Sleep(15),
            Call::Sleep(15),
            Call::SendBurst(MiniCmd::B),
            Call::Sleep(15),
            Call::SetTone(Tone::Off),
            Call::Sleep(0),
        ]
    );
}

#[test]
fn diseqc_11_repeats_and_uncommitted_first() {
    let mut dev = FakeDevice::new();
    let mut params = DiseqcParams {
        committed_no: 0,
        uncommitted_no: 2,
        ..DiseqcParams::default()
    };
    send_diseqc(&mut dev, 2, true, 0, 0, &params, -1);
    let uncommitted = DiseqcMessage {
        msg: [0xe0, 0x10, 0x39, 0xf2, 0, 0],
        len: 4,
    };
    assert_eq!(dev.master_cmds(), vec![uncommitted, uncommitted]);

    dev.calls.clear();
    params.committed_no = 1;
    params.uncommitted_no = 1;
    send_diseqc(&mut dev, 5, true, 0, 0, &params, -1);
    assert_eq!(
        dev.master_cmds(),
        vec![
            DiseqcMessage {
                msg: [0xe0, 0x10, 0x38, 0xf4, 0, 0],
                len: 4,
            },
            DiseqcMessage {
                msg: [0xe0, 0x10, 0x39, 0xf1, 0, 0],
                len: 4,
            },
        ]
    );
}

#[test]
fn diseqc_multi_resends_previous_position_first() {
    let mut dev = FakeDevice::new();
    let params = DiseqcParams::default();
    send_diseqc(&mut dev, 1, true, 0, 0, &params, 3);
    assert_eq!(
        dev.master_cmds(),
        vec![
            DiseqcMessage {
                msg: [0xe0, 0x10, 0x38, 0xfc, 0, 0],
                len: 4,
            },
            DiseqcMessage {
                msg: [0xe0, 0x10, 0x38, 0xf4, 0, 0],
                len: 4,
            },
        ]
    );
}

#[test]
fn diseqc_fast_mode_skips_unchanged_position() {
    let mut dev = FakeDevice::new();
    let params = DiseqcParams {
        fast: 1,
        ..DiseqcParams::default()
    };
    send_diseqc(&mut dev, 2, false, 0, 1, &params, -1);
    assert!(dev.master_cmds().is_empty());
    assert_eq!(dev.count(|call| matches!(call, Call::SendBurst(_))), 0);
    assert!(
        dev.calls.contains(&Call::SetTone(Tone::On)),
        "the high-band tone is still set"
    );
}

fn unicable_state() -> AdapterState {
    let mut state = state_with_switch(-1, -1, -1);
    state.tp = sat_transponder(11_500_000);
    state.tp.diseqc_param.switch_type = SWITCH_UNICABLE;
    state.tp.diseqc_param.ufreq = 1400;
    state.tp.diseqc_param.uslot = 0;
    state
}

fn jess_state() -> AdapterState {
    let mut state = state_with_switch(-1, -1, -1);
    state.tp = sat_transponder(11_500_000);
    state.tp.diseqc_param.switch_type = SWITCH_JESS;
    state.tp.diseqc_param.ufreq = 1400;
    state.tp.diseqc_param.uslot = 1;
    state
}

#[test]
fn unicable_en50494_message_and_power_sequence() {
    let mut dev = FakeDevice::new();
    let mut state = unicable_state();
    assert_eq!(state.setup_switch(&mut dev, false, None), 1_400_000);
    assert_eq!(
        dev.master_cmds(),
        vec![DiseqcMessage {
            msg: [0xe0, 0x10, 0x5a, 0x09, 0xb6, 0],
            len: 5,
        }]
    );
    assert_eq!(
        dev.calls,
        vec![
            Call::Wakeup(Voltage::V13),
            Call::SetVoltage(Voltage::V13),
            Call::SetTone(Tone::Off),
            Call::Sleep(0),
            Call::SetVoltage(Voltage::V18),
            Call::Sleep(15),
            Call::SendMasterCmd(DiseqcMessage {
                msg: [0xe0, 0x10, 0x5a, 0x09, 0xb6, 0],
                len: 5,
            }),
            Call::Sleep(54),
            Call::SetVoltage(Voltage::V13),
        ]
    );
    assert!(
        state.old_pol == 0 && state.old_hiband == 0 && state.old_diseqc == 0,
        "Unicable state ignores polarity and band"
    );
}

#[test]
fn unicable_pin_and_only13v_variants() {
    let mut dev = FakeDevice::new();
    let mut state = unicable_state();
    state.tp.diseqc_param.pin = 200;
    state.tp.diseqc_param.only13v = 1;
    assert_eq!(state.setup_switch(&mut dev, false, None), 1_400_000);
    assert_eq!(
        dev.master_cmds(),
        vec![DiseqcMessage {
            msg: [0xe0, 0x10, 0x5c, 0x09, 0xb6, 200],
            len: 6,
        }]
    );
    assert_eq!(
        dev.count(|call| *call == Call::SetVoltage(Voltage::V18)),
        0,
        "only13v must not apply 18 V"
    );

    dev.calls.clear();
    let mut tuned = unicable_state();
    tuned.tune_time = 123;
    tuned.tp.diseqc_param.pin = 200;
    tuned.setup_switch(&mut dev, false, None);
    assert_eq!(
        dev.calls.first(),
        Some(&Call::Wakeup(Voltage::V13)),
        "a retune keeps the wakeup hook first"
    );
    assert_eq!(
        dev.calls.get(1),
        Some(&Call::SetVoltage(Voltage::V18)),
        "a tuned adapter skips the initial 13 V/tone reset"
    );
}

#[test]
fn jess_en50607_message_and_pin() {
    let mut dev = FakeDevice::new();
    let mut state = jess_state();
    assert_eq!(state.setup_switch(&mut dev, false, None), 1_400_000);
    assert_eq!(
        dev.master_cmds(),
        vec![DiseqcMessage {
            msg: [0x70, 0x0e, 0x72, 0x02, 0, 0],
            len: 4,
        }]
    );

    dev.calls.clear();
    let mut pinned = jess_state();
    pinned.tp.diseqc_param.pin = 123;
    pinned.setup_switch(&mut dev, false, None);
    assert_eq!(
        dev.master_cmds(),
        vec![DiseqcMessage {
            msg: [0x71, 0x0e, 0x72, 0x02, 123, 0],
            len: 5,
        }]
    );
}

#[test]
fn setup_switch_master_and_slave_sharing() {
    let mut dev = FakeDevice::new();
    let mut state = state_with_switch(1, 0, 0);
    state.tp = sat_transponder(11_000_000);

    let _ = state.setup_switch(&mut dev, false, None);
    assert_eq!(
        dev.count(|call| matches!(call, Call::Wakeup(_))),
        0,
        "an exact switch-state match remains a no-op"
    );

    let mut master = SwitchState {
        pol: 1,
        hiband: 0,
        diseqc: 0,
    };
    let _ = state.setup_switch(
        &mut dev,
        false,
        Some(MasterView {
            state: &mut master,
            idle: true,
        }),
    );
    assert_eq!(
        dev.count(|call| matches!(call, Call::Wakeup(_))),
        1,
        "an idle master accepts the slave parameters"
    );
    assert_eq!(
        master,
        SwitchState {
            pol: 1,
            hiband: 0,
            diseqc: 0
        }
    );

    dev.calls.clear();
    let mut busy = SwitchState::default();
    assert_eq!(
        state.setup_switch(
            &mut dev,
            false,
            Some(MasterView {
                state: &mut busy,
                idle: false,
            })
        ),
        -1
    );
    assert!(dev.calls.is_empty(), "a busy master rejects the change");
    assert_eq!(busy, SwitchState::default(), "the busy master is unchanged");
    assert!(
        state.old_pol == 1 && state.old_hiband == 0 && state.old_diseqc == 0,
        "a rejected slave tune keeps its own cache"
    );
}

#[test]
fn absolute_source_mapping_and_filtering() {
    let mut sources = AbsoluteSources::default();
    assert_eq!(sources.map(1, SYS_DVBS), 1);
    sources.set(0, 2, 4);
    assert_eq!(sources.map(1, SYS_DVBS), 5);
    assert_eq!(sources.map(2, SYS_DVBS2), 6);
    assert_eq!(sources.map(3, SYS_DVBS), 7);
    assert_eq!(sources.map(1, SYS_DVBT), 1);
    assert_eq!(sources.map(0, SYS_DVBS), 0);
    assert_eq!(
        sources.map(MAX_SOURCES as i32 + 1, SYS_DVBS),
        MAX_SOURCES as i32 + 1
    );

    let mut tp = sat_transponder(11_000_000);
    tp.diseqc = Some(3);
    assert!(sources.source_enabled(&tp));
    tp.diseqc = Some(4);
    assert!(!sources.source_enabled(&tp));
    tp.diseqc = Some(0);
    assert!(sources.source_enabled(&tp));
    tp.sys = Some(SYS_DVBT);
    tp.diseqc = Some(4);
    assert!(sources.source_enabled(&tp));

    let mut empty = AbsoluteSources::default();
    empty.enable();
    tp.sys = Some(SYS_DVBS2);
    tp.diseqc = Some(1);
    assert!(
        !empty.source_enabled(&tp),
        "absolute mode excludes sources with no entry"
    );

    let mut invalid = AbsoluteSources::default();
    invalid.set(0, 0, 1);
    invalid.set(MAX_SOURCES as i32, 0, 0);
    invalid.set(0, 0, 15);
    assert_eq!(invalid.map(1, SYS_DVBS), 2, "invalid entries are ignored");
}

fn backend_state() -> AdapterState {
    AdapterState {
        fe: 1,
        ..AdapterState::default()
    }
}

#[test]
fn backend_sets_dvr_buffer_and_per_pid_filters() {
    let mut tuner = LinuxDvbTuner::with_device(FakeDevice::new(), backend_state());
    assert_eq!(
        tuner.device().calls.first(),
        Some(&Call::SetBufferSize(DemuxFd(42), DVR_BUFFER))
    );

    tuner
        .set_pids(&PidSet::from_pids([0_u16, 16]))
        .expect("pids are valid");
    let calls = &tuner.device().calls;
    assert_eq!(
        calls[1..],
        [
            Call::OpenDemux,
            Call::SetPesFilter(DemuxFd(100), 0, PesOutput::TsTap),
            Call::OpenDemux,
            Call::SetPesFilter(DemuxFd(101), 16, PesOutput::TsTap),
        ]
    );

    tuner
        .set_pids(&PidSet::from_pids([16_u16, 32]))
        .expect("pids are valid");
    assert_eq!(
        tuner.device().calls[5..],
        [
            Call::DemuxStop(DemuxFd(100)),
            Call::CloseDemux(DemuxFd(100)),
            Call::OpenDemux,
            Call::SetPesFilter(DemuxFd(102), 32, PesOutput::TsTap),
        ]
    );

    tuner.set_pids(&PidSet::all()).expect("8192 is valid");
    assert!(tuner.device().calls.contains(&Call::SetPesFilter(
        DemuxFd(103),
        PID_ALL,
        PesOutput::TsTap
    )));
    match tuner.set_pids(&PidSet::from_pids([PID_ALL + 1])) {
        Err(TunerError::Backend(-1)) => {}
        other => panic!("PID above 8192 must be rejected, got {other:?}"),
    }
}

#[test]
fn backend_read_returns_whole_packets_and_keeps_partial() {
    let mut dev = FakeDevice::new();
    dev.stream = vec![0x47; 188 * 2 + 50];
    let mut tuner = LinuxDvbTuner::with_device(dev, backend_state());

    let mut buf = TsBuf::with_packets(4);
    assert_eq!(tuner.read(&mut buf).expect("read succeeds"), 376);
    assert_eq!(buf.len(), 376);
    assert_eq!(
        tuner.read(&mut buf).expect("short read succeeds"),
        0,
        "the incomplete tail is not emitted"
    );

    let mut small = FakeDevice::new();
    small.stream = vec![0x47; 188 * 2];
    let mut tuner = LinuxDvbTuner::with_device(small, backend_state());
    let mut buf = TsBuf::with_packets(1);
    assert_eq!(tuner.read(&mut buf).expect("read succeeds"), 188);
    buf.clear();
    assert_eq!(tuner.read(&mut buf).expect("read succeeds"), 188);
    assert_eq!(buf.len(), 188);
}

#[test]
fn signal_scaling_matches_cpp_tables() {
    assert_eq!(calibrate_snr(SnrCalib::Bcm3158Vu, 25_000), 65_535);
    assert_eq!(calibrate_snr(SnrCalib::BcmFbc, 41), 655);
    assert_eq!(calibrate_snr(SnrCalib::NimFbc, 30_000), 32_439);
    assert_eq!(calibrate_snr(SnrCalib::None, 0x1234), 0x1234);
    assert_eq!(strength_percent(10_000), 6_553_500);
    assert_eq!(strength_decibels(0), 65_535);
    assert_eq!(snr_decibels(100_000, 1.0), (999, 65_535));
    assert_eq!(apply_multiplier(65_000, 2.0), 65_535);
    assert_eq!(apply_multiplier(0x4000, 1.0), 0x4000);
}

#[test]
fn snr_calibration_names_match_enigma2() {
    assert_eq!(
        snr_calibration("Vuplus DVB-C NIM(BCM3158)"),
        SnrCalib::Bcm3158Vu
    );
    assert_eq!(snr_calibration("BCM45308"), SnrCalib::BcmFbc);
    assert_eq!(snr_calibration("NIM(45308X FBC)"), SnrCalib::NimFbc);
    assert_eq!(snr_calibration("Mock FE"), SnrCalib::None);
}
