//! SAT>IP URL query parsing into [`TuneParams`].
//!
//! Frequencies are stored in kHz and symbol rates in kSym/s, matching the
//! `TuneParams` documentation (the C++ stores Hz and sym/s internally).

use crate::rtsp::{parse_int_prefix, split_tokens};
use ms_tuner::{DeliverySystem, PID_ALL, PidSet, Polarization, TuneParams};

/// What a SAT>IP URL changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ParseReport {
    /// A `pids`, `addpids` or `delpids` parameter was present.
    pub pids_specified: bool,
    /// A `freq=` parameter cleared the parameter set, including PIDs.
    pub reset_pids: bool,
    /// The URL contained a query string.
    pub query_present: bool,
}

/// Applies a SAT>IP request URL to fresh parameters.
pub fn parse_satip_url(url: &str, params: &mut TuneParams) -> ParseReport {
    reset_tuning(params);
    apply_satip_url(url, params)
}

/// Applies a SAT>IP request URL to `params`, preserving unspecified values.
pub fn merge_satip_url(url: &str, params: &mut TuneParams) -> ParseReport {
    apply_satip_url(url, params)
}

fn apply_satip_url(url: &str, params: &mut TuneParams) -> ParseReport {
    let Some((_, query)) = url.split_once('?') else {
        return ParseReport::default();
    };

    let mut report = ParseReport {
        query_present: true,
        ..ParseReport::default()
    };
    if query.contains("freq=") {
        params.pids.clear();
        report.reset_pids = true;
    }

    for token in split_tokens(query, '&') {
        let (key, value) = token.split_once('=').unwrap_or((token, ""));
        match key {
            "msys" => params.msys = delivery_system(value),
            "freq" => params.freq = parse_float(value, 1.0).max(0) as u32,
            "pol" => params.pol = polarization(value),
            "sr" => {
                let sr = parse_int_prefix(value, 0);
                params.sr = if (0..=1_000_000).contains(&sr) {
                    sr as u32
                } else {
                    0
                };
            }
            "fe" => params.fe = Some(parse_int_prefix(value, 0)),
            "src" => {
                params.src = u8::try_from(parse_int_prefix(value, 0)).unwrap_or(0);
                params.src_specified = true;
            }
            "ro" => params.ro = rolloff(value),
            "mtype" => {
                if let Some(mtype) = modulation(value) {
                    params.mtype = mtype;
                    params.mtype_specified = true;
                }
            }
            "fec" => {
                if let Some(fec) = code_rate(value) {
                    params.fec = fec;
                    params.fec_specified = true;
                }
            }
            "plts" => params.plts = pilot(value),
            "gi" => params.gi = guard_interval(value),
            "tmode" => params.tmode = transmission_mode(value),
            "bw" => params.bw = Some(bandwidth(value)),
            "specinv" => params.specinv = Some(parse_int_prefix(value, 0)),
            "c2tft" => params.c2tft = Some(parse_int_prefix(value, 0)),
            "ds" => params.ds = Some(parse_int_prefix(value, 0)),
            "plp" | "isi" => params.plp = Some(parse_int_prefix(value, 0)),
            "t2id" => params.t2id = Some(parse_int_prefix(value, 0)),
            "sm" => params.sm = Some(parse_int_prefix(value, 0)),
            "plsm" => params.plsm = pls_mode(value),
            "plsc" => params.plsc = Some(parse_int_prefix(value, 0)),
            "x_pmt" => {
                let pmt = parse_int_prefix(value, -1);
                params.x_pmt = (pmt >= 0).then_some(pmt);
            }
            "pids" => {
                params.pids.clear();
                params.pids.extend(parse_pids_list(value).iter());
                report.pids_specified = true;
            }
            "addpids" => {
                params.pids.extend(parse_pids_list(value).iter());
                report.pids_specified = true;
            }
            "delpids" => {
                for pid in parse_pids_list(value).iter() {
                    params.pids.remove(pid);
                }
                report.pids_specified = true;
            }
            _ => {}
        }
    }

    if params.plsm == Some(0) {
        params.plsc = Some(pls_scrambling_index(params.plsc));
    }
    report
}

