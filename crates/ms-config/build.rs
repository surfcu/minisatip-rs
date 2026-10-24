//! Stamps the date and time in `__DATE__`/`__TIME__` form, as C++ expands them
//! in `minisatip.cpp:1149-1150`, but in local time.

#![allow(unsafe_code)]

use std::time::{SystemTime, UNIX_EPOCH};

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

fn main() {
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");

    let epoch = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or_else(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_secs() as i64)
                .unwrap_or(0)
        });
    let (date, time) = local_stamp(epoch);
    println!("cargo:rustc-env=MS_BUILD_DATE={date}");
    println!("cargo:rustc-env=MS_BUILD_TIME={time}");

    // `DVBAPIVERSION` (`dvb.h:19`): the C++ build takes it from the headers.
    let s2api = if std::env::var_os("CARGO_FEATURE_LINUX_DVB").is_some() {
        "050C"
    } else {
        "0000"
    };
    println!("cargo:rustc-env=MS_S2API_VERSION={s2api}");
}

/// `__DATE__` ("Mmm dd yyyy", day space-padded) and `__TIME__` in local time.
#[cfg(unix)]
fn local_stamp(epoch: i64) -> (String, String) {
    // SAFETY: localtime_r only reads `seconds` and writes into the stack `tm`.
    unsafe {
        let seconds = epoch as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&seconds, &mut tm).is_null() {
            return utc_stamp(epoch);
        }
        let month = MONTHS[tm.tm_mon.clamp(0, 11) as usize];
        (
            format!("{month} {:2} {}", tm.tm_mday, tm.tm_year + 1900),
            format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec),
        )
    }
}

/// Non-unix hosts (the build script runs on the host) have no `localtime_r`.
#[cfg(not(unix))]
fn local_stamp(epoch: i64) -> (String, String) {
    utc_stamp(epoch)
}

/// Deterministic UTC fallback when `localtime_r` fails.
fn utc_stamp(epoch: i64) -> (String, String) {
    let days = epoch.div_euclid(86_400);
    let seconds = epoch.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    (
        format!("{} {day:2} {year}", MONTHS[month as usize - 1]),
        format!(
            "{:02}:{:02}:{:02}",
            seconds / 3_600,
            seconds % 3_600 / 60,
            seconds % 60
        ),
    )
}

/// Days since 1970-01-01 to a Gregorian date (Howard Hinnant's `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (if month <= 2 { y + 1 } else { y }, month as u32, day as u32)
}
