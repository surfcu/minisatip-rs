//! Stamps the build date and time used by `datetime_compile`/`minisatip_sym`.
//!
//! C++ composes the value from the compiler's `__DATE__`/`__TIME__`
//! (minisatip.cpp:1149-1150), so it changes whenever the binary is compiled.
//! `MINISATIP_DATETIME_COMPILE` and `MINISATIP_BUILD_STAMP` carry the same
//! value under the names used by the runtime and the symbol table.

use std::time::{SystemTime, UNIX_EPOCH};

/// Converts days since the Unix epoch to a proleptic Gregorian date
/// (Howard Hinnant's `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (if month <= 2 { y + 1 } else { y }, month as u32, day as u32)
}

fn main() {
    let epoch = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or_else(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_secs() as i64)
                .unwrap_or(0)
        });
    let (year, month, day) = civil_from_days(epoch.div_euclid(86_400));
    let seconds = epoch.rem_euclid(86_400);
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let date = format!("{} {:2} {}", MONTHS[month as usize - 1], day, year);
    let time = format!(
        "{:02}:{:02}:{:02}",
        seconds / 3600,
        seconds % 3600 / 60,
        seconds % 60
    );
    // `__DATE__ | __TIME__` as assembled in minisatip.cpp:1149-1150.
    println!("cargo:rustc-env=MINISATIP_DATETIME_COMPILE={date} | {time}");
    // Same value for the `minisatip_sym` symbol table.
    println!("cargo:rustc-env=MINISATIP_BUILD_STAMP={date} | {time}");
}
