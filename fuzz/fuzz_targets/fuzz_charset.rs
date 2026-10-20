#![no_main]

use libfuzzer_sys::fuzz_target;
use ms_psi::{CharsetOverride, dvb_get_string};

fuzz_target!(|data: &[u8]| {
    let _ = dvb_get_string(data, 0, CharsetOverride::Auto);
    let _ = dvb_get_string(data, 256, CharsetOverride::Auto);
    let _ = dvb_get_string(data, 256, CharsetOverride::Latin1);
});