/// Parses a URL into fresh parameters.
pub fn parse_satip_url_new(url: &str) -> (TuneParams, ParseReport) {
    let mut params = TuneParams::default();
    let report = parse_satip_url(url, &mut params);
    (params, report)
}

fn reset_tuning(params: &mut TuneParams) {
    params.freq = 0;
    params.pol = Polarization::None;
    params.msys = DeliverySystem::Undefined;
    params.sr = 0;
    params.src = 0;
    params.src_specified = false;
    params.fec = 0;
    params.fec_specified = false;
    params.mtype = 0;
    params.mtype_specified = false;
    params.plts = None;
    params.ro = None;
    params.bw = None;
    params.plp = None;
    params.fe = None;
    params.tmode = None;
    params.gi = None;
    params.t2id = None;
    params.sm = None;
    params.c2tft = None;
    params.ds = None;
    params.specinv = None;
    params.plsm = None;
    params.plsc = None;
    params.x_pmt = None;
}

fn parse_pids_list(value: &str) -> PidSet {
    let mut pids = PidSet::new();
    if value.is_empty() || value == "none" {
        return pids;
    }
    if value == "all" {
        pids.insert(PID_ALL);
        return pids;
    }
    for token in split_tokens(value, ',') {
        match token {
            "all" => pids.insert(PID_ALL),
            "none" => {}
            _ => {
                let pid = parse_int_prefix(token, -1);
                if (0..=i32::from(PID_ALL)).contains(&pid) {
                    pids.insert(pid as u16);
                }
            }
        }
    }
    pids
}

fn delivery_system(value: &str) -> DeliverySystem {
    match value.to_ascii_lowercase().as_str() {
        "undefined" => DeliverySystem::Undefined,
        "dvbc" => DeliverySystem::DvbC,
        "dvbcb" => DeliverySystem::Other(2),
        "dvbt" => DeliverySystem::DvbT,
        "dss" => DeliverySystem::Other(4),
        "dvbs" => DeliverySystem::DvbS,
        "dvbs2" => DeliverySystem::DvbS2,
        "dvbh" => DeliverySystem::Other(7),
        "isdbt" => DeliverySystem::Other(8),
        "isdbs" => DeliverySystem::Other(9),
        "isdbc" => DeliverySystem::Other(10),
        "atsc" => DeliverySystem::Other(11),
        "atscmh" => DeliverySystem::Other(12),
        "dmbth" => DeliverySystem::Other(13),
        "cmmb" => DeliverySystem::Other(14),
        "dab" => DeliverySystem::Other(15),
        "dvbt2" => DeliverySystem::DvbT2,
        "turbo" => DeliverySystem::Other(17),
        "dvbcc" => DeliverySystem::Other(18),
        "dvbc2" => DeliverySystem::DvbC2,
        _ => DeliverySystem::Undefined,
    }
}

fn polarization(value: &str) -> Polarization {
    match value.to_ascii_lowercase().as_str() {
        "v" => Polarization::Vertical,
        "h" => Polarization::Horizontal,
        "r" => Polarization::Right,
        "l" => Polarization::Left,
        "none" => Polarization::None,
        _ => Polarization::None,
    }
}

fn rolloff(value: &str) -> Option<u8> {
    match value.to_ascii_lowercase().as_str() {
        "0.35" => Some(0),
        "0.20" => Some(1),
        "0.25" => Some(2),
        "0.15" => Some(4),
        "0.10" => Some(5),
        "0.05" => Some(6),
        _ => None,
    }
}

fn modulation(value: &str) -> Option<u32> {
    match value.to_ascii_lowercase().as_str() {
        "qpsk" => Some(0),
        "16qam" => Some(1),
        "32qam" => Some(2),
        "64qam" => Some(3),
        "128qam" => Some(4),
        "256qam" => Some(5),
        "auto" => Some(6),
        "8vsb" => Some(7),
        "16vsb" => Some(8),
        "8psk" => Some(9),
        "16apsk" => Some(10),
        "32apsk" => Some(11),
        "dqpsk" => Some(12),
        "qam_4_nr" => Some(13),
        "64apsk" => Some(14),
        "128apsk" => Some(15),
        "256apsk" => Some(16),
        _ => None,
    }
}

