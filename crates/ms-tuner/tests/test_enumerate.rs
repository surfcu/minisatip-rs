//! `find_dvb_adapter` discovery against a fake `/dev/dvb` tree (dvb.cpp:2373).

#![cfg(feature = "linux-dvb")]

use std::collections::{BTreeMap, BTreeSet};

use ms_tuner::DeliverySystem;
use ms_tuner::linux_dvb::*;

#[derive(Default)]
struct FakeTree {
    readable: BTreeSet<String>,
    writable: BTreeSet<String>,
}

impl FakeTree {
    fn with_frontend(mut self, adapter: u32, frontend: u32) -> Self {
        for path in [
            frontend_path(adapter, frontend),
            demux_path(adapter, frontend),
            dvr_path(adapter, frontend),
        ] {
            self.readable.insert(path);
        }
        self
    }

    fn with_sec(mut self, adapter: u32, frontend: u32) -> Self {
        self.writable.insert(sec_path(adapter, frontend));
        self
    }
}

impl DeviceTree for FakeTree {
    fn readable(&self, path: &str) -> bool {
        self.readable.contains(path)
    }

    fn writable(&self, path: &str) -> bool {
        self.writable.contains(path)
    }
}

#[derive(Default)]
struct FakeProbe {
    frontends: BTreeMap<(u32, u32), ProbedFrontend>,
}

impl FakeProbe {
    fn set(&mut self, adapter: u32, frontend: u32, name: &str, systems: &[DeliverySystem]) {
        self.frontends.insert(
            (adapter, frontend),
            ProbedFrontend {
                name: name.to_owned(),
                systems: systems.to_vec(),
            },
        );
    }
}

impl FrontendProbe for FakeProbe {
    fn probe(&self, adapter: u32, frontend: u32) -> Option<ProbedFrontend> {
        self.frontends.get(&(adapter, frontend)).cloned()
    }
}

fn frontend(adapter: u32, path_adapter: u32, path_frontend: u32) -> DvbFrontend {
    DvbFrontend {
        adapter,
        path_adapter,
        path_frontend,
        name: String::new(),
        systems: Vec::new(),
        fbc: false,
    }
}

// C++: find_dvb_adapter scans adapter 0..MAX_ADAPTERS and frontend
// 0..MAX_ADAPTERS, numbering adapters in discovery order while keeping pa/fn
// (dvb.cpp:2383-2449).
#[test]
fn two_adapters_with_two_frontends_mixed_s2_and_t2() {
    let tree = FakeTree::default()
        .with_frontend(2, 0)
        .with_frontend(2, 1)
        .with_frontend(5, 0)
        .with_frontend(5, 1);
    let mut probe = FakeProbe::default();
    probe.set(2, 0, "S2-0", &[DeliverySystem::DvbS2, DeliverySystem::DvbS]);
    probe.set(2, 1, "S2-1", &[DeliverySystem::DvbS2, DeliverySystem::DvbS]);
    probe.set(5, 0, "T2-0", &[DeliverySystem::DvbT2, DeliverySystem::DvbT]);
    probe.set(5, 1, "T2-1", &[DeliverySystem::DvbT2, DeliverySystem::DvbT]);

    let found = enumerate(&tree, &probe);
    assert_eq!(found.len(), 4);
    assert_eq!(
        found[0],
        DvbFrontend {
            adapter: 0,
            path_adapter: 2,
            path_frontend: 0,
            name: "S2-0".to_owned(),
            systems: vec![DeliverySystem::DvbS2, DeliverySystem::DvbS],
            fbc: false,
        }
    );
    assert_eq!(
        found[1],
        DvbFrontend {
            adapter: 1,
            path_adapter: 2,
            path_frontend: 1,
            name: "S2-1".to_owned(),
            systems: vec![DeliverySystem::DvbS2, DeliverySystem::DvbS],
            fbc: false,
        }
    );
    assert_eq!(
        found[2],
        DvbFrontend {
            adapter: 2,
            path_adapter: 5,
            path_frontend: 0,
            name: "T2-0".to_owned(),
            systems: vec![DeliverySystem::DvbT2, DeliverySystem::DvbT],
            fbc: false,
        }
    );
    assert_eq!(
        found[3],
        DvbFrontend {
            adapter: 3,
            path_adapter: 5,
            path_frontend: 1,
            name: "T2-1".to_owned(),
            systems: vec![DeliverySystem::DvbT2, DeliverySystem::DvbT],
            fbc: false,
        }
    );
}

// C++: a writable secM marks a DDCI device and excludes the frontend
// (dvb.cpp:2398-2404).
#[test]
fn writable_sec_device_is_not_a_dvb_adapter() {
    let tree = FakeTree::default()
        .with_frontend(0, 0)
        .with_frontend(1, 0)
        .with_sec(1, 0);
    let mut probe = FakeProbe::default();
    probe.set(0, 0, "FE", &[DeliverySystem::DvbS2]);
    probe.set(1, 0, "DDCI", &[DeliverySystem::DvbS2]);

    let found = enumerate(&tree, &probe);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].path_adapter, 0);
    assert_eq!(found[0].path_frontend, 0);
}

