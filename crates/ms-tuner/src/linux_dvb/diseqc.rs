//! DiSEqC 1.0/1.1, unicable (EN 50494), JESS (EN 50607) and `setup_switch` LNB sharing.

use super::consts::{SWITCH_JESS, SWITCH_UNICABLE};
use super::device::{DiseqcMessage, MiniCmd, SecDevice, Tone, Voltage, error_parts};
use super::transponder::{DiseqcParams, DvbTransponder, lnb_hiband, lnb_int_freq};

/// `#define DEFAULT_LOG LOG_DVB` (dvb.cpp:53).
const DEFAULT_LOG: u32 = ms_log::LOG_DVB;

/// Last switch state sent by an adapter (`old_pol`, `old_hiband`, `old_diseqc`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SwitchState {
    pub pol: i32,
    pub hiband: i32,
    pub diseqc: i32,
}

impl Default for SwitchState {
    // adapter_alloc sets all three to -1 (adapter.cpp:94-96).
    fn default() -> Self {
        Self {
            pol: -1,
            hiband: -1,
            diseqc: -1,
        }
    }
}

/// The master adapter as seen by a slave in `setup_switch`.
#[derive(Debug)]
pub struct MasterView<'a> {
    /// `master->id`, used by the C++ log messages.
    pub id: i32,
    /// The master's cached switch state; the slave updates it.
    pub state: &'a mut SwitchState,
    /// `!master->sid_cnt` and no other slave marked in `master->used`.
    pub idle: bool,
}

/// Per-call inputs of `setup_switch` besides the transponder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SwitchOptions {
    /// `ad->id`, used by the C++ log messages.
    pub own_id: i32,
    /// Resend even when nothing changed (lock recovery).
    pub force: bool,
    /// `ad->tune_time != 0`: the adapter already tuned once since it was opened.
    pub tuned_before: bool,
    /// `ad->diseqc_multi` (`-0`), -1 when unset.
    pub diseqc_multi: i32,
}

/// `diseqc_cmd` (dvb.cpp:949): send `cmd` `times` times with the `-q` delays.
///
/// `name` is the C++ `str` argument ("committed", "uncommitted", ...), logged on
/// failure.
pub fn diseqc_cmd<S: SecDevice + ?Sized>(
    dev: &mut S,
    times: i32,
    name: &str,
    cmd: &DiseqcMessage,
    d: &DiseqcParams,
) {
    dev.sleep_ms(d.before_cmd);
    for i in 0..times {
        if let Err(error) = dev.send_master_cmd(cmd) {
            let fd = dev.log_fd();
            let (_, text) = error_parts(&error);
            ms_log::log!(
                "send_diseqc: FE_DISEQC_SEND_MASTER_CMD {} failed for fd {}: {}",
                name,
                fd,
                text
            );
        }
        dev.sleep_ms(if i > 0 {
            d.after_repeated_cmd
        } else {
            d.after_cmd
        });
    }
}

/// Committed/uncommitted positions used by `send_diseqc` and its log line.
fn diseqc_positions(pos: i32, d: &DiseqcParams) -> (i32, i32) {
    let mut committed_no = d.committed_no;
    if committed_no == 0 && d.uncommitted_no == 0 {
        committed_no = 1;
    }
    if d.uncommitted_no > 0 && committed_no > 0 {
        (pos % 4, pos / 4)
    } else {
        (pos, pos)
    }
}

/// Committed (1.0) and uncommitted (1.1) messages built by `send_diseqc` (dvb.cpp:969-997).
pub fn diseqc_messages(
    pos: i32,
    pol: i32,
    hiband: i32,
    d: &DiseqcParams,
) -> (DiseqcMessage, DiseqcMessage) {
    let (posc, posu) = diseqc_positions(pos, d);
    let committed = DiseqcMessage {
        msg: [
            0xe0,
            d.addr as u8,
            0x38,
            (0xf0 | ((posc << 2) & 0x0c) | i32::from(hiband != 0) | if pol != 0 { 2 } else { 0 })
                as u8,
            0,
            0,
        ],
        len: 4,
    };
    let uncommitted = DiseqcMessage {
        msg: [0xe0, d.addr as u8, 0x39, (0xf0 | (posu & 0x0f)) as u8, 0, 0],
        len: 4,
    };
    (committed, uncommitted)
}

