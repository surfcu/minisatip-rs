//! Ports of the `detect_dvb_parameters` tests from `tests/test_dvb.cpp`.

use ms_net::parse_satip_url_new;
use ms_tuner::{DeliverySystem, Polarization};

fn parse(parameters: &str) -> ms_tuner::TuneParams {
    let (params, _) = parse_satip_url_new(&format!("rtsp://127.0.0.1/?{parameters}"));
    params
}

// C++ stores `freq` in kHz; `TuneParams::freq` carries the SAT>IP MHz value
// (DEV-030), so 11 361 750 kHz is 11 361 MHz after the same truncation.
// Ported from tests/test_dvb.cpp:46
#[test]
fn test_detect_dvb_parameters_general() {
    let tp = parse(
        "fe=0&src=1&freq=11361.75&pol=h&ro=0.35&msys=dvbs2&\
         mtype=qpsk&plts=off&sr=22000&fec=23&pids=0,16,201,302",
    );

    assert_eq!(tp.fe, Some(0), "fe parsed incorrectly");
    assert!(tp.src_specified, "src parsed incorrectly");
    assert_eq!(tp.src, 1, "src parsed incorrectly");
    assert_eq!(tp.freq, 11_361, "freq parsed incorrectly");
    assert_eq!(tp.pol, Polarization::Horizontal, "pol parsed incorrectly");
    assert_eq!(tp.ro, Some(0), "ro parsed incorrectly");
    assert_eq!(tp.msys, DeliverySystem::DvbS2, "msys parsed incorrectly");
    assert_eq!(tp.mtype, 0, "mtype parsed incorrectly");
    assert!(tp.mtype_specified, "mtype must be marked as specified");
    assert_eq!(tp.plts, Some(false), "plts parsed incorrectly");
    assert_eq!(tp.sr, 22_000, "sr parsed incorrectly");
    assert_eq!(tp.fec, 2, "fec parsed incorrectly");
    assert!(tp.fec_specified, "fec must be marked as specified");
    assert!(
        tp.pids.len() == 4
            && tp.pids.contains(0)
            && tp.pids.contains(16)
            && tp.pids.contains(201)
            && tp.pids.contains(302),
        "pids parsed incorrectly"
    );
}

// Ported from tests/test_dvb.cpp:69
#[test]
fn test_detect_dvb_parameters_roll_off() {
    let tp = parse("freq=11734");
    assert_eq!(tp.ro, None, "ro parsed incorrectly");

    let tp = parse("freq=11734&ro=0.35");
    assert_eq!(tp.ro, Some(0), "ro parsed incorrectly");
    let tp = parse("freq=11734&ro=0.25");
    assert_eq!(tp.ro, Some(2), "ro parsed incorrectly");
    let tp = parse("freq=11734&ro=0.20");
    assert_eq!(tp.ro, Some(1), "ro parsed incorrectly");
}

// Ported from tests/test_dvb.cpp:91
#[test]
fn test_detect_dvb_parameters_edge_cases() {
    let tp = parse("freq=11362&pids=all");
    assert!(
        tp.pids.len() == 1 && tp.pids.contains(8192),
        "pids=all parsed incorrectly"
    );

    let tp = parse("freq=11362&pids=none");
    assert!(tp.pids.is_empty(), "pids=none parsed incorrectly");

    let tp = parse("pids=1,2,3&addpids=4,5&delpids=2&x_pmt=123");
    assert!(
        tp.pids.len() == 4
            && tp.pids.contains(1)
            && !tp.pids.contains(2)
            && tp.pids.contains(3)
            && tp.pids.contains(4)
            && tp.pids.contains(5),
        "pids/addpids/delpids parsed incorrectly"
    );
    assert_eq!(tp.x_pmt, Some(123), "x_pmt parsed incorrectly");

    let tp = parse("freq=11362&plsm=root&plsc=42");
    assert_eq!(tp.plsm, Some(0), "pls_mode parsed incorrectly");
    assert_eq!(
        tp.plsc,
        Some(114_384),
        "pls_code not calculated for ROOT mode"
    );
}

// Ported from tests/test_dvb.cpp:127
#[test]
fn test_detect_dvb_parameters_pids_bounds() {
    let tp = parse("pids=0,100,8191,8192,8193,99999");
    assert_eq!(tp.pids.len(), 4, "out-of-range pids must be dropped");
    assert!(tp.pids.contains(8192), "the pids=all marker must survive");
    // `contains_exact` matches std::set::contains; `PID_ALL` would widen `contains`.
    // 99999 is not representable as u16; the four-entry count proves it was dropped.
    assert!(
        !tp.pids.contains_exact(8193),
        "pids above 8192 must be dropped"
    );
}

// `freq` uses 0 for "unset" and `sr` has no presence flag in `TuneParams`.
// Ported from tests/test_dvb.cpp:140
#[test]
fn test_detect_dvb_parameters_numeric_bounds() {
    let tp = parse("freq=&src=&sr=3000000");

    assert_eq!(tp.freq, 0, "empty freq must stay unset");
    assert!(!tp.src_specified, "empty src must stay unset");
    assert_eq!(tp.sr, 0, "huge sr must clamp to 0");
}