// C++ counts frontend, demux and dvr; only cnt == 3 is accepted
// (dvb.cpp:2385-2406).
#[test]
fn frontend_demux_and_dvr_must_all_be_readable() {
    let mut tree = FakeTree::default();
    tree.readable.insert(frontend_path(0, 0));
    tree.readable.insert(frontend_path(0, 2));
    tree.readable.insert(demux_path(0, 2));
    tree = tree.with_frontend(0, 1);

    let mut probe = FakeProbe::default();
    probe.set(0, 1, "complete", &[DeliverySystem::DvbT2]);
    probe.set(0, 2, "missing-dvr", &[DeliverySystem::DvbT2]);

    let found = enumerate(&tree, &probe);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].path_frontend, 1);
}

// C++ allocates the adapter slot before opening it; a failed FE_GET_INFO
// leaves an adapter with no name/delsys (dvb.cpp:2414-2421).
#[test]
fn probe_failure_still_numbers_the_adapter() {
    let tree = FakeTree::default().with_frontend(7, 3);
    let probe = FakeProbe::default();

    let found = enumerate(&tree, &probe);
    assert_eq!(found, vec![frontend(0, 7, 3)]);
}

// C++: dvb_delsys falls back to FE_GET_INFO when DTV_ENUM_DELSYS is missing
// (dvb.cpp:1856-1910); get_lnb... uses FE_CAN_2G_MODULATION for S2/T2.
#[test]
fn frontend_info_fallback_maps_every_type() {
    fn info(fe_type: u32, caps: u32) -> FrontendInfo {
        FrontendInfo {
            name: "FE".to_owned(),
            fe_type,
            caps,
        }
    }

    assert_eq!(
        systems_from_info(&info(FE_OFDM, FE_CAN_2G_MODULATION)),
        vec![DeliverySystem::DvbT2, DeliverySystem::DvbT]
    );
    assert_eq!(
        systems_from_info(&info(FE_OFDM, 0)),
        vec![DeliverySystem::DvbT]
    );
    assert_eq!(
        systems_from_info(&info(FE_QAM, 0)),
        vec![DeliverySystem::DvbC]
    );
    assert_eq!(
        systems_from_info(&info(FE_QPSK, FE_CAN_2G_MODULATION)),
        vec![DeliverySystem::DvbS2, DeliverySystem::DvbS]
    );
    assert_eq!(
        systems_from_info(&info(FE_QPSK, 0)),
        vec![DeliverySystem::DvbS]
    );
    assert_eq!(
        systems_from_info(&info(FE_ATSC, FE_CAN_8VSB)),
        vec![DeliverySystem::Other(SYS_ATSC)]
    );
    assert_eq!(
        systems_from_info(&info(FE_ATSC, FE_CAN_QAM_256)),
        vec![DeliverySystem::Other(SYS_DVBC_ANNEX_B)]
    );
    assert!(systems_from_info(&info(FE_ATSC, 0)).is_empty());
    assert!(systems_from_info(&info(99, 0)).is_empty());
}

// Kernel fe_delivery_system_t -> ms_tuner::DeliverySystem (dvb.h names).
#[test]
fn delivery_system_numbers_match_the_kernel() {
    assert_eq!(delivery_system_from_number(0), None);
    assert_eq!(
        delivery_system_from_number(SYS_DVBC_ANNEX_A as u32),
        Some(DeliverySystem::DvbC)
    );
    assert_eq!(
        delivery_system_from_number(SYS_DVBT as u32),
        Some(DeliverySystem::DvbT)
    );
    assert_eq!(
        delivery_system_from_number(SYS_DVBS as u32),
        Some(DeliverySystem::DvbS)
    );
    assert_eq!(
        delivery_system_from_number(SYS_DVBS2 as u32),
        Some(DeliverySystem::DvbS2)
    );
    assert_eq!(
        delivery_system_from_number(SYS_DVBT2 as u32),
        Some(DeliverySystem::DvbT2)
    );
    assert_eq!(
        delivery_system_from_number(SYS_DVBC2 as u32),
        Some(DeliverySystem::DvbC2)
    );
    assert_eq!(
        delivery_system_from_number(999),
        Some(DeliverySystem::Other(999))
    );
}

// dvb.cpp:2442-2446: only adapter 0 frontends with a writable fbc_connect
// are FBC tuners.
#[test]
fn fbc_frontends_are_marked() {
    let mut tree = FakeTree::default()
        .with_frontend(0, 0)
        .with_frontend(0, 1)
        .with_frontend(1, 0);
    tree.writable
        .insert("/proc/stb/frontend/1/fbc_connect".to_owned());
    tree.writable
        .insert("/proc/stb/frontend/0/fbc_connect".to_owned());
    let found = enumerate(&tree, &FakeProbe::default());
    let fbc: Vec<(u32, u32, bool)> = found
        .iter()
        .map(|f| (f.path_adapter, f.path_frontend, f.fbc))
        .collect();
    assert_eq!(fbc, [(0, 0, true), (0, 1, true), (1, 0, false)]);
}
