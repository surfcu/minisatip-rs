//! SAT>IP tuner description string, ported from `describe_adapter`.
//! Used for the RTCP `SES1` APP packet and for DESCRIBE SDP `a=fmtp` lines.

use ms_tuner::{DeliverySystem, PID_ALL, PidSet, Polarization, SignalStatus, TuneParams};
use std::fmt::Write;

/// Size of the C++ `dad` buffer; output is truncated to one byte less.
pub const DESCRIBE_BUF_LEN: usize = 1000;

/// Adapter-side inputs to the description.
#[derive(Debug, Clone, Copy)]
pub struct DescribeInput<'a> {
    /// Adapter index (`aid`), zero-based.
    pub adapter_id: i32,
    /// Tuning parameters: the adapter's when it is in use, else the stream's.
    pub tune: &'a TuneParams,
    /// Frontend status; `None` matches C++ `ad->status < 0`.
    pub signal: Option<SignalStatus>,
    /// `true` when an adapter is attached and the stream plays (`use_ad`).
    pub use_adapter: bool,
    /// PIDs this stream receives (`get_stream_pids`) or requested (`t->pids`).
    pub pids: &'a PidSet,
}

/// Formats the `ver=...;tuner=...;pids=...` string exactly as C++.
pub fn describe_adapter(input: &DescribeInput<'_>) -> String {
    let t = input.tune;
    let aid = input.adapter_id.max(0);
    let (mut strength, mut status, mut snr) = (255i32, 1i32, 15i32);
    if input.use_adapter
        && let Some(signal) = input.signal
    {
        strength = i32::from(signal.strength);
        snr = i32::from(signal.snr);
        if snr > 15 {
            snr >>= 4;
        }
        status = i32::from(signal.locked);
        if !(0..=255).contains(&strength) {
            strength = 1;
        }
        if !(0..=15).contains(&snr) {
            snr = 1;
        }
    }
    let fe = match t.fe {
        Some(fe) if input.use_adapter && fe > 0 => fe,
        _ => aid + 1,
    };

    let mut out = String::new();
    match t.msys {
        DeliverySystem::Undefined | DeliverySystem::Other(0) => {
            let _ = write!(out, "ver=1.0;src=1;tuner={},0,0,0,0,,,,,,,;pids=", aid + 1);
        }
        DeliverySystem::DvbS | DeliverySystem::DvbS2 => {
            let _ = write!(
                out,
                "ver=1.0;src={};tuner={},{},{},{},{},{},{},{},{},{},{},{};pids=",
                t.src,
                fe,
                strength,
                status,
                snr,
                t.freq / 1_000,
                pol_name(t.pol),
                delsys_name(t.msys),
                modulation_name(t.mtype),
                pilot_name(t.plts),
                rolloff_name(t.ro),
                t.sr / 1_000,
                fec_name(t.fec),
            );
        }
        DeliverySystem::DvbT | DeliverySystem::DvbT2 => {
            let _ = write!(
                out,
                "ver=1.1;tuner={},{},{},{},{:.2},{},{},{},{},{},{},{},{},{};pids=",
                fe,
                strength,
                status,
                snr,
                f64::from(t.freq) / 1_000.0,
                t.bw.unwrap_or(8_000_000) / 1_000_000,
                delsys_name(t.msys),
                tmode_name(t.tmode),
                modulation_name(t.mtype),
                gi_name(t.gi),
                fec_name(t.fec),
                positive(t.plp),
                t.t2id.unwrap_or(0),
                t.sm.unwrap_or(0),
            );
        }
        _ => {
            let _ = write!(
                out,
                "ver=1.2;tuner={},{},{},{},{:.2},{},{},{},{},{},{},{},{};pids=",
                fe,
                strength,
                status,
                snr,
                f64::from(t.freq) / 1_000.0,
                t.bw.unwrap_or(8_000_000) / 1_000_000,
                delsys_name(t.msys),
                modulation_name(t.mtype),
                t.sr / 1_000,
                t.c2tft.unwrap_or(0),
                positive(t.ds),
                positive(t.plp),
                inversion_name(t.specinv),
            );
        }
    }

    if input.use_adapter {
        out.push_str(&adapter_pid_list(input.pids));
    } else {
        out.push_str(&pid_list(input.pids));
    }
    if out.len() >= DESCRIBE_BUF_LEN {
        let mut end = DESCRIBE_BUF_LEN - 1;
        while !out.is_char_boundary(end) {
            end -= 1;
        }
        out.truncate(end);
    }
    out
}

