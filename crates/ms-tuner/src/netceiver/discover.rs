//! `find_netcv_adapter`: waits for the netceivers and turns their tuners into adapters.

use std::fmt;

use super::api::{API_SOCK_NAMESPACE, MCLI_PORT, McliDiscoveryApi, NetceiverInfo};
use super::params::{FE_ATSC, FE_DVBS2, FE_OFDM, FE_QAM, FE_QPSK};
use crate::tuner::DeliverySystem;

/// Tuner types in the order adapters are handed out.
const MAP_TYPE: [i32; 5] = [FE_DVBS2, FE_QPSK, FE_QAM, FE_OFDM, FE_ATSC];

/// `-n` settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetcvConfig {
    /// `opts.netcv_if`.
    pub ifname: String,
    /// `opts.netcv_count`.
    pub count: usize,
}

/// One adapter created for a netceiver tuner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetcvAdapter {
    /// Logical adapter id.
    pub id: usize,
    /// Tuner type it was assigned (`FE_*`).
    pub fe_type: i32,
    /// `ad->sys`, most capable first.
    pub sys: Vec<DeliverySystem>,
}

/// Result of [`find_netcv_adapters`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NetcvDiscovery {
    /// Netceivers seen when the list was locked.
    pub netceivers: Vec<NetceiverInfo>,
    /// Adapters to create.
    pub adapters: Vec<NetcvAdapter>,
    /// New `a_count`.
    pub a_count: usize,
}

/// Discovery failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetcvError {
    /// The interface is missing or not up/running/multicast (C++ logs and `exit(0)`).
    InterfaceUnavailable(String),
}

impl fmt::Display for NetcvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InterfaceUnavailable(name) => write!(f, "network interface {name} not available"),
        }
    }
}

impl std::error::Error for NetcvError {}

fn delivery_systems(fe_type: i32) -> Vec<DeliverySystem> {
    match fe_type {
        FE_DVBS2 => vec![DeliverySystem::DvbS2, DeliverySystem::DvbS],
        FE_QPSK => vec![DeliverySystem::DvbS],
        FE_QAM => vec![DeliverySystem::DvbC],
        FE_OFDM => vec![DeliverySystem::DvbT],
        _ => Vec::new(),
    }
}

/// `find_netcv_adapter` (netceiver.cpp:398-548); `None` config means `-n` was not given.
pub fn find_netcv_adapters(
    api: &mut impl McliDiscoveryApi,
    cfg: Option<&NetcvConfig>,
    a_count: usize,
    max_adapters: usize,
    is_disabled: impl Fn(usize) -> bool,
) -> Result<NetcvDiscovery, NetcvError> {
    let mut out = NetcvDiscovery {
        a_count,
        ..NetcvDiscovery::default()
    };
    let Some(cfg) = cfg else { return Ok(out) };
    if !api.interface_ready(&cfg.ifname) {
        return Err(NetcvError::InterfaceUnavailable(cfg.ifname.clone()));
    }
    // Failures are only logged in C++.
    let _ = api.recv_init(&cfg.ifname, MCLI_PORT);
    let _ = api.api_sock_init(API_SOCK_NAMESPACE);

    let mut n = 0;
    let list = loop {
        api.sleep_ms(250);
        let list = api.netceivers();
        let more = list.len() < cfg.count && n < 19;
        n += 1;
        if !more {
            break list;
        }
    };
    api.lock_list();

    let mut counts = [0i32; 5];
    let mut total = 0;
    for tuner in list.iter().flat_map(|nc| &nc.tuners) {
        // C++ indexes nc_sys_c with any type; values outside 0..=4 are skipped here.
        if let Some(c) = usize::try_from(tuner.fe_type)
            .ok()
            .and_then(|t| counts.get_mut(t))
        {
            *c += 1;
            total += 1;
        }
    }

    let mut i = 0;
    let mut na = a_count;
    for _ in 0..total {
        while i < FE_DVBS2 as usize && counts[MAP_TYPE[i] as usize] == 0 {
            i += 1;
        }
        counts[MAP_TYPE[i] as usize] -= 1;
        if is_disabled(na) {
            na += 1;
            continue;
        }
        if na >= max_adapters {
            break;
        }
        out.adapters.push(NetcvAdapter {
            id: na,
            fe_type: MAP_TYPE[i],
            sys: delivery_systems(MAP_TYPE[i]),
        });
        na += 1;
        out.a_count = na;
    }
    api.unlock_list();
    out.netceivers = list;
    Ok(out)
}
