#![no_main]

use libfuzzer_sys::fuzz_target;
use ms_ts::{SectionAssembler, TS_PACKET_SIZE};

fuzz_target!(|data: &[u8]| {
    let mut assembled = SectionAssembler::new(true);
    for packet in data.chunks_exact(TS_PACKET_SIZE) {
        let _ = assembled.assemble_packet(packet);
    }

    let mut pushed = SectionAssembler::new(true);
    for packet in data.chunks_exact(TS_PACKET_SIZE) {
        pushed.push(packet, &mut |_section| {});
    }
});