/// `send_diseqc` (dvb.cpp:962): voltage, DiSEqC 1.0/1.1, tone burst, then the 22 kHz tone.
pub fn send_diseqc<S: SecDevice + ?Sized>(
    dev: &mut S,
    pos: i32,
    pos_change: bool,
    pol: i32,
    hiband: i32,
    d: &DiseqcParams,
    diseqc_multi: i32,
) {
    let mut pos_change = pos_change;
    if pos_change && diseqc_multi >= 0 && pos != diseqc_multi {
        send_diseqc(dev, diseqc_multi, true, pol, hiband, d, diseqc_multi);
        pos_change = true;
    }
    let uncommitted_first = d.uncommitted_no > d.committed_no;
    let mut committed_no = d.committed_no;
    if committed_no == 0 && d.uncommitted_no == 0 {
        committed_no = 1;
    }
    let (cmd, uncmd) = diseqc_messages(pos, pol, hiband, d);
    let (posc, posu) = diseqc_positions(pos, d);
    let voltage = if pol != 0 { Voltage::V18 } else { Voltage::V13 };
    let fd = dev.log_fd();

    ms_log::logm!(
        "send_diseqc fd {}, pos = {} (c {} u {}), pol = {}, hiband = {}",
        fd,
        pos,
        posc,
        posu,
        pol,
        hiband
    );
    dev.wakeup(voltage);
    if let Err(error) = dev.set_tone(Tone::Off) {
        let (_, text) = error_parts(&error);
        ms_log::log!("send_diseqc: FE_SET_TONE failed for fd {}: {}", fd, text);
    }
    if let Err(error) = dev.set_voltage(voltage) {
        let (_, text) = error_parts(&error);
        ms_log::log!("send_diseqc: FE_SET_VOLTAGE failed for fd {}: {}", fd, text);
    }

    if d.fast == 0 || pos_change {
        if uncommitted_first {
            diseqc_cmd(dev, d.uncommitted_no, "uncommitted", &uncmd, d);
        }
        diseqc_cmd(dev, committed_no, "committed", &cmd, d);
        if !uncommitted_first {
            diseqc_cmd(dev, d.uncommitted_no, "uncommitted", &uncmd, d);
        }
        dev.sleep_ms(d.after_switch);
        if let Err(error) = dev.send_burst(if pos & 1 != 0 { MiniCmd::B } else { MiniCmd::A }) {
            let (_, text) = error_parts(&error);
            ms_log::log!(
                "send_diseqc: FE_DISEQC_SEND_BURST failed for fd {}: {}",
                fd,
                text
            );
        }
    }

    dev.sleep_ms(d.after_burst);
    if let Err(error) = dev.set_tone(if hiband != 0 { Tone::On } else { Tone::Off }) {
        let (_, text) = error_parts(&error);
        ms_log::log!("send_diseqc: FE_SET_TONE failed for fd {}: {}", fd, text);
    }
    dev.sleep_ms(d.after_tone);
}

/// EN 50494 ODU_Channel_change (or the PIN variant) built by `send_unicable` (dvb.cpp:1042-1057).
pub fn unicable_message(
    freq_mhz: i32,
    pos: i32,
    pol: i32,
    hiband: i32,
    d: &DiseqcParams,
) -> DiseqcMessage {
    let t = (freq_mhz + d.ufreq + 2) / 4 - 350;
    let mut cmd = DiseqcMessage {
        msg: [0xe0, d.addr as u8, 0x5a, 0, 0, 0],
        len: 5,
    };
    cmd.msg[3] = (((t & 0x0300) >> 8)
        | (d.uslot << 5)
        | if pos != 0 { 0x10 } else { 0 }
        | if hiband != 0 { 4 } else { 0 }
        | if pol != 0 { 8 } else { 0 }) as u8;
    cmd.msg[4] = (t & 0xff) as u8;
    if d.pin > 0 && d.pin < 256 {
        cmd.len = 6;
        cmd.msg[2] = 0x5c;
        cmd.msg[5] = d.pin as u8;
    }
    cmd
}

