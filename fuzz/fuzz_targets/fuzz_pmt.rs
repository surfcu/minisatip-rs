#![no_main]

use libfuzzer_sys::fuzz_target;
use ms_psi::{CrcCheck, Pmt};

fuzz_target!(|data: &[u8]| {
    let _ = Pmt::parse(data, CrcCheck::Skip);
    let _ = Pmt::parse(data, CrcCheck::Verify);
});
