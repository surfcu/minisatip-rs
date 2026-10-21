//! Enigma2 hardware descrambler (`--hw-descrambler`, hw_descrambler.cpp).
//!
//! Control words go to the box's CA device with `CA_SET_DESCR_MODE`, `CA_SET_PID`,
//! `CA_SET_DESCR` and `CA_SET_DESCR_DATA`; one descrambler slot is allocated per PMT and
//! physical adapter. The ioctls sit behind [`CaDevice`], with [`fake::RecordingCaDevice`]
//! for tests and [`LinuxCaDevice`] on Linux.

mod device;
pub mod fake;
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
mod sys;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};
use std::io;

pub use device::*;
#[cfg(target_os = "linux")]
pub use sys::LinuxCaDevice;

use crate::algo::{Algo, AlgoOp, AlgoRegistry, Backend};

/// Slots assumed when `CA_GET_DESCR_INFO` is unavailable.
pub const DEFAULT_MAX_DESCRAMBLERS: i32 = 16;
/// Most slots tracked per physical adapter.
pub const MAX_SLOTS: usize = 64;
/// `MAX_ADAPTERS` (adapter.h); physical adapter numbers outside it are ignored.
pub const MAX_ADAPTERS: i32 = 100;
/// `TABLES_RESULT_OK` returned by the CA hooks.
pub const TABLES_RESULT_OK: i32 = 0;

/// Fields of a logical adapter the descrambler reads (`adapter`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HwAdapter {
    /// Logical adapter id.
    pub id: usize,
    /// Physical adapter number (`ad->pa`).
    pub pa: i32,
    /// Frontend number (`ad->fn`), which names the `caN` node.
    pub fe: i32,
    /// `ad->enabled`.
    pub enabled: bool,
}

/// Fields of a PMT the descrambler reads (`SPMT`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HwPmt {
    /// PMT slot id.
    pub id: usize,
    /// Logical adapter, `None` for C++ `adapter < 0`.
    pub adapter: Option<usize>,
    /// `pmt->enabled`.
    pub enabled: bool,
    /// Elementary PIDs in PMT order (`stream_pids`).
    pub stream_pids: Vec<u16>,
}

/// The adapter and PMT tables (C++ `a[]` and `pmts`).
#[derive(Debug, Clone, Default)]
pub struct HwRegistry {
    /// Adapters by logical id.
    pub adapters: BTreeMap<usize, HwAdapter>,
    /// PMTs by id.
    pub pmts: BTreeMap<usize, HwPmt>,
}

impl HwRegistry {
    /// `get_adapter`: the adapter if present and enabled.
    pub fn adapter(&self, id: usize) -> Option<&HwAdapter> {
        self.adapters.get(&id).filter(|ad| ad.enabled)
    }
}

/// Hardware key state hung off a control word (`HwKey`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HwKey {
    /// CA device the key was programmed on.
    pub ca_fd: Option<CaFd>,
    /// Physical adapter, -1 until programmed.
    pub adapter_id: i32,
    /// PMT id, `None` until programmed.
    pub pmt_id: Option<usize>,
    /// Descrambler slot, -1 until allocated.
    pub slot_index: i32,
    /// Algorithm at creation.
    pub algo: Algo,
    /// Slots of the device.
    pub num_descramblers: i32,
}

/// A control word (`SCW`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HwCw {
    /// Control word id.
    pub id: i32,
    /// Algorithm.
    pub algo: Algo,
    /// 0 = even, 1 = odd.
    pub parity: u8,
    /// Key bytes (8 used for CSA, 16 otherwise).
    pub cw: [u8; 16],
    /// AES-CBC IV.
    pub iv: [u8; 16],
    /// Hardware key, set by [`HwDescrambler::create_key`].
    pub key: Option<HwKey>,
}

