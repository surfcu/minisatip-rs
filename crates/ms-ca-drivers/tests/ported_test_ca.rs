//! test_ca.cpp `-ca-channels` tests: the parsed option applied to the CA devices.

use std::sync::Arc;

use ms_ca_drivers::ddci::Ddci;
use ms_ca_drivers::en50221::{CaRegistry, ManualClock};
use ms_config::{MAX_CAID, MAX_PMT_PER_CAPMT, parse_args};

/// `ca_devices[]`: the CI slots (CAIDs) and the CAPMT limits DDCI reads.
struct CaDevices {
    slots: CaRegistry,
    cams: Ddci,
}

/// `set_ca_channels(config)` on freshly freed `ca_devices[]`.
fn set_ca_channels(config: &str) -> CaDevices {
    let args = ["-c".to_string(), config.to_string()];
    let opts = parse_args(&args)
        .into_options()
        .expect("arguments should parse");
    let mut d = CaDevices {
        slots: CaRegistry::new(Arc::new(ManualClock::new(0, 0))),
        cams: Ddci::new(),
    };
    d.slots.set_ca_channels(&opts.ca_channels);
    d.cams.set_ca_channels(&opts.ca_channels);
    d
}

impl CaDevices {
    fn allocated(&self, i: usize) -> bool {
        self.slots.get(i as i32).is_some() && self.cams.cams.contains_key(&i)
    }
}

// Ported from tests/test_ca.cpp:802
#[test]
fn test_set_ca_channels_parsing() {
    let d = set_ca_channels("0:*2-0B00-0500");
    assert!(d.allocated(0), "Expected ca_devices[0] to be allocated");
    let (slot, cam) = (d.slots.get(0).unwrap(), &d.cams.cams[&0]);
    assert_eq!(cam.multiple_pmt, 1, "Expected multiple_pmt to be 1");
    assert_eq!(cam.max_ca_pmt, 2, "Expected max_ca_pmt to be 2");
    assert_eq!(slot.caids().len(), 2, "Expected 2 CAIDs to be parsed");
    assert_eq!(slot.caids()[0], 0x0B00, "Expected first CAID to be 0x0B00");
    assert!(slot.has_forced_caids, "Expected has_forced_caids to be 1");

    // ** packs 3 programs per CAPMT; more stars are capped
    let mut d = set_ca_channels("8:**2");
    assert!(
        d.allocated(8) && d.cams.cams[&8].multiple_pmt == 2 && d.cams.cams[&8].max_ca_pmt == 2,
        "Expected 2 CAPMTs with 3 PMTs each"
    );
    d.cams.cams.get_mut(&8).unwrap().enabled = true;
    assert_eq!(d.cams.max_pmt_for_ca(8), 6, "Expected 6 channels on CA 8");

    let d = set_ca_channels("8:****1");
    assert!(
        d.allocated(8) && d.cams.cams[&8].multiple_pmt == (MAX_PMT_PER_CAPMT - 1) as usize,
        "Expected the number of stars to be capped"
    );

    // Test MAX_CAID bounds protection
    let d = set_ca_channels(
        "0:*2-0100-0200-0300-0400-0500-0600-0700-0800-0900-0A00-0B00-0C00-0D00-\
         0E00-0F00-1000-1100-1200-1300-1400-1500-1600",
    );
    assert!(
        d.allocated(0),
        "Expected ca_devices[0] to be allocated on overflow test"
    );
    let slot = d.slots.get(0).unwrap();
    assert_eq!(
        slot.caids().len(),
        MAX_CAID,
        "Expected caids to be clamped to MAX_CAID"
    );
    assert_eq!(
        slot.caids()[MAX_CAID - 1],
        0x1400,
        "Expected 20th CAID to be 0x1400"
    );
}
