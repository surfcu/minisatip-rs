//! Frontend discovery (`find_dvb_adapter`, dvb.cpp:2373).
//!
//! Discovery is split from the syscall layer through [`DeviceTree`] and
//! [`FrontendProbe`], so a fake `/dev` tree can exercise the C++ order rules.

use crate::DeliverySystem;

use super::consts::*;
use super::device::{DvbDevice, FrontendInfo};

/// `#define DEFAULT_LOG LOG_DVB` (dvb.cpp:53).
const DEFAULT_LOG: u32 = ms_log::LOG_DVB;

/// C++ scans `adapterN/frontendM` with both indices below `MAX_ADAPTERS`.
pub const MAX_DVB_ADAPTERS: u32 = 100;

/// `/dev/dvb/adapterN/frontendM` (`DEV_FRONTEND`).
pub fn frontend_path(adapter: u32, frontend: u32) -> String {
    format!("/dev/dvb/adapter{adapter}/frontend{frontend}")
}

/// `/dev/dvb/adapterN/demuxM` (`DEV_DEMUX`).
pub fn demux_path(adapter: u32, frontend: u32) -> String {
    format!("/dev/dvb/adapter{adapter}/demux{frontend}")
}

/// `/dev/dvb/adapterN/dvrM` (`DEV_DVR`).
pub fn dvr_path(adapter: u32, frontend: u32) -> String {
    format!("/dev/dvb/adapter{adapter}/dvr{frontend}")
}

/// `/dev/dvb/adapterN/secM` (`DEV_SEC`).
pub fn sec_path(adapter: u32, frontend: u32) -> String {
    format!("/dev/dvb/adapter{adapter}/sec{frontend}")
}

/// Filesystem view used by [`enumerate`].
pub trait DeviceTree {
    /// Whether `path` is readable (`access(path, R_OK) == 0`).
    fn readable(&self, path: &str) -> bool;
    /// Whether `path` is writable (`access(path, W_OK) == 0`).
    fn writable(&self, path: &str) -> bool;
}

/// Capabilities returned by opening one frontend.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProbedFrontend {
    /// `dvb_frontend_info::name` (`dvb_name`, dvb.cpp:1940).
    pub name: String,
    /// Delivery systems from `DTV_ENUM_DELSYS` or the `FE_GET_INFO` fallback.
    pub systems: Vec<DeliverySystem>,
}

/// Capability query for one discovered frontend.
pub trait FrontendProbe {
    /// Opens `adapterN/frontendM`, or returns `None` when it cannot be opened.
    fn probe(&self, adapter: u32, frontend: u32) -> Option<ProbedFrontend>;

    /// [`probe`](Self::probe) with the logical adapter id `dvb_delsys` logs.
    fn probe_with_id(&self, adapter: u32, frontend: u32, aid: i32) -> Option<ProbedFrontend> {
        let _ = aid;
        self.probe(adapter, frontend)
    }
}

/// One discovered frontend, numbered like the C++ adapter table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DvbFrontend {
    /// Sequential adapter number (`adapter::id`, assigned in discovery order).
    pub adapter: u32,
    /// `/dev/dvb/adapterN` number (`adapter::pa`).
    pub path_adapter: u32,
    /// `frontendM` number (`adapter::fn`).
    pub path_frontend: u32,
    /// Frontend name reported by `FE_GET_INFO`.
    pub name: String,
    /// Detected delivery systems.
    pub systems: Vec<DeliverySystem>,
    /// Writable `/proc/stb/frontend/M/fbc_connect` on adapter 0 (`is_fbc`).
    pub fbc: bool,
}

/// Discovers DVB frontends in `find_dvb_adapter` order (dvb.cpp:2373).
///
/// A frontend is accepted when frontend, demux and dvr are all readable and no
/// writable `secM` exists (DDCI devices own their `/dev/dvb` node).
pub fn enumerate(tree: &dyn DeviceTree, probe: &dyn FrontendProbe) -> Vec<DvbFrontend> {
    let mut found = Vec::new();
    for path_adapter in 0..MAX_DVB_ADAPTERS {
        for path_frontend in 0..MAX_DVB_ADAPTERS {
            if tree.writable(&sec_path(path_adapter, path_frontend)) {
                ms_log::log!(
                    "{} device found, this is a DDCI device",
                    sec_path(path_adapter, path_frontend)
                );
                continue;
            }
            let nodes = [
                tree.readable(&frontend_path(path_adapter, path_frontend)),
                tree.readable(&demux_path(path_adapter, path_frontend)),
                tree.readable(&dvr_path(path_adapter, path_frontend)),
            ];
            if !nodes.iter().all(|present| *present) {
                continue;
            }
            let probed = probe
                .probe_with_id(path_adapter, path_frontend, found.len() as i32)
                .unwrap_or_default();
            ms_log::logm!(
                "find_dvb_adapter: adding {} {} to the list of devices",
                path_adapter,
                path_frontend
            );
            let fbc = path_adapter == 0
                && tree.writable(&format!("/proc/stb/frontend/{path_frontend}/fbc_connect"));
            found.push(DvbFrontend {
                adapter: found.len() as u32,
                path_adapter,
                path_frontend,
                name: probed.name,
                systems: probed.systems,
                fbc,
            });
        }
    }
    found
}