#[derive(Debug, Clone)]
struct AdapterCaState {
    ca_fd: Option<CaFd>,
    num_descramblers: i32,
    pmt_slots: [Option<usize>; MAX_SLOTS],
    bound_pids: BTreeMap<usize, BTreeSet<u16>>,
}

impl Default for AdapterCaState {
    fn default() -> Self {
        Self {
            ca_fd: None,
            num_descramblers: DEFAULT_MAX_DESCRAMBLERS,
            pmt_slots: [None; MAX_SLOTS],
            bound_pids: BTreeMap::new(),
        }
    }
}

impl AdapterCaState {
    fn max_desc(&self) -> usize {
        (self.num_descramblers.max(0) as usize).min(MAX_SLOTS)
    }
}

/// `/dev/dvb/adapter<pa>/ca<fn>`.
pub fn ca_device_path(ad: &HwAdapter) -> String {
    format!("/dev/dvb/adapter{}/ca{}", ad.pa, ad.fe)
}

/// Slot manager and CA hooks of the hardware descrambler (`HwSlotManager` plus `hw_*`).
#[derive(Debug)]
pub struct HwDescrambler<D: CaDevice> {
    enabled: bool,
    dev: D,
    adapters: BTreeMap<i32, AdapterCaState>,
}

impl<D: CaDevice> HwDescrambler<D> {
    /// `enabled` is `opts.hw_descrambler`.
    pub fn new(enabled: bool, dev: D) -> Self {
        Self {
            enabled,
            dev,
            adapters: BTreeMap::new(),
        }
    }

    /// `opts.hw_descrambler`.
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Toggles `opts.hw_descrambler`.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// The wrapped device.
    pub fn device(&self) -> &D {
        &self.dev
    }

    /// Mutable access to the wrapped device.
    pub fn device_mut(&mut self) -> &mut D {
        &mut self.dev
    }

    /// `init_hw_descrambler`: registers the hardware ops when enabled; `true` if the CA
    /// hooks (`add_ca(&hw_ca_op)`) are active.
    pub fn register(&self, registry: &mut AlgoRegistry) -> bool {
        if !self.enabled {
            return false;
        }
        for algo in [Algo::Csa, Algo::Aes128Ecb, Algo::Aes128Cbc] {
            registry.register(AlgoOp {
                algo,
                backend: Backend::Hardware,
            });
        }
        true
    }

    /// `hw_create_key`.
    pub fn create_key(cw: &mut HwCw) {
        cw.key = Some(HwKey {
            ca_fd: None,
            adapter_id: -1,
            pmt_id: None,
            slot_index: -1,
            algo: cw.algo,
            num_descramblers: DEFAULT_MAX_DESCRAMBLERS,
        });
    }

    /// `hw_delete_key`.
    pub fn delete_key(cw: &mut HwCw) {
        cw.key = None;
    }

    fn state(&mut self, pa: i32) -> Option<&mut AdapterCaState> {
        if !(0..MAX_ADAPTERS).contains(&pa) {
            return None;
        }
        Some(self.adapters.entry(pa).or_default())
    }

    /// `get_or_open_ca_fd`: opens the node (or `/dev/ci<pa>` on `ENOENT`) once per adapter.
    fn get_or_open_ca_fd(&mut self, pa: i32, path: &str) -> Option<CaFd> {
        if !(0..MAX_ADAPTERS).contains(&pa) {
            return None;
        }
        if let Some(fd) = self.adapters.get(&pa).and_then(|ca| ca.ca_fd) {
            return Some(fd);
        }
        let mut opened = self.dev.open(path);
        if matches!(&opened, Err(e) if e.kind() == io::ErrorKind::NotFound) {
            opened = self.dev.open(&format!("/dev/ci{pa}"));
        }
        let fd = opened.ok()?;
        let num = match self.dev.get_descr_info(fd) {
            Ok(info) => i32::try_from(info.num).unwrap_or(-1),
            Err(_) => DEFAULT_MAX_DESCRAMBLERS,
        };
        let ca = self.state(pa)?;
        ca.ca_fd = Some(fd);
        ca.num_descramblers = if num > 0 {
            num
        } else {
            DEFAULT_MAX_DESCRAMBLERS
        };
        Some(fd)
    }