/// EN 50607 ODU_Channel_change (or the PIN variant) built by `send_jess` (dvb.cpp:1088-1103).
pub fn jess_message(
    freq_mhz: i32,
    pos: i32,
    pol: i32,
    hiband: i32,
    d: &DiseqcParams,
) -> DiseqcMessage {
    let t = freq_mhz - 100;
    let mut cmd = DiseqcMessage {
        msg: [0x70, 0, 0, 0, 0, 0],
        len: 4,
    };
    cmd.msg[1] = ((d.uslot << 3) as u8) | ((t >> 8) & 0x07) as u8;
    cmd.msg[2] = (t & 0xff) as u8;
    cmd.msg[3] =
        (((pos & 0x3f) << 2) | if pol != 0 { 2 } else { 0 } | i32::from(hiband != 0)) as u8;
    if d.pin > 0 && d.pin < 256 {
        cmd.len = 5;
        cmd.msg[0] = 0x71;
        cmd.msg[4] = d.pin as u8;
    }
    cmd
}

// Shared tail of send_unicable/send_jess (dvb.cpp:1063-1083, 1110-1132).
fn send_single_cable<S: SecDevice + ?Sized>(
    dev: &mut S,
    name: &str,
    cmd: &DiseqcMessage,
    d: &DiseqcParams,
    tuned_before: bool,
) -> i32 {
    let fd = dev.log_fd();
    dev.wakeup(Voltage::V13);
    if !tuned_before {
        if let Err(error) = dev.set_voltage(Voltage::V13) {
            let (_, text) = error_parts(&error);
            ms_log::log!(
                "{}: pre voltage  SEC_VOLTAGE_13 failed for fd {}: {}",
                name,
                fd,
                text
            );
        }
        if let Err(error) = dev.set_tone(Tone::Off) {
            let (_, text) = error_parts(&error);
            ms_log::log!("{}: FE_SET_TONE failed for fd {}: {}", name, fd, text);
        }
        dev.sleep_ms(d.after_tone);
    }
    if d.only13v == 0
        && let Err(error) = dev.set_voltage(Voltage::V18)
    {
        let (_, text) = error_parts(&error);
        ms_log::log!("{}: FE_SET_VOLTAGE failed for fd {}: {}", name, fd, text);
    }
    diseqc_cmd(dev, d.committed_no, name, cmd, d);
    if let Err(error) = dev.set_voltage(Voltage::V13) {
        let (_, text) = error_parts(&error);
        ms_log::log!("{}: FE_SET_VOLTAGE failed for fd {}: {}", name, fd, text);
    }
    d.ufreq.wrapping_mul(1000)
}

/// `send_unicable` (dvb.cpp:1040): returns the user-band frequency in kHz.
pub fn send_unicable<S: SecDevice + ?Sized>(
    dev: &mut S,
    freq_mhz: i32,
    pos: i32,
    pol: i32,
    hiband: i32,
    d: &DiseqcParams,
    tuned_before: bool,
) -> i32 {
    let cmd = unicable_message(freq_mhz, pos, pol, hiband, d);
    let fd = dev.log_fd();
    ms_log::logm!(
        "send_unicable fd {}, freq {}, ufreq {}, pos = {}, pol = {}, hiband = {}, slot {}, pin {}, diseqc => {:02x} {:02x} {:02x} {:02x} {:02x}",
        fd,
        freq_mhz,
        d.ufreq,
        pos,
        pol,
        hiband,
        d.uslot,
        d.pin,
        cmd.msg[0],
        cmd.msg[1],
        cmd.msg[2],
        cmd.msg[3],
        cmd.msg[4]
    );
    send_single_cable(dev, "send_unicable", &cmd, d, tuned_before)
}

