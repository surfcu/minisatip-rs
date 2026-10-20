//! PSI-parsing parts of upstream `tests/test_pmt.cpp`.

use ms_psi::{CrcCheck, Descriptor, Pmt, parse_descriptors};

fn descriptor(bytes: &[u8]) -> Descriptor {
    let mut descriptors = parse_descriptors(bytes).unwrap();
    assert_eq!(descriptors.len(), 1);
    descriptors.pop().unwrap()
}

#[test]
fn descriptor_equality() {
    let descr1 = descriptor(&[0x09, 0x04, 0x0B, 0x00, 0x05, 0x73]);

    // other type
    let descr2 = descriptor(&[0x01, 0x04, 0x0B, 0x00, 0x05, 0x73]);

    // other length
    let descr3 = descriptor(&[0x09, 0x02, 0x0B, 0x00]);

    // other data
    let descr4 = descriptor(&[0x09, 0x04, 0x0B, 0x00, 0x05, 0xAB]);

    // identical
    let descr5 = descriptor(&[0x09, 0x04, 0x0B, 0x00, 0x05, 0x73]);

    assert_ne!(descr1, descr2, "descr1 and descr2 should not match");
    assert_ne!(descr1, descr3, "descr1 and descr3 should not match");
    assert_ne!(descr1, descr4, "descr1 and descr4 should not match");
    assert_eq!(descr1, descr1, "descr1 should match itself");
    assert_eq!(descr1, descr5, "descr1 and descr5 should match");
}

#[test]
fn descriptor_caid_capid_getters() {
    let descr1 = descriptor(&[0x09, 0x04, 0x0B, 0x00, 0x05, 0x73]);

    assert_eq!(descr1.ca_system_id(), Some(0x0B00), "CAID mismatch");
    assert_eq!(descr1.ca_pid(), Some(0x0573), "CA PID mismatch");
}

#[test]
fn multi_service_pid_parses_per_sid() {
    let mut section: [u8; 21] = [
        0x02, 0xB0, 0x12, 0x00, 0x64, 0xC1, 0x00, 0x00, 0xE2, 0xBD, 0xF0, 0x00, 0x02, 0xE2, 0xBD,
        0xF0, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];
    let p1 = Pmt::parse(&section, CrcCheck::Skip).expect("sid 100 to parse");
    section[4] = 0xC8; // sid 200, video pid 702
    section[8] = 0xE2;
    section[9] = 0xBE;
    section[13] = 0xE2;
    section[14] = 0xBE;
    let p2 = Pmt::parse(&section, CrcCheck::Skip).expect("sid 200 to parse");

    assert_eq!(p1.header.table_id_extension, 100);
    assert_eq!(p2.header.table_id_extension, 200);
    assert_ne!(
        p1.header.table_id_extension, p2.header.table_id_extension,
        "each sid should own its own parse result"
    );
    assert_eq!(
        p1.streams.len(),
        1,
        "sid 100 should carry exactly one stream"
    );
    assert_eq!(
        p1.streams[0].elementary_pid, 701,
        "sid 100 should carry pid 701"
    );
    assert_eq!(
        p2.streams.len(),
        1,
        "sid 200 should carry exactly one stream"
    );
    assert_eq!(
        p2.streams[0].elementary_pid, 702,
        "sid 200 should carry pid 702"
    );
}