    fn max_slots(&mut self, pa: i32) -> i32 {
        match self.state(pa) {
            Some(ca) if ca.num_descramblers > 0 => ca.num_descramblers,
            _ => DEFAULT_MAX_DESCRAMBLERS,
        }
    }

    fn allocate_slot(&mut self, pa: i32, pmt_id: usize) -> Option<usize> {
        let ca = self.state(pa)?;
        if ca.num_descramblers <= 0 {
            ca.num_descramblers = DEFAULT_MAX_DESCRAMBLERS;
        }
        let max = ca.max_desc();
        if let Some(i) = ca.pmt_slots[..max].iter().position(|s| *s == Some(pmt_id)) {
            return Some(i);
        }
        let free = ca.pmt_slots[..max].iter().position(Option::is_none)?;
        ca.pmt_slots[free] = Some(pmt_id);
        Some(free)
    }

    fn release_slot(&mut self, pa: i32, pmt_id: usize) {
        let Some(ca) = self.state(pa) else { return };
        let max = ca.max_desc();
        if let Some(slot) = ca.pmt_slots[..max].iter_mut().find(|s| **s == Some(pmt_id)) {
            *slot = None;
        }
    }

    /// Descrambler slot currently held by `pmt_id` on physical adapter `pa`.
    pub fn slot_of(&self, pa: i32, pmt_id: usize) -> Option<usize> {
        let ca = self.adapters.get(&pa)?;
        ca.pmt_slots[..ca.max_desc()]
            .iter()
            .position(|s| *s == Some(pmt_id))
    }

    /// `hw_bound_pids_for_test`: PIDs tracked as bound for a PMT.
    pub fn bound_pids(&self, pa: i32, pmt_id: usize) -> BTreeSet<u16> {
        self.adapters
            .get(&pa)
            .and_then(|ca| ca.bound_pids.get(&pmt_id))
            .cloned()
            .unwrap_or_default()
    }

    /// `hw_set_cw`: programs mode, PIDs and key of `cw` into the PMT's slot.
    pub fn set_cw(&mut self, cw: &mut HwCw, pmt: &HwPmt, reg: &HwRegistry) {
        if !self.enabled || cw.key.is_none() || cw.algo == Algo::CsaIcam {
            return;
        }
        let Some(ad) = pmt.adapter.and_then(|id| reg.adapter(id)).copied() else {
            return;
        };
        let path = ca_device_path(&ad);
        let Some(fd) = self.get_or_open_ca_fd(ad.pa, &path) else {
            return;
        };
        let num_descramblers = self.max_slots(ad.pa);
        let slot = self.allocate_slot(ad.pa, pmt.id);
        let Some(key) = cw.key.as_mut() else { return };
        key.ca_fd = Some(fd);
        key.adapter_id = ad.pa;
        key.pmt_id = Some(pmt.id);
        key.num_descramblers = num_descramblers;
        key.slot_index = slot.map_or(-1, |s| s as i32);
        let Some(slot) = slot else { return };
        let index = slot as u32;

        // Results are only logged in C++; a failing ioctl does not stop the sequence.
        let _ = self.dev.set_descr_mode(
            fd,
            &CaDescrMode {
                index,
                algo: cw.algo.raw(),
                cipher_mode: u32::from(cw.algo == Algo::Aes128Cbc),
            },
        );
        for &pid in &pmt.stream_pids {
            let _ = self.dev.set_pid(
                fd,
                &CaPid {
                    pid: u32::from(pid),
                    index: slot as i32,
                },
            );
        }
        let parity = u32::from(cw.parity);
        if cw.algo == Algo::Csa {
            let mut word = [0u8; 8];
            word.copy_from_slice(&cw.cw[..8]);
            let _ = self.dev.set_descr(
                fd,
                &CaDescr {
                    index,
                    parity,
                    cw: word,
                },
            );
            return;
        }
        let mut data = CaDescrData {
            index,
            parity,
            data_type: DATA_TYPE_CW,
            data_len: 16,
            data: cw.cw,
        };
        let _ = self.dev.set_descr_data(fd, &data);
        if cw.algo == Algo::Aes128Cbc {
            data.data_type = DATA_TYPE_IV;
            data.data = cw.iv;
            let _ = self.dev.set_descr_data(fd, &data);
        }
    }

