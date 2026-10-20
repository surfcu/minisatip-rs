#![no_main]

use libfuzzer_sys::fuzz_target;
use ms_psi::{CrcCheck, Sdt};

fuzz_target!(|data: &[u8]| {
    let _ = Sdt::parse(data, CrcCheck::Skip);
    let _ = Sdt::parse(data, CrcCheck::Verify);
});