/// Comma-separated PIDs, `all` for the 8192 sentinel, `none` when empty.
pub fn pid_list(pids: &PidSet) -> String {
    if pids.is_all() {
        return "all".to_owned();
    }
    if pids.is_empty() {
        return "none".to_owned();
    }
    let list: Vec<String> = pids
        .iter()
        .filter(|pid| *pid != PID_ALL)
        .map(|pid| pid.to_string())
        .collect();
    list.join(",")
}

/// `get_stream_pids` style list: each PID in order, 8192 as `all`, `none` when empty.
pub fn adapter_pid_list(pids: &PidSet) -> String {
    if pids.is_empty() {
        return "none".to_owned();
    }
    let list: Vec<String> = pids
        .iter()
        .map(|pid| match pid {
            PID_ALL => "all".to_owned(),
            pid => pid.to_string(),
        })
        .collect();
    list.join(",")
}

fn positive(value: Option<i32>) -> String {
    match value {
        Some(value) if value >= 0 => value.to_string(),
        _ => String::new(),
    }
}

fn pol_name(pol: Polarization) -> &'static str {
    match pol {
        Polarization::Horizontal => "h",
        Polarization::Vertical => "v",
        Polarization::Left => "l",
        Polarization::Right => "r",
        Polarization::None => "",
    }
}

/// Delivery system name as used in SAT>IP `msys=`.
pub fn delsys_name(msys: DeliverySystem) -> &'static str {
    match msys {
        DeliverySystem::Undefined => "undefined",
        DeliverySystem::DvbC => "dvbc",
        DeliverySystem::DvbT => "dvbt",
        DeliverySystem::DvbS => "dvbs",
        DeliverySystem::DvbS2 => "dvbs2",
        DeliverySystem::DvbT2 => "dvbt2",
        DeliverySystem::DvbC2 => "dvbc2",
        DeliverySystem::Other(value) => match value {
            0 => "undefined",
            2 => "dvbcb",
            4 => "dss",
            7 => "dvbh",
            8 => "isdbt",
            9 => "isdbs",
            10 => "isdbc",
            11 => "atsc",
            12 => "atscmh",
            13 => "dmbth",
            14 => "cmmb",
            15 => "dab",
            17 => "turbo",
            18 => "dvbcc",
            _ => "",
        },
    }
}

fn modulation_name(mtype: u32) -> &'static str {
    match mtype {
        0 => "qpsk",
        1 => "16qam",
        2 => "32qam",
        3 => "64qam",
        4 => "128qam",
        5 => "256qam",
        7 => "8vsb",
        8 => "16vsb",
        9 => "8psk",
        10 => "16apsk",
        11 => "32apsk",
        12 => "dqpsk",
        13 => "qam_4_nr",
        14 => "64apsk",
        15 => "128apsk",
        16 => "256apsk",
        _ => "",
    }
}

fn fec_name(fec: u8) -> &'static str {
    match fec {
        0 => "none",
        1 => "12",
        2 => "23",
        3 => "34",
        4 => "45",
        5 => "56",
        6 => "67",
        7 => "78",
        8 => "89",
        10 => "35",
        11 => "910",
        12 => "25",
        13 => "14",
        14 => "13",
        _ => "",
    }
}

fn pilot_name(plts: Option<bool>) -> &'static str {
    match plts {
        Some(true) => "on",
        Some(false) => "off",
        None => "",
    }
}

fn rolloff_name(ro: Option<u8>) -> &'static str {
    match ro {
        Some(0) => "0.35",
        Some(1) => "0.20",
        Some(2) => "0.25",
        Some(4) => "0.15",
        Some(5) => "0.10",
        Some(6) => "0.05",
        _ => "",
    }
}

fn gi_name(gi: Option<i32>) -> &'static str {
    match gi {
        Some(0) => "132",
        Some(1) => "116",
        Some(2) => "18",
        Some(3) => "14",
        Some(5) => "1128",
        Some(6) => "19128",
        Some(7) => "19256",
        Some(8) => "pn420",
        Some(9) => "pn595",
        Some(10) => "pn945",
        _ => "",
    }
}

fn tmode_name(tmode: Option<i32>) -> &'static str {
    match tmode {
        Some(0) => "2k",
        Some(1) => "8k",
        Some(3) => "4k",
        Some(4) => "1k",
        Some(5) => "16k",
        Some(6) => "32k",
        Some(7) => "c1",
        Some(8) => "c3780",
        _ => "",
    }
}

fn inversion_name(specinv: Option<i32>) -> &'static str {
    match specinv {
        Some(0) => "0",
        Some(1) => "1",
        _ => "",
    }
}