fn code_rate(value: &str) -> Option<u8> {
    match value.to_ascii_lowercase().as_str() {
        "none" => Some(0),
        "12" => Some(1),
        "23" => Some(2),
        "34" => Some(3),
        "45" => Some(4),
        "56" => Some(5),
        "67" => Some(6),
        "78" => Some(7),
        "89" => Some(8),
        "auto" => Some(9),
        "35" => Some(10),
        "910" => Some(11),
        "25" => Some(12),
        "14" => Some(13),
        "13" => Some(14),
        _ => None,
    }
}

fn pilot(value: &str) -> Option<bool> {
    match value.to_ascii_lowercase().as_str() {
        "on" => Some(true),
        "off" => Some(false),
        _ => None,
    }
}

fn guard_interval(value: &str) -> Option<i32> {
    match value.to_ascii_lowercase().as_str() {
        "132" => Some(0),
        "116" => Some(1),
        "18" => Some(2),
        "14" => Some(3),
        "1128" => Some(5),
        "19128" => Some(6),
        "19256" => Some(7),
        "pn420" => Some(8),
        "pn595" => Some(9),
        "pn945" => Some(10),
        _ => None,
    }
}

fn transmission_mode(value: &str) -> Option<i32> {
    match value.to_ascii_lowercase().as_str() {
        "2k" => Some(0),
        "8k" => Some(1),
        "4k" => Some(3),
        "1k" => Some(4),
        "16k" => Some(5),
        "32k" => Some(6),
        "c1" => Some(7),
        "c3780" => Some(8),
        _ => None,
    }
}

fn pls_mode(value: &str) -> Option<i32> {
    match value.to_ascii_lowercase().as_str() {
        "root" => Some(0),
        "gold" => Some(1),
        "combo" => Some(2),
        _ => None,
    }
}

fn bandwidth(value: &str) -> u32 {
    let mut bandwidth = parse_float(value, 1_000_000.0);
    if !(0..=100_000_000).contains(&bandwidth) {
        bandwidth = parse_float(value, 1_000.0);
    }
    if !(0..=100_000_000).contains(&bandwidth) {
        bandwidth = parse_float(value, 1.0);
    }
    u32::try_from(bandwidth).unwrap_or(0)
}

fn pls_scrambling_index(code: Option<i32>) -> i32 {
    let mut x: u32 = 1;
    for g in 0..0x3ffff {
        if code == Some(x as i32) {
            return g;
        }
        x = (((x ^ (x >> 7)) & 1) << 17) | (x >> 1);
    }
    0x3ffff
}

/// Parses a C-style decimal float prefix and multiplies it, as `parse_float` does.
fn parse_float(text: &str, multiplier: f64) -> i32 {
    let bytes = text.as_bytes();
    if bytes.is_empty() || !(bytes[0] == b'+' || bytes[0] == b'-' || bytes[0].is_ascii_digit()) {
        return 0;
    }
    let mut end = usize::from(bytes[0] == b'+' || bytes[0] == b'-');
    let digits = end;
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        end += 1;
    }
    if end < bytes.len() && bytes[end] == b'.' {
        end += 1;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
        }
    }
    if end == digits {
        return 0;
    }
    if end < bytes.len() && (bytes[end] == b'e' || bytes[end] == b'E') {
        let mut exponent = end + 1;
        if exponent < bytes.len() && (bytes[exponent] == b'+' || bytes[exponent] == b'-') {
            exponent += 1;
        }
        let start = exponent;
        while exponent < bytes.len() && bytes[exponent].is_ascii_digit() {
            exponent += 1;
        }
        if exponent > start {
            end = exponent;
        }
    }
    let Ok(value) = text[..end].parse::<f64>() else {
        return 0;
    };
    let value = (value as f32) as f64 * multiplier;
    if !(value >= f64::from(i32::MIN) && value <= f64::from(i32::MAX)) {
        return 0;
    }
    value as i32
}