/// Queries capabilities through an already-open device (`dvb_delsys`).
///
/// `aid` is the logical adapter id the C++ function receives and logs.
pub fn probe_device<D: DvbDevice + ?Sized>(dev: &mut D, aid: i32) -> ProbedFrontend {
    // `dvb_delsys` queries DTV_ENUM_DELSYS before FE_GET_INFO (dvb.cpp:1868-1884).
    let enumerated = match dev.enum_delsys() {
        Ok(systems) => systems,
        Err(_) => {
            ms_log::log!("unable to query frontend, perhaps DVB-API < 5.5 ?");
            Vec::new()
        }
    };
    let info = match dev.get_info() {
        Ok(info) => info,
        Err(error) => {
            let fd = dev.log_fd();
            let (_, text) = super::device::error_parts(&error);
            ms_log::log!(
                "FE_GET_INFO failed for adapter {}, fd {}: {} ",
                aid,
                fd,
                text
            );
            FrontendInfo::default()
        }
    };
    ms_log::log!(
        "Detected adapter {} handle {} DVB Card Name: {}",
        aid,
        dev.log_fd(),
        info.name
    );
    let systems: Vec<DeliverySystem> = if enumerated.is_empty() {
        let systems = systems_from_info(&info);
        // The C++ `default:` arm is the only one that logs and returns empty.
        if systems.is_empty() && !matches!(info.fe_type, FE_OFDM | FE_QAM | FE_QPSK | FE_ATSC) {
            ms_log::log!("no available delivery system for adapter {}", aid);
        }
        systems
    } else {
        enumerated
            .into_iter()
            .filter_map(delivery_system_from_number)
            .collect()
    };
    for system in &systems {
        let number = super::transponder::delsys_number(*system).unwrap_or(SYS_UNDEFINED);
        ms_log::log!(
            "Detected delivery system for adapter {}: {} [{}]",
            aid,
            crate::names::fe_delsys(number),
            number
        );
    }
    ProbedFrontend {
        name: info.name,
        systems,
    }
}

/// `FE_GET_INFO` fallback used when `DTV_ENUM_DELSYS` is unavailable (dvb.cpp:1886).
pub fn systems_from_info(info: &FrontendInfo) -> Vec<DeliverySystem> {
    let mut systems = Vec::new();
    match info.fe_type {
        FE_OFDM => {
            if info.caps & FE_CAN_2G_MODULATION != 0 {
                systems.push(DeliverySystem::DvbT2);
            }
            systems.push(DeliverySystem::DvbT);
        }
        FE_QAM => systems.push(DeliverySystem::DvbC),
        FE_QPSK => {
            if info.caps & FE_CAN_2G_MODULATION != 0 {
                systems.push(DeliverySystem::DvbS2);
            }
            systems.push(DeliverySystem::DvbS);
        }
        FE_ATSC => {
            if info.caps & (FE_CAN_8VSB | FE_CAN_16VSB) != 0 {
                systems.push(DeliverySystem::Other(SYS_ATSC));
            } else if info.caps & (FE_CAN_QAM_64 | FE_CAN_QAM_256 | FE_CAN_QAM_AUTO) != 0 {
                systems.push(DeliverySystem::Other(SYS_DVBC_ANNEX_B));
            }
        }
        _ => {}
    }
    systems
}

/// Maps a kernel `fe_delivery_system_t` to the crate enum.
pub fn delivery_system_from_number(number: u32) -> Option<DeliverySystem> {
    Some(match number as i32 {
        SYS_UNDEFINED => return None,
        SYS_DVBC_ANNEX_A => DeliverySystem::DvbC,
        SYS_DVBT => DeliverySystem::DvbT,
        SYS_DVBS => DeliverySystem::DvbS,
        SYS_DVBS2 => DeliverySystem::DvbS2,
        SYS_DVBT2 => DeliverySystem::DvbT2,
        SYS_DVBC2 => DeliverySystem::DvbC2,
        other => DeliverySystem::Other(other),
    })
}