    /// `hw_drop_held_pids`: keeps PIDs another enabled PMT on the same hardware still lists.
    fn drop_held_pids(ad: &HwAdapter, pmt: &HwPmt, reg: &HwRegistry, pids: &mut BTreeSet<u16>) {
        pids.retain(|pid| {
            !reg.pmts.values().any(|o| {
                o.enabled
                    && o.id != pmt.id
                    && o.adapter
                        .and_then(|id| reg.adapter(id))
                        .is_some_and(|oa| oa.pa == ad.pa)
                    && o.stream_pids.contains(pid)
            })
        });
    }

    fn unbind_pids(&mut self, pa: i32, path: &str, pids: &BTreeSet<u16>) {
        if pids.is_empty() {
            return;
        }
        let Some(fd) = self.get_or_open_ca_fd(pa, path) else {
            return;
        };
        for &pid in pids {
            let _ = self.dev.set_pid(
                fd,
                &CaPid {
                    pid: u32::from(pid),
                    index: -1,
                },
            );
        }
    }

    /// `hw_ca_add_pmt`: retargets the bound set; PIDs the update dropped are unbound.
    pub fn add_pmt(&mut self, ad: &HwAdapter, pmt: &HwPmt, _update: bool, reg: &HwRegistry) -> i32 {
        if !self.enabled {
            return TABLES_RESULT_OK;
        }
        let current: BTreeSet<u16> = pmt.stream_pids.iter().copied().collect();
        let mut removed = match self.state(ad.pa) {
            Some(ca) => {
                let old = ca.bound_pids.insert(pmt.id, current.clone());
                old.unwrap_or_default()
                    .difference(&current)
                    .copied()
                    .collect()
            }
            None => BTreeSet::new(),
        };
        Self::drop_held_pids(ad, pmt, reg, &mut removed);
        self.unbind_pids(ad.pa, &ca_device_path(ad), &removed);
        TABLES_RESULT_OK
    }

    /// `hw_ca_del_pmt`: unbinds every PID ever bound plus the live list, then frees the slot.
    pub fn del_pmt(&mut self, ad: &HwAdapter, pmt: &HwPmt, reg: &HwRegistry) -> i32 {
        if !self.enabled {
            return 0;
        }
        let mut bound = self
            .state(ad.pa)
            .and_then(|ca| ca.bound_pids.remove(&pmt.id))
            .unwrap_or_default();
        bound.extend(pmt.stream_pids.iter().copied());
        Self::drop_held_pids(ad, pmt, reg, &mut bound);
        self.unbind_pids(ad.pa, &ca_device_path(ad), &bound);
        self.release_slot(ad.pa, pmt.id);
        0
    }

    /// `hw_ca_init_dev`.
    pub fn init_dev(&mut self, _ad: &HwAdapter) -> i32 {
        TABLES_RESULT_OK
    }

    /// `hw_ca_close_dev`: closes the shared fd and forgets slots and bound PIDs.
    pub fn close_dev(&mut self, ad: &HwAdapter) -> i32 {
        if let Some(ca) = self.state(ad.pa) {
            let fd = ca.ca_fd.take();
            ca.pmt_slots = [None; MAX_SLOTS];
            ca.bound_pids.clear();
            if let Some(fd) = fd {
                self.dev.close(fd);
            }
        }
        0
    }
}
