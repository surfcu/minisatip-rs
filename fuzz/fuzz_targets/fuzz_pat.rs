#![no_main]

use libfuzzer_sys::fuzz_target;
use ms_psi::{CrcCheck, Pat, parse_header};

fuzz_target!(|data: &[u8]| {
    let _ = parse_header(data);
    let _ = Pat::parse(data, CrcCheck::Skip);
    let _ = Pat::parse(data, CrcCheck::Verify);
});
