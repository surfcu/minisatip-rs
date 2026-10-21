//! Deterministic no-panic checks standing in for the libFuzzer targets.

use ms_psi::{
    Cat, CharsetOverride, CrcCheck, Pat, Pmt, Sdt, dvb_get_string, iter_descriptors, parse_header,
};

fn xorshift(state: &mut u64) -> u64 {
    let mut value = *state;
    value ^= value << 13;
    value ^= value >> 7;
    value ^= value << 17;
    *state = value;
    value
}

#[test]
fn parsers_do_not_panic_on_random_input() {
    let mut state = 0x1234_5678_9ABC_DEF0u64;
    for _ in 0..4000 {
        let length = (xorshift(&mut state) % 600) as usize;
        let mut data = vec![0u8; length];
        for byte in &mut data {
            *byte = xorshift(&mut state) as u8;
        }

        let outcome = std::panic::catch_unwind(|| {
            let _ = parse_header(&data);
            for crc in [CrcCheck::Skip, CrcCheck::Verify] {
                let _ = Pat::parse(&data, crc);
                let _ = Pmt::parse(&data, crc);
                let _ = Cat::parse(&data, crc);
                let _ = Sdt::parse(&data, crc);
            }
            let _ = ms_psi::parse_descriptors(&data);
            let _ = iter_descriptors(&data).count();
            for charset in [CharsetOverride::Auto, CharsetOverride::Latin1] {
                let _ = dvb_get_string(&data, 0, charset);
                let _ = dvb_get_string(&data, 256, charset);
            }
        });
        assert!(outcome.is_ok(), "panic on random input of length {length}");
    }
}