/// `send_jess` (dvb.cpp:1086): returns the user-band frequency in kHz.
pub fn send_jess<S: SecDevice + ?Sized>(
    dev: &mut S,
    freq_mhz: i32,
    pos: i32,
    pol: i32,
    hiband: i32,
    d: &DiseqcParams,
    tuned_before: bool,
) -> i32 {
    let cmd = jess_message(freq_mhz, pos, pol, hiband, d);
    let fd = dev.log_fd();
    ms_log::logm!(
        "send_jess fd {}, freq {}, ufreq {}, pos = {}, pol = {}, hiband = {}, slot {}, pin {}, diseqc => {:02x} {:02x} {:02x} {:02x} {:02x}",
        fd,
        freq_mhz,
        d.ufreq,
        pos,
        pol,
        hiband,
        d.uslot,
        d.pin,
        cmd.msg[0],
        cmd.msg[1],
        cmd.msg[2],
        cmd.msg[3],
        cmd.msg[4]
    );
    send_single_cable(dev, "send_jess", &cmd, d, tuned_before)
}

/// `setup_switch` (dvb.cpp:1135): drives the LNB/switch and returns the IF in kHz, or -1.
///
/// `dev` is the master's frontend when `master` is set; `own` is this adapter's cache.
pub fn setup_switch<S: SecDevice + ?Sized>(
    dev: &mut S,
    tp: &DvbTransponder,
    own: &mut SwitchState,
    master: Option<MasterView<'_>>,
    opts: SwitchOptions,
) -> i32 {
    let d = &tp.diseqc_param;
    let diseqc = if tp.diseqc.unwrap_or(0) > 0 {
        tp.diseqc.unwrap_or(0) - 1
    } else {
        0
    };
    let mut pol = (tp.pol.unwrap_or(-1) - 1) & 1;
    let mut freq = lnb_int_freq(tp, d);
    let mut hiband = lnb_hiband(tp, d);

    if d.switch_type == SWITCH_UNICABLE {
        freq = send_unicable(dev, freq / 1000, diseqc, pol, hiband, d, opts.tuned_before);
        hiband = 0;
        pol = 0;
    } else if d.switch_type == SWITCH_JESS {
        freq = send_jess(dev, freq / 1000, diseqc, pol, hiband, d, opts.tuned_before);
        hiband = 0;
        pol = 0;
    } else {
        let changed = |s: &SwitchState| s.pol != pol || s.hiband != hiband || s.diseqc != diseqc;
        let do_setup_switch = match &master {
            None => changed(own) || opts.force,
            Some(m) => {
                if changed(&*m.state) && !m.idle {
                    ms_log::log!(
                        "Slave adapter {} wants to change the paramters of the master adapter {}: pol {} {}, hiband {} {}, diseqc {} {} ",
                        opts.own_id,
                        m.id,
                        pol,
                        own.pol,
                        hiband,
                        own.hiband,
                        diseqc,
                        own.diseqc
                    );
                    return -1;
                }
                m.idle
            }
        };
        if do_setup_switch {
            let pos_change = opts.force || own.diseqc != diseqc;
            send_diseqc(dev, diseqc, pos_change, pol, hiband, d, opts.diseqc_multi);
        } else if master.is_none() {
            ms_log::logm!(
                "Skip sending diseqc commands since the switch position doesn't need to be changed: pol {}, hiband {}, switch position {}",
                pol,
                hiband,
                diseqc
            );
        } else if let Some(m) = &master {
            ms_log::logm!(
                "Slave adapter {} not sending switch updates using master {}",
                opts.own_id,
                m.id
            );
        }
    }

    let new_state = SwitchState {
        pol,
        hiband,
        diseqc,
    };
    if let Some(m) = master {
        *m.state = new_state;
    }
    *own = new_state;
    freq
}
