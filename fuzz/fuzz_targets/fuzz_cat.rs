#![no_main]

use libfuzzer_sys::fuzz_target;
use ms_psi::{Cat, CrcCheck};

fuzz_target!(|data: &[u8]| {
    let _ = Cat::parse(data, CrcCheck::Skip);
    let _ = Cat::parse(data, CrcCheck::Verify);
});
