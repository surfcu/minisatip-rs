//! DDCI driver (ddci.cpp): programs from tuners are remapped onto one CI adapter,
//! which gets a generated PAT/SDT/PMT/EIT; the CAM output is mapped back.

pub mod fifo;
pub mod psi;

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::Path;

use ms_ca::capmt::MAX_CA_PMT;
use ms_ca::{Program, ProgramTable, TablesResult};
use ms_ts::crc::crc32_mpeg2;

use fifo::Fifo;
pub use psi::{LocalTime, buffer_to_ts, pid_from_ts, set_pid_ts};
use psi::{dvb_put_string, ymd_to_mjd};

/// `#define DEFAULT_LOG LOG_DDCI` (ddci.cpp:54).
const DEFAULT_LOG: u32 = ms_log::LOG_DDCI;

pub const MAX_CHANNELS_ON_CI: usize = 8;
pub const DDCI_BUFFER: usize = 20000 * 188;
pub const MAX_ADAPTERS: usize = 100;
pub const DVB_FRAME: usize = 188;
/// `DDCI_SID` (`MAX_STREAMS + 1`): the stream id DDCI marks its pids with.
pub const DDCI_SID: i32 = 257; // MAX_STREAMS + 1 (stream.h:13)
pub const PID_STREAM_ID_UNDEFINED: i32 = -1;

/// One channel slot of a DDCI device (`ddci_pmt_t`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DdciPmt {
    pub id: i32,
    pub ver: i32,
    pub pcr_pid: i32,
    pub cc: i16,
    pub crc: u32,
    /// Emission clock (ms); 0 emits on the next tick.
    pub last_pmt: u64,
}

impl DdciPmt {
    /// The `memset(-1)` state of an unused slot.
    pub fn unused() -> Self {
        Self {
            id: -1,
            ver: -1,
            pcr_pid: -1,
            cc: -1,
            crc: u32::MAX,
            last_pmt: u64::MAX,
        }
    }
}

/// One mapping entry (`ddci_mapping_table_t`), keyed by `(adapter, pid)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mapping {
    pub ad: u16,
    pub pid: u16,
    pub ddci_pid: i32,
    pub ddci: i32,
    pub rewrite: bool,
    pub pmt: BTreeSet<i32>,
    pub filter_id: Option<usize>,
    /// Set when DDCI added the source pid itself (`pid_added`).
    pub pid_added: bool,
}

/// One DDCI device (`ddci_device_t`).
#[derive(Clone, Debug)]
pub struct DdciDevice {
    pub enabled: bool,
    pub id: i32,
    pub channels: i32,
    pub max_channels: usize,
    pub pmt: [DdciPmt; MAX_CHANNELS_ON_CI + 1],
    pub cat_processed: bool,
    pub read_index: Vec<u64>,
    pub last_pat: u64,
    pub last_sdt: u64,
    pub psi_changed: bool,
    pub tid: u16,
    pub ver: i32,
    pub sdt_ver: i32,
    pub sdt_data_mask: u32,
    pub pat_cc: i16,
    pub sdt_cc: i16,
    pub eit_cc: i16,
    pub disable_cat: bool,
    pub mapping: BTreeMap<(u16, u16), Mapping>,
    /// CAT-listed EMM pids, never swept by updates.
    pub emm_pids: BTreeSet<(u16, u16)>,
    pub fifo: Fifo,
}

impl DdciDevice {
    /// A zero-initialised enabled device (`ddci_device_t d = {}` plus `enabled = 1`).
    pub fn new(id: i32, max_channels: usize) -> Self {
        Self {
            enabled: true,
            id,
            channels: 0,
            max_channels: max_channels.min(MAX_CHANNELS_ON_CI),
            pmt: [DdciPmt::default(); MAX_CHANNELS_ON_CI + 1],
            cat_processed: false,
            read_index: vec![0; MAX_ADAPTERS],
            last_pat: 0,
            last_sdt: 0,
            psi_changed: false,
            tid: 0,
            ver: 0,
            sdt_ver: 0,
            sdt_data_mask: 0,
            pat_cc: 0,
            sdt_cc: 0,
            eit_cc: 0,
            disable_cat: false,
            mapping: BTreeMap::new(),
            emm_pids: BTreeSet::new(),
            fifo: Fifo::new(DDCI_BUFFER),
        }
    }

    /// `memset(&d->pmt, -1)`: every slot unused.
    pub fn clear_slots(&mut self) {
        self.pmt = [DdciPmt::unused(); MAX_CHANNELS_ON_CI + 1];
    }

    /// State after `ddci_open_device`.
    pub fn opened(id: i32) -> Self {
        let mut d = Self::new(id, MAX_CHANNELS_ON_CI);
        d.clear_slots();
        d
    }

    fn get_ddci_pid(&self, dpid: i32) -> Option<&Mapping> {
        self.mapping.values().find(|m| m.ddci_pid == dpid)
    }

    /// `find_ddci_pid`: the first DDCI-side pid from `pid` up that is free.
    fn find_ddci_pid(&self, pid: u16) -> i32 {
        if pid > 8191 {
            ms_log::log_and_return!(-1, "pid {} invalid", pid);
        }
        (i32::from(pid)..8191)
            .find(|&p| self.get_ddci_pid(p).is_none())
            .unwrap_or(-1)
    }

    fn slots(&self) -> std::ops::Range<usize> {
        0..self.max_channels.min(MAX_CHANNELS_ON_CI + 1)
    }

    fn bump_versions(&mut self) {
        self.ver = (self.ver + 1) & 0xF;
        self.sdt_ver = (self.sdt_ver + 1) & 0xF;
    }
}

/// A DDCI channel entry (`Sddci_channel`): candidate devices for a sid.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Channel {
    pub ddci: Vec<u8>,
    pub sid: u16,
    pub name: String,
    pub locked: bool,
}

/// The CA view DDCI reads (`ca_devices[i]`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CamInfo {
    pub enabled: bool,
    pub initialized: bool,
    pub multiple_pmt: usize,
    pub max_ca_pmt: usize,
}

/// Adapter pid entry (`SPid`): stream ids and the deleted flag.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PidEntry {
    pub sids: BTreeSet<i32>,
    pub deleted: bool,
}

/// Per-adapter state DDCI touches.
#[derive(Clone, Debug, Default)]
pub struct AdapterState {
    pub transponder_id: u16,
    pub pids: BTreeMap<u16, PidEntry>,
    /// The adapter's real pid table (`find_pid` in C++), mirrored by the
    /// runtime: pid -> owned by at least one stream (`!p->sid.empty()`).
    pub client_pids: BTreeMap<u16, bool>,
}

/// TS buffer of an adapter (`ad->buf`, `ad->rlen`, `ad->lbuf = buf.len()`).
#[derive(Clone, Debug)]
pub struct TsBuffer {
    pub id: u16,
    pub buf: Vec<u8>,
    pub rlen: usize,
}

/// Write side of the CI device (`writev` on `ad2->fe`).
pub trait CiWriter {
    fn writev(&mut self, iov: &[&[u8]]) -> isize;

    /// `errno` left by the last failed `writev`, for the incomplete-write log.
    fn last_errno(&self) -> i32 {
        0
    }

    /// `strerror(errno)` of the last failed `writev`.
    fn last_error(&self) -> &str {
        ""
    }

    /// `ad->fe` of the CI adapter.
    fn fd(&self) -> i32 {
        -1
    }

    /// `ad->fe_sock` of the CI adapter (always -1 in `ddci_open_device`).
    fn socket(&self) -> i32 {
        -1
    }
}

/// All DDCI devices plus the global state ddci.cpp shares with the rest of C++.
pub struct Ddci {
    /// `ddci_devices[]`, by adapter index.
    pub devices: BTreeMap<usize, DdciDevice>,
    pub channels: BTreeMap<u16, Channel>,
    /// `ca_devices[]` by adapter index.
    pub cams: BTreeMap<usize, CamInfo>,
    /// `ca[dvbca_id].ad_info[i]` CAID/mask pairs.
    pub caid_masks: BTreeMap<usize, Vec<(u16, u16)>>,
    /// Adapters that exist (`a[i]`), with their pid tables.
    pub adapters: BTreeMap<u16, AdapterState>,
    /// CAT section filters: id → (adapter, device index).
    pub filters: BTreeMap<usize, (u16, usize)>,
    next_filter: usize,
    /// `getTick()` in ms.
    pub now_ms: u64,
    /// Local time one hour ago, for the EIT start time.
    pub eit_start: LocalTime,
}

impl Default for Ddci {
    fn default() -> Self {
        Self::new()
    }
}

impl Ddci {
    pub fn new() -> Self {
        Self {
            devices: BTreeMap::new(),
            channels: BTreeMap::new(),
            cams: BTreeMap::new(),
            caid_masks: BTreeMap::new(),
            adapters: BTreeMap::new(),
            filters: BTreeMap::new(),
            next_filter: 0,
            now_ms: 0,
            eit_start: LocalTime::default(),
        }
    }

    pub fn create_adapter(&mut self, id: u16) {
        self.adapters.entry(id).or_default();
    }

    /// `set_ca_channels`: CAPMT count and programs per CAPMT of each `-c` CA.
    pub fn set_ca_channels(&mut self, channels: &[ms_config::CaChannels]) {
        for c in channels {
            let cam = self.cams.entry(usize::from(c.adapter)).or_default();
            cam.multiple_pmt = usize::try_from(c.multiple_pmt).unwrap_or(0);
            // C++ keeps a negative count from malformed input; 0 sends no CAPMT either.
            cam.max_ca_pmt = usize::try_from(c.max_ca_pmt).unwrap_or(0);
        }
    }

    /// `add_caid_mask(dvbca_id, i, caid, mask)`.
    pub fn add_caid_mask(&mut self, i: usize, caid: u16, mask: u16) {
        self.caid_masks.entry(i).or_default().push((caid, mask));
    }

    fn get_ddci(&self, i: i32) -> Option<&DdciDevice> {
        let i = usize::try_from(i).ok()?;
        self.devices.get(&i).filter(|d| d.enabled)
    }

    /// `is_ca_initializing`.
    pub fn is_ca_initializing(&self, i: usize) -> bool {
        self.cams
            .get(&i)
            .is_some_and(|c| c.enabled && !c.initialized)
    }

    /// `get_max_pmt_for_ca`.
    pub fn max_pmt_for_ca(&self, i: usize) -> usize {
        match self.cams.get(&i) {
            Some(c) if c.enabled => (c.multiple_pmt + 1) * c.max_ca_pmt,
            _ => MAX_CA_PMT,
        }
    }

    fn ca_multiple_pmt(&self, i: i32) -> usize {
        usize::try_from(i)
            .ok()
            .and_then(|i| self.cams.get(&i))
            .filter(|c| c.enabled)
            .map_or(0, |c| c.multiple_pmt)
    }

    // ---- adapter pid table (adapter.cpp) ----

    /// `post_tune` on a CI adapter: pid 0 for the generated PAT.
    pub fn post_tune_ci(&mut self, ad: u16) {
        if self.find_pid(ad, 0).is_none_or(|p| p.deleted) {
            self.mark_pid_add(PID_STREAM_ID_UNDEFINED, ad, 0);
        }
    }

    /// `find_pid`.
    pub fn find_pid(&self, ad: u16, pid: u16) -> Option<&PidEntry> {
        self.adapters.get(&ad)?.pids.get(&pid)
    }

    /// `mark_pid_add`.
    pub fn mark_pid_add(&mut self, sid: i32, ad: u16, pid: u16) {
        let Some(a) = self.adapters.get_mut(&ad) else {
            return;
        };
        let p = a.pids.entry(pid).or_default();
        if sid != PID_STREAM_ID_UNDEFINED {
            p.sids.insert(sid);
        }
        p.deleted = false;
    }

    /// `mark_pid_deleted` (no filter keep-alive).
    pub fn mark_pid_deleted(&mut self, ad: u16, sid: i32, pid: u16) {
        let Some(p) = self
            .adapters
            .get_mut(&ad)
            .and_then(|a| a.pids.get_mut(&pid))
        else {
            return;
        };
        if sid == PID_STREAM_ID_UNDEFINED {
            p.sids.clear();
            p.deleted = true;
            return;
        }
        p.sids.remove(&sid);
        if p.sids.is_empty() {
            p.deleted = true;
        }
    }

    // ---- mapping table ----

    /// `add_pid_mapping_table`: the DDCI-side pid, or -1.
    pub fn add_pid_mapping_table(
        &mut self,
        ad: u16,
        pid: u16,
        pmt: i32,
        dev: usize,
        force_add_pid: bool,
    ) -> i32 {
        let Some(d) = self.devices.get_mut(&dev) else {
            return -1;
        };
        if !d.mapping.contains_key(&(ad, pid)) {
            let ddci_pid = d.find_ddci_pid(pid);
            let id = d.id;
            d.mapping.insert(
                (ad, pid),
                Mapping {
                    ad,
                    pid,
                    ddci_pid,
                    ddci: id,
                    rewrite: true,
                    pmt: BTreeSet::new(),
                    filter_id: None,
                    pid_added: false,
                },
            );
        }
        let d_id = d.id;
        let m = d.mapping.get_mut(&(ad, pid)).expect("just inserted");
        let ddci_pid = m.ddci_pid;
        if ddci_pid == -1 {
            ms_log::log_and_return!(
                -1,
                "could not add pid {} and ad {} to the mapping table",
                pid,
                ad
            );
        }
        if m.pmt.contains(&pmt) {
            // Already mapped: re-sends keep the stored PCR pid.
            ms_log::logm!(
                "Adapter {} pid {} already mapped to pmt {}, already added {}",
                ad,
                pid,
                pmt,
                u8::from(m.pid_added)
            );
            return ddci_pid;
        }
        let add_pid = m.pmt.is_empty();
        m.pmt.insert(pmt);
        if add_pid && force_add_pid {
            m.pid_added = true;
            if pid == 1 {
                let f = self.next_filter;
                self.next_filter += 1;
                self.filters.insert(f, (ad, dev));
                self.devices
                    .get_mut(&dev)
                    .unwrap()
                    .mapping
                    .get_mut(&(ad, pid))
                    .unwrap()
                    .filter_id = Some(f);
            }
            self.mark_pid_add(DDCI_SID, ad, pid);
        }
        if add_pid {
            self.mark_pid_add(DDCI_SID, d_id as u16, ddci_pid as u16);
        }
        // `m->ad`/`m->pid` are the entry's own (ad, pid).
        ms_log::log!(
            "mapped adapter {} ({}) pid {} ({}) to {}, pmt {}",
            ad,
            ad,
            pid,
            pid,
            ddci_pid,
            pmt
        );
        ddci_pid
    }

    /// `dump_mapping_table`: the debug dump of every enabled device's mappings.
    fn dump_mapping_table(&self) {
        // The C++ builds the per-entry PMT list only when the mask is on.
        if !ms_log::enabled(DEFAULT_LOG) {
            return;
        }
        ms_log::logm!("Mapping Table for all devices");
        for d in self.devices.values().filter(|d| d.enabled) {
            for m in d.mapping.values() {
                let mut out = String::new();
                for elem in &m.pmt {
                    out.push_str(&format!("{elem} "));
                }
                ms_log::logm!(
                    "DD {}, ddpid {}, adapter {} pid {}, rewrite {}: {} PMTS {}",
                    m.ddci,
                    m.ddci_pid,
                    m.ad,
                    m.pid,
                    u8::from(m.rewrite),
                    m.pmt.len(),
                    out
                );
            }
        }
    }

    /// `get_pid_mapping_allddci`.
    pub fn get_pid_mapping_allddci(&self, ad: u16, pid: u16) -> Option<&Mapping> {
        self.devices
            .values()
            .filter(|d| d.enabled)
            .find_map(|d| d.mapping.get(&(ad, pid)))
    }

    fn release_mapping_entry(&mut self, dev_id: i32, ad: u16, pmt: i32, m: &Mapping) {
        let pid = m.pid;
        let ddci_pid = m.ddci_pid;
        ms_log::log!(
            "Deleting ad {} pmt {} pid {} ddci_pid {}",
            ad,
            pmt,
            pid,
            ddci_pid
        );
        if self.find_pid(dev_id as u16, ddci_pid as u16).is_some() {
            self.mark_pid_deleted(dev_id as u16, DDCI_SID, ddci_pid as u16);
        }
        if let Some(f) = m.filter_id {
            self.filters.remove(&f);
        }
        if m.pid_added {
            ms_log::logm!(
                "Marking pid {} deleted on adapter {} (initial ad {} pid {})",
                ddci_pid,
                dev_id,
                ad,
                pid
            );
            self.mark_pid_deleted(ad, DDCI_SID, pid);
        }
    }

    /// `del_pmt_mapping_table`.
    pub fn del_pmt_mapping_table(&mut self, dev: usize, ad: u16, pmt: i32) {
        let Some(d) = self.devices.get_mut(&dev) else {
            return;
        };
        let dev_id = d.id;
        let mut released = Vec::new();
        for m in d.mapping.values_mut() {
            if m.ad != ad {
                continue;
            }
            m.pmt.remove(&pmt);
            if m.pmt.is_empty() {
                released.push(m.clone());
            }
        }
        for m in &released {
            d.mapping.remove(&(ad, m.pid));
        }
        for m in released {
            self.release_mapping_entry(dev_id, ad, pmt, &m);
        }
    }

    /// `unmap_removed_pids`: unmap the pids an update dropped.
    fn unmap_removed_pids(&mut self, dev: usize, ad: u16, pmt: &Program) -> usize {
        let d = self.devices.get_mut(&dev).expect("device");
        let dev_id = d.id;
        let emm = d.emm_pids.clone();
        let still_needed = |pid: u16| {
            pid == pmt.pid
                || pid == 1
                || pid == 20
                || emm.contains(&(ad, pid))
                || pmt.streams.iter().any(|s| s.pid == pid)
                || pmt.caids.iter().any(|c| c.pid == pid)
        };
        let mut released = Vec::new();
        for m in d.mapping.values_mut() {
            if m.ad != ad || !m.pmt.contains(&pmt.id) || still_needed(m.pid) {
                continue;
            }
            m.pmt.remove(&pmt.id);
            if m.pmt.is_empty() {
                released.push(m.clone());
            }
        }
        for m in &released {
            d.mapping.remove(&(ad, m.pid));
        }
        let n = released.len();
        for m in released {
            self.release_mapping_entry(dev_id, ad, pmt.id, &m);
        }
        n
    }

    // ---- channel selection ----

    /// `create_channel_for_pmt`: ready DDCIs whose CAIDs match; initializing ones force a retry.
    fn create_channel_for_pmt(&self, pmt: &Program) -> Result<Channel, TablesResult> {
        let mut c = Channel::default();
        let mut saw_initializing = false;
        for (&i, _) in self.devices.iter().filter(|(_, d)| d.enabled) {
            if self.is_ca_initializing(i) {
                saw_initializing = true;
                continue;
            }
            for &(caid, mask) in self.caid_masks.get(&i).map_or(&[][..], Vec::as_slice) {
                if pmt.match_caid(caid, mask) {
                    ms_log::log!(
                        "DDCI {} CAID {:04X} and mask {:04X} matched PMT {}",
                        i,
                        caid,
                        mask,
                        pmt.id
                    );
                    c.ddci.push(i as u8);
                    c.sid = pmt.sid;
                    c.name = pmt.name.chars().take(49).collect();
                }
            }
        }
        if c.ddci.is_empty() && saw_initializing {
            return Err(TablesResult::Retry);
        }
        Ok(c)
    }

    /// `find_ddci_for_pmt`: the least used ready candidate, the first on a tie.
    fn find_ddci_for_pmt(&self, c: &Channel, pmt: &Program) -> Result<i32, TablesResult> {
        let mut best: Option<(i32, i32)> = None;
        let (mut saw_full, mut saw_initializing) = (false, false);
        for &cand in &c.ddci {
            let Some(d) = self.get_ddci(i32::from(cand)) else {
                ms_log::log!("find_ddci_for_pmt: DDCI {} not enabled, skipping", cand);
                continue;
            };
            if d.channels >= d.max_channels as i32 {
                ms_log::log!(
                    "find_ddci_for_pmt: DDCI {} cannot be used for PMT {}, pid {}, sid, {} (used channels {} max {}), skipping",
                    cand,
                    pmt.id,
                    pmt.pid,
                    pmt.sid,
                    d.channels,
                    d.max_channels
                );
                saw_full = true;
                continue;
            }
            if self.is_ca_initializing(usize::from(cand)) {
                ms_log::log!("find_ddci_for_pmt: DDCI {} is initializing, skipping", cand);
                saw_initializing = true;
                continue;
            }
            if best.is_none_or(|(_, n)| d.channels < n) {
                best = Some((i32::from(cand), d.channels));
            }
        }
        match best {
            Some((id, channels)) => {
                ms_log::log!(
                    "find_ddci_for_pmt: Using DDCI {}, running {} channels",
                    id,
                    channels
                );
                Ok(id)
            }
            None => {
                // The C++ returns on the initializing case before checking full.
                if saw_initializing {
                    ms_log::log!(
                        "find_ddci_for_pmt: DDCI for PMT {} (sid {}) is initializing, retrying",
                        pmt.id,
                        pmt.sid
                    );
                    return Err(TablesResult::Retry);
                }
                if saw_full {
                    ms_log::log!(
                        "find_ddci_for_pmt: all DDCIs full for PMT {} (sid {}), retrying",
                        pmt.id,
                        pmt.sid
                    );
                    return Err(TablesResult::Retry);
                }
                if c.ddci.is_empty() {
                    ms_log::log!(
                        "find_ddci_for_pmt: no suitable DDCI found for PMT {} (sid {}): not mapped to any DDCI device in ddci.conf",
                        pmt.id,
                        pmt.sid
                    );
                } else {
                    ms_log::log!(
                        "find_ddci_for_pmt: no suitable DDCI found for PMT {} (sid {})",
                        pmt.id,
                        pmt.sid
                    );
                }
                Err(TablesResult::NoRetry)
            }
        }
    }

    /// `is_pmt_running`: the DDCI mapping the first stream pid.
    fn is_pmt_running(&self, pmt: &Program) -> i32 {
        let Some(s) = pmt.streams.first() else {
            return -1;
        };
        self.get_pid_mapping_allddci(pmt.adapter, s.pid)
            .map_or(-1, |m| m.ddci)
    }

    fn find_registered_ddci(&self, pmt_id: i32) -> i32 {
        self.devices
            .iter()
            .filter(|(_, d)| d.enabled)
            .find(|(_, d)| d.slots().any(|j| d.pmt[j].id == pmt_id))
            .map_or(-1, |(&i, _)| i as i32)
    }

    /// `ddci_process_pmt`.
    pub fn process_pmt(&mut self, ad: u16, pmt: &Program, update: bool) -> TablesResult {
        if self.get_ddci(i32::from(ad)).is_some() {
            ms_log::log!("Skip processing pmt for ddci adapter {}", ad);
            return TablesResult::Ok;
        }
        let registered = self.find_registered_ddci(pmt.id);
        let mut ddid = registered;
        if ddid == -1 {
            ddid = self.is_pmt_running(pmt);
        }
        ms_log::log!(
            "ddci_process_pmt: adapter {}, pmt {}, pid {}, sid {}, ddid {}, update {}, name: {}",
            ad,
            pmt.id,
            pmt.pid,
            pmt.sid,
            ddid,
            u8::from(update),
            pmt.name
        );
        if registered == -1 {
            let channel = match self.channels.get(&pmt.sid) {
                Some(c) => c.clone(),
                None => match self.create_channel_for_pmt(pmt) {
                    Err(r) => {
                        ms_log::log_and_return!(
                            r,
                            "DDCI not ready or busy at the moment: {}",
                            if r == TablesResult::NoRetry {
                                "no retry"
                            } else {
                                "retry"
                            }
                        );
                    }
                    Ok(c) if c.ddci.is_empty() => {
                        ms_log::log_and_return!(TablesResult::NoRetry, "no suitable DDCI found");
                    }
                    Ok(c) => {
                        self.channels.insert(pmt.sid, c.clone());
                        c
                    }
                },
            };
            if ddid == -1 {
                match self.find_ddci_for_pmt(&channel, pmt) {
                    Ok(id) => ddid = id,
                    Err(TablesResult::Retry) => {
                        // Unlocked candidates may predate a DDCI that became ready since.
                        if !channel.locked {
                            self.channels.remove(&pmt.sid);
                        }
                        return TablesResult::Retry;
                    }
                    Err(r) => return r,
                }
            }
        } else if self.is_ca_initializing(ddid as usize) {
            return TablesResult::Retry;
        }
        if self.get_ddci(ddid).is_none() {
            ms_log::log!(
                "Could not find ddci device for adapter {}, ddci {}",
                ad,
                ddid
            );
            return TablesResult::NoRetry;
        }
        let dev = ddid as usize;
        let transponder_id = self.adapters.get(&ad).map_or(0, |a| a.transponder_id);
        let d = self.devices.get_mut(&dev).unwrap();
        let mut pos = None;
        let mut already_registered = false;
        for i in d.slots() {
            if d.pmt[i].id == pmt.id {
                pos = Some(i);
                already_registered = true;
            }
        }
        if pos.is_none() {
            pos = d.slots().find(|&i| d.pmt[i].id == -1);
        }
        let d_id = d.id;
        let Some(pos) = pos else {
            ms_log::log_and_return!(
                TablesResult::Retry,
                "No free slot found for pmt {} on DDCI {}",
                pmt.id,
                d_id
            );
        };
        d.pmt[pos].id = pmt.id;
        d.pmt[pos].last_pmt = 0;
        d.bump_versions();
        if !already_registered {
            if d.channels == 0 {
                d.tid = transponder_id;
            }
            d.channels += 1;
            d.psi_changed = true;
        }
        // C++ `count_new`: mappings this call creates, for the summary log.
        // `has_pid_mapping` only tells that the entry was absent entirely.
        let mut mapped = 0;
        // Mandatory pids; some CAMs need the TDT to wake up. Pid 0 comes from post_tune.
        for pid in [1u16, 20] {
            let d = self.devices.get_mut(&dev).unwrap();
            if !d.mapping.contains_key(&(pmt.adapter, pid)) {
                ms_log::log!(
                    "Mapping mandatory PID {} to PMT {} on DDCI {}",
                    pid,
                    pmt.id,
                    d.id
                );
                mapped += 1;
                if pid == 1 {
                    d.cat_processed = false;
                }
                self.add_pid_mapping_table(ad, pid, pmt.id, dev, true);
            }
        }
        {
            let d = &self.devices[&dev];
            ms_log::log!(
                "found DDCI {} for pmt {}, running channels {}, max_channels {}",
                ddid,
                pmt.id,
                d.channels,
                d.max_channels
            );
        }
        count_new_mapping(&self.devices[&dev], ad, pmt.pid, pmt, &mut mapped);
        self.add_pid_mapping_table(ad, pmt.pid, pmt.id, dev, false);
        if let Some(m) = self
            .devices
            .get_mut(&dev)
            .unwrap()
            .mapping
            .get_mut(&(ad, pmt.pid))
        {
            m.rewrite = false; // the PMT pid is regenerated, not forwarded
        }
        for ca in &pmt.caids {
            ms_log::logm!("DD {} adding ECM pid {}", self.devices[&dev].id, ca.pid);
            count_new_mapping(&self.devices[&dev], ad, ca.pid, pmt, &mut mapped);
            self.add_pid_mapping_table(ad, ca.pid, pmt.id, dev, true);
        }
        for s in &pmt.streams {
            ms_log::logm!(
                "DD {} adding stream pid {} {}",
                self.devices[&dev].id,
                s.pid,
                if s.pid == pmt.pcr_pid { "PCR" } else { "" }
            );
            count_new_mapping(&self.devices[&dev], ad, s.pid, pmt, &mut mapped);
            let ddci_pid = self.add_pid_mapping_table(ad, s.pid, pmt.id, dev, false);
            if s.pid == pmt.pcr_pid {
                self.devices.get_mut(&dev).unwrap().pmt[pos].pcr_pid = ddci_pid;
            }
        }
        // A re-send doubles as the update path; on first adds it heals stale entries.
        let unmapped = self.unmap_removed_pids(dev, ad, pmt);
        ms_log::log!(
            "DDCI {}: PMT {} {} on slot {} (+{} -{} pids), keeping ddci pids",
            self.devices[&dev].id,
            pmt.id,
            if update { "update" } else { "add" },
            pos,
            mapped,
            unmapped
        );
        self.dump_mapping_table();
        TablesResult::Ok
    }

    /// `ddci_del_pmt`.
    pub fn del_pmt(&mut self, ad: u16, pmt: &Program) {
        if self.get_ddci(i32::from(ad)).is_some() {
            ms_log::log!("Skip deleting pmt for ddci adapter {}", ad);
            return;
        }
        let Some((ddci, ddci_pid)) = self
            .get_pid_mapping_allddci(ad, pmt.pid)
            .map(|m| (m.ddci, m.ddci_pid))
        else {
            self.dump_mapping_table();
            ms_log::log!(
                "ddci_del_pmt: pid mapping for adapter {}, pmt {} and pid {} not found",
                ad,
                pmt.id,
                pmt.pid
            );
            // No mapping: still free a slot this PMT holds, or it leaks.
            for d in self.devices.values_mut().filter(|d| d.enabled) {
                for j in d.slots() {
                    if d.pmt[j].id == pmt.id {
                        d.pmt[j].id = -1;
                        if d.channels > 0 {
                            d.channels -= 1;
                        }
                        d.bump_versions();
                        d.psi_changed = true;
                    }
                }
            }
            return;
        };
        if self.get_ddci(ddci).is_none() {
            ms_log::log!("ddci_del_pmt: ddci {} already disabled", ddci);
            return;
        }
        let dev = ddci as usize;
        let d = self.devices.get_mut(&dev).unwrap();
        d.bump_versions();
        let mut was_registered = false;
        for i in d.slots() {
            if d.pmt[i].id == pmt.id {
                d.pmt[i].id = -1;
                was_registered = true;
            }
        }
        if was_registered {
            if d.channels > 0 {
                d.channels -= 1;
            }
            d.psi_changed = true;
        }
        ms_log::log!(
            "ddci_del_pmt: deleting pmt id {}, sid {} ({:X}), pid {}, ddci {}, name {}, held a slot {}, running channels {}",
            pmt.id,
            pmt.sid,
            pmt.sid,
            ddci_pid,
            ddci,
            pmt.name,
            u8::from(was_registered),
            d.channels
        );
        self.del_pmt_mapping_table(dev, ad, pmt.id);
        self.dump_mapping_table();
    }

    // ---- generated tables ----

    /// `ddci_create_pat`: pointer field, then the section with CRC.
    pub fn create_pat(&self, dev: usize, programs: &ProgramTable) -> Vec<u8> {
        let d = &self.devices[&dev];
        let mut b = vec![0, 0, 0xB0, 0];
        b.extend_from_slice(&d.tid.to_be_bytes());
        b.push(0xC1 | ((d.ver as u8) << 1));
        b.extend_from_slice(&[0, 0, 0, 0, 0x00, 0x10]);
        for i in d.slots() {
            let Some(pmt) = programs.get(d.pmt[i].id) else {
                continue;
            };
            let Some(m) = d.mapping.get(&(pmt.adapter, pmt.pid)) else {
                ms_log::log!(
                    "adapter {} pid {} not found in the mapping table",
                    pmt.adapter,
                    pmt.pid
                );
                continue;
            };
            b.extend_from_slice(&pmt.sid.to_be_bytes());
            b.extend_from_slice(&(0xE000 | m.ddci_pid as u16).to_be_bytes());
        }
        let len1 = b.len();
        b[2] |= (len1 >> 8) as u8;
        b[3] |= len1 as u8;
        let crc = crc32_mpeg2(&b[1..len1]);
        ms_log::logm!(
            "ddci_create_pat: transponder {} version {}, len {}",
            d.tid,
            d.ver,
            len1
        );
        let desc = format!("PAT Created CRC {crc:08X}: ");
        ms_log::hexdump!(&desc, &b[1..len1]);
        b.extend_from_slice(&crc.to_be_bytes());
        b
    }

    /// `ddci_create_sdt`; bumps the SDT version when services gain real SDT data.
    pub fn create_sdt(&mut self, dev: usize, programs: &ProgramTable) -> Vec<u8> {
        let d = self.devices.get_mut(&dev).unwrap();
        let mut b = vec![0x00, 0x42, 0xF0, 0x00];
        let (mut tsid, mut onid, mut data_mask) = (1u16, 0u16, 0u32);
        let mut have_ids = false;
        for i in d.slots() {
            if let Some(sdt) = programs.get(d.pmt[i].id).and_then(|p| p.sdt) {
                data_mask |= 1 << i;
                if !have_ids {
                    tsid = sdt.tsid;
                    onid = sdt.onid;
                    have_ids = true;
                }
            }
        }
        if data_mask != d.sdt_data_mask {
            d.sdt_data_mask = data_mask;
            d.sdt_ver = (d.sdt_ver + 1) & 0xF;
        }
        b.extend_from_slice(&tsid.to_be_bytes());
        b.push(0xC1 | ((d.sdt_ver as u8) << 1));
        b.extend_from_slice(&[0, 0]);
        b.extend_from_slice(&onid.to_be_bytes());
        b.push(0x00);
        for i in d.slots() {
            let Some(pmt) = programs.get(d.pmt[i].id) else {
                continue;
            };
            ms_log::logm!("Adding PMT {} to SDT, sid {}", d.pmt[i].id, pmt.sid);
            if !d.mapping.contains_key(&(pmt.adapter, pmt.pid)) {
                ms_log::log!(
                    "adapter {} pid {} not found in the mapping table",
                    pmt.adapter,
                    pmt.pid
                );
                continue;
            }
            b.extend_from_slice(&pmt.sid.to_be_bytes());
            let sdt = pmt.sdt;
            b.push(sdt.map_or(0, |s| (s.eit_schedule << 1) | s.eit_pf));
            let r =
                (sdt.map_or(4, |s| s.running_status) << 5) | (sdt.map_or(1, |s| s.ca_mode) << 4);
            b.push(r);
            let dlen = b.len();
            b.push(0);
            if let Some(s) = sdt {
                b.push(0x48);
                let sdlen = b.len();
                b.push(0);
                b.push(s.service_type);
                dvb_put_string(&mut b, &pmt.provider);
                dvb_put_string(&mut b, &pmt.name);
                b[sdlen] = (b.len() - sdlen - 1) as u8;
            }
            b[dlen] = (b.len() - dlen - 1) as u8;
        }
        // section_length is 12 bits: descriptors can push it past 255
        let section_length = b.len() - 4 + 4;
        b[2] |= ((section_length >> 8) & 0x0F) as u8;
        b[3] = section_length as u8;
        let crc = crc32_mpeg2(&b[1..]);
        b.extend_from_slice(&crc.to_be_bytes());
        b
    }

    /// `ddci_create_eit`: a present event that started an hour ago.
    pub fn create_eit(&self, sid: u16, version: i32) -> Vec<u8> {
        let tm = self.eit_start;
        let mjd = ymd_to_mjd(tm.year, tm.month, tm.mday);
        let mut p = vec![0x00, 0x4E, 0xF0, 0x00];
        p.extend_from_slice(&sid.to_be_bytes());
        p.push(0xC1 | (((version & 0x0F) as u8) << 1));
        p.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0x4E, 0x00, 0x01]);
        p.extend_from_slice(&mjd.to_be_bytes());
        p.extend_from_slice(&[tm.hour, tm.min, tm.sec, 0x24, 0x00, 0x00]);
        p.push((4 << 5) ^ (1 << 4));
        p.push(0x00);
        let desc = p.len();
        p.extend_from_slice(&[0x55, 0x04, b'9', b'0', b'2', 0]);
        p[3] = (p.len() - 4 + 4) as u8;
        p[desc - 1] = (p.len() - desc) as u8;
        let crc = crc32_mpeg2(&p[1..]);
        p.extend_from_slice(&crc.to_be_bytes());
        p
    }

    fn safe_get_pid_mapping(d: &DdciDevice, ad: u16, pid: u16) -> u16 {
        d.mapping.get(&(ad, pid)).map_or(pid, |m| m.ddci_pid as u16)
    }

    /// `ddci_create_pmt` into at most `pmt_size` bytes; bumps `dp.ver` when content changed.
    pub fn create_pmt(
        &self,
        dev: usize,
        pmt: &Program,
        pmt_size: usize,
        dp: &mut DdciPmt,
    ) -> Vec<u8> {
        let d = &self.devices[&dev];
        let mut b = vec![0, 0x02, 0, 0];
        let start_pmt = b.len();
        b.extend_from_slice(&pmt.sid.to_be_bytes());
        b.push(((dp.ver << 1) | 0x01) as u8);
        b.extend_from_slice(&[0, 0]);
        b.push(0xE0 | ((dp.pcr_pid >> 8) & 0xFF) as u8);
        b.push(dp.pcr_pid as u8);
        let start_pi_len = b.len();
        b.extend_from_slice(&[0, 0]);
        ms_log::logm!(
            "ddci_create_pmt: PMT {} AD {}, pid: {:04X} ({}), ver {}, sid {:04X} ({}) {} {}",
            pmt.id,
            pmt.adapter,
            pmt.pid,
            pmt.pid,
            dp.ver,
            pmt.sid,
            pmt.sid,
            if pmt.name.is_empty() { "" } else { "channel:" },
            pmt.name
        );
        let mut pi_len = 0usize;
        for ca in &pmt.caids {
            let private = ca.private_data.len();
            if b.len() + 10 + private > pmt_size {
                ms_log::log!(
                    "ddci_create_pmt: truncating PMT {} program CA list, buffer full",
                    pmt.id
                );
                break;
            }
            b.push(0x09);
            b.push((4 + private) as u8);
            b.extend_from_slice(&ca.caid.to_be_bytes());
            b.extend_from_slice(&Self::safe_get_pid_mapping(d, pmt.adapter, ca.pid).to_be_bytes());
            b.extend_from_slice(&ca.private_data);
            pi_len += 6 + private;
            ms_log::logm!(
                "ddci_create_pmt: pmt {} added caid {:04X}, pid {:04X}",
                pmt.id,
                ca.caid,
                ca.pid
            );
        }
        b[start_pi_len..start_pi_len + 2].copy_from_slice(&(pi_len as u16).to_be_bytes());
        let multiple = self.ca_multiple_pmt(d.id) > 0;
        for s in &pmt.streams {
            if multiple {
                // Do not map pids the client did not request.
                let pids = self.adapters.get(&pmt.adapter).map(|a| &a.client_pids);
                let owned = pids.and_then(|p| p.get(&s.pid).or_else(|| p.get(&8192)));
                if owned != Some(&true) {
                    ms_log::logm!(
                        "ddci_create_pmt: adapter {} pid {} not requested by the client",
                        pmt.adapter,
                        s.pid
                    );
                    continue;
                }
            }
            let es_info_len: usize = s.descriptors.iter().map(|d| d.data.len() + 2).sum();
            if b.len() + 9 + es_info_len > pmt_size {
                ms_log::log!(
                    "ddci_create_pmt: truncating PMT {} stream list, buffer full",
                    pmt.id
                );
                break;
            }
            b.push(s.stream_type);
            b.extend_from_slice(&Self::safe_get_pid_mapping(d, pmt.adapter, s.pid).to_be_bytes());
            b.extend_from_slice(&(es_info_len as u16).to_be_bytes());
            for desc in &s.descriptors {
                b.push(desc.tag);
                b.push(desc.data.len() as u8);
                if desc.is_ca() && desc.data.len() >= 4 {
                    let raw = (u16::from(desc.data[2] & 0x1F) << 8) | u16::from(desc.data[3]);
                    let mapped = Self::safe_get_pid_mapping(d, pmt.adapter, raw);
                    b.extend_from_slice(&desc.data[..2]);
                    b.push((desc.data[2] & 0xE0) | ((mapped >> 8) as u8 & 0x1F));
                    b.push(mapped as u8);
                    b.extend_from_slice(&desc.data[4..]);
                } else {
                    b.extend_from_slice(&desc.data);
                }
            }
            ms_log::logm!(
                "ddci_create_pmt: pmt {} added pid {:04X}, type {:02X}, es_len {}",
                pmt.id,
                s.pid,
                s.stream_type,
                es_info_len
            );
        }
        let section_len = (4 + b.len() - start_pmt) as u16;
        b[start_pmt - 2..start_pmt].copy_from_slice(&section_len.to_be_bytes());
        let mut crc = crc32_mpeg2(&b[1..]);
        // The user may have added or removed pids: a content change bumps the version.
        if dp.crc != crc {
            ms_log::logm!(
                "ddci_create_pmt: PMT {} content changed, bumping ver {} -> {}",
                pmt.id,
                dp.ver,
                (dp.ver + 1) & 0xF
            );
            dp.ver = (dp.ver + 1) & 0xF;
            b[start_pmt + 2] = ((dp.ver << 1) | 0x01) as u8;
            crc = crc32_mpeg2(&b[1..]);
        }
        dp.crc = crc;
        b.extend_from_slice(&crc.to_be_bytes());
        b
    }

    /// `ddci_create_pmt(d, pmt, buf, size, &d->pmt[slot])`.
    pub fn create_pmt_for_slot(
        &mut self,
        dev: usize,
        slot: usize,
        pmt: &Program,
        pmt_size: usize,
    ) -> Vec<u8> {
        let mut dp = self.devices[&dev].pmt[slot];
        let psi = self.create_pmt(dev, pmt, pmt_size, &mut dp);
        self.devices.get_mut(&dev).unwrap().pmt[slot] = dp;
        psi
    }

    /// `ddci_update_pmt_name`: name the CI-side PMT once the source is named.
    fn update_pmt_name(&self, dev: usize, pmt: &Program, programs: &mut ProgramTable) {
        if pmt.name.is_empty() {
            return;
        }
        let ci = self.devices[&dev].id as u16;
        let Some(dpmt) = programs.first_for_sid(ci, pmt.sid) else {
            return;
        };
        if dpmt.id == pmt.id || dpmt.name == pmt.name {
            return;
        }
        let id = dpmt.id;
        ms_log::log!(
            "DD {}: naming PMT {} (sid {}) {}",
            self.devices[&dev].id,
            dpmt.id,
            dpmt.sid,
            pmt.name
        );
        let dpmt = programs.get_mut(id).unwrap();
        dpmt.name = pmt.name.clone();
        dpmt.provider = pmt.provider.clone();
    }

    /// `ddci_add_psi`: PAT/SDT on change or every 500 ms, each channel's PMT+EIT every 100 ms.
    pub fn add_psi(
        &mut self,
        dev: usize,
        dst: &mut [u8],
        ad: u16,
        programs: &mut ProgramTable,
    ) -> isize {
        let ctime = self.now_ms;
        let len = dst.len();
        let mut pos: isize = 0;
        let d = &self.devices[&dev];
        let now = d.psi_changed;
        let (mut pat_sent, mut sdt_sent) = (false, false);
        // Unsigned like C++ (int64 - uint64): a timer set ahead of ctime reads as due.
        if now || ctime.wrapping_sub(d.last_pat) > 500 {
            let psi = self.create_pat(dev, programs);
            let d = self.devices.get_mut(&dev).unwrap();
            let n = buffer_to_ts(&mut dst[pos as usize..], &psi, &mut d.pat_cc, 0);
            if n > 0 {
                pos += n;
                d.last_pat = ctime;
                pat_sent = true;
            }
        }
        if now || ctime.wrapping_sub(self.devices[&dev].last_sdt) > 500 {
            let psi = self.create_sdt(dev, programs);
            let d = self.devices.get_mut(&dev).unwrap();
            let n = buffer_to_ts(&mut dst[pos as usize..], &psi, &mut d.sdt_cc, 17);
            if n > 0 {
                pos += n;
                d.last_sdt = ctime;
                sdt_sent = true;
            }
        }
        if now && pat_sent && sdt_sent {
            self.devices.get_mut(&dev).unwrap().psi_changed = false;
        }
        let slots = self.devices[&dev].slots();
        for i in slots {
            let slot = self.devices[&dev].pmt[i];
            if ctime.wrapping_sub(slot.last_pmt) <= 100 {
                continue;
            }
            // Foreign channels wait for their own adapter's tick.
            let Some(pmt) = programs.get(slot.id).filter(|p| p.adapter == ad).cloned() else {
                continue;
            };
            self.update_pmt_name(dev, &pmt, programs);
            let mut dp = slot;
            let psi = self.create_pmt(dev, &pmt, 1500, &mut dp);
            let mapped = self.devices[&dev]
                .mapping
                .get(&(pmt.adapter, pmt.pid))
                .map(|m| m.ddci_pid);
            // C++ adds buffer_to_ts's -1 to pos; Rust counts a failed packing as 0 (DEV-100).
            if let Some(ddci_pid) = mapped {
                let at = pos.clamp(0, len as isize) as usize;
                pos += buffer_to_ts(&mut dst[at..], &psi, &mut dp.cc, ddci_pid as u16).max(0);
            } else {
                ms_log::log!(
                    "ddci_add_psi: could not find PMT {} adapter {} and pid {} to mapping table",
                    slot.id,
                    pmt.adapter,
                    pmt.pid
                );
            }
            let eit = self.create_eit(pmt.sid, dp.ver);
            let d = self.devices.get_mut(&dev).unwrap();
            let at = pos.clamp(0, len as isize) as usize;
            pos += buffer_to_ts(&mut dst[at..], &eit, &mut d.eit_cc, 18).max(0);
            dp.last_pmt = ctime;
            d.pmt[i] = dp;
        }
        pos
    }

    /// `push_ts_to_adapter`: fills null/free slots of the adapter buffer from the CAM FIFO.
    pub fn push_ts_to_adapter(&mut self, dev: usize, ad: &mut TsBuffer, mapping: &[u16; 8192]) {
        let d = self.devices.get_mut(&dev).unwrap();
        let lbuf = ad.buf.len();
        let mut i = 0usize;
        let mut popped = 0usize;
        while i + DVB_FRAME < lbuf {
            if i < ad.rlen && pid_from_ts(&ad.buf[i..]) != 0x1FFF {
                i += DVB_FRAME;
                continue;
            }
            let ri = &mut d.read_index[usize::from(ad.id)];
            let n = d.fifo.pop_offset(&mut ad.buf[i..i + DVB_FRAME], ri);
            if n == 0 {
                break;
            }
            popped += n;
            let pid = pid_from_ts(&ad.buf[i..]);
            let dpid = mapping[usize::from(pid)];
            if dpid == 0 {
                // Not mapped to this adapter: drop it and reuse the slot.
                ms_log::debugm!(
                    "Skipping pid {}, ddci_pid {} on adapter {} DDCI {}",
                    pid,
                    dpid,
                    ad.id,
                    d.id
                );
                set_pid_ts(&mut ad.buf[i..], 0x1FFF);
                continue;
            }
            set_pid_ts(&mut ad.buf[i..], dpid);
            if i >= ad.rlen {
                ad.rlen = i + DVB_FRAME;
            }
            ms_log::debugm!(
                "DDCI {} pid {} -> AD {} pid {}, pos {}, len {}, left {} [ri {}]",
                d.id,
                pid,
                ad.id,
                dpid,
                i / DVB_FRAME,
                n,
                d.fifo.write_index.wrapping_sub(*ri),
                *ri
            );
            ms_log::dump_packets!("DDCI -> AD", &ad.buf[i..i + DVB_FRAME], i as i64);
            i += DVB_FRAME;
        }
        ms_log::debugm!(
            "popped {} bytes from fifo up to index {}, rlen {}, lbuf {}, left in fifo {}",
            popped,
            i,
            ad.rlen,
            lbuf,
            d.fifo
                .write_index
                .wrapping_sub(d.read_index[usize::from(ad.id)])
        );
    }

    /// `ddci_process_ts`: sends the adapter's mapped packets plus PSI to the CI, then pulls CAM output back.
    pub fn process_ts(
        &mut self,
        ad: &mut TsBuffer,
        dev: usize,
        programs: &mut ProgramTable,
        ci: &mut dyn CiWriter,
    ) {
        let Some(d) = self.devices.get(&dev) else {
            return;
        };
        if !d.enabled || !self.adapters.contains_key(&(d.id as u16)) {
            return;
        }
        let dev_id = d.id;
        let mut ad_dd = [0u16; 8192];
        let mut dd_ad = [0u16; 8192];
        let mut ec = 0;
        for m in d.mapping.values().filter(|m| m.ad == ad.id && m.rewrite) {
            ad_dd[usize::from(m.pid)] = m.ddci_pid as u16;
            dd_ad[(m.ddci_pid as usize) & 0x1FFF] = m.pid;
            ec += 1;
            ms_log::debugm!(
                "Using pid {} in adapter {} as pid {} in DDCI {}",
                m.pid,
                ad.id,
                m.ddci_pid,
                dev_id
            );
        }
        let d = self.devices.get_mut(&dev).unwrap();
        if ec == 0 {
            d.read_index[usize::from(ad.id)] = 0;
            return;
        }
        // Start at the write index so old FIFO data is not read.
        if d.read_index[usize::from(ad.id)] == 0 {
            d.read_index[usize::from(ad.id)] = d.fifo.write_index;
        }
        let mut runs: Vec<(usize, usize)> = Vec::new();
        let mut i = 0;
        while i + DVB_FRAME <= ad.rlen {
            let b = &mut ad.buf[i..i + DVB_FRAME];
            if b[0] == 0x47 {
                let pid = pid_from_ts(b);
                let dpid = ad_dd[usize::from(pid)];
                if dpid != 0 {
                    ms_log::dump_packets!("ddci_process_ts -> DD", &b[..DVB_FRAME], i as i64);
                    set_pid_ts(b, dpid);
                    match runs.last_mut() {
                        Some((s, l)) if *s + *l == i => *l += DVB_FRAME,
                        _ => runs.push((i, DVB_FRAME)),
                    }
                    ms_log::debugm!(
                        "pos {} of {}, mapping pid {} to {}, buf pos {}",
                        runs.len() - 1,
                        ad.rlen / DVB_FRAME,
                        pid,
                        dpid,
                        i / DVB_FRAME
                    );
                }
            }
            i += DVB_FRAME;
        }
        if !runs.is_empty() {
            let mut psi = vec![0u8; MAX_CHANNELS_ON_CI * 1500 - 1];
            let psi_len = self.add_psi(dev, &mut psi, ad.id, programs);
            ms_log::hexdump!("PSI -> ", &psi[..usize::try_from(psi_len).unwrap_or(0)]);
            // Every write ends with a null packet: the FPGA reads in 128 byte units (#1437).
            let mut null_tail = [0xFFu8; DVB_FRAME];
            null_tail[..4].copy_from_slice(&[0x47, 0x1F, 0xFF, 0x10]);
            let mut iov: Vec<&[u8]> = runs.iter().map(|&(s, l)| &ad.buf[s..s + l]).collect();
            if psi_len > 0 {
                iov.push(&psi[..psi_len as usize]);
            }
            iov.push(&null_tail);
            let bytes: usize = iov.iter().map(|s| s.len()).sum();
            ms_log::debugm!(
                "writing {} bytes to DDCI device {}, fd {}, sock {}",
                bytes,
                dev_id,
                ci.fd(),
                ci.socket()
            );
            let rb = ci.writev(&iov);
            if rb != bytes as isize {
                ms_log::log!(
                    "ddci_process_ts: write incomplete to DDCI {},fd {}, wrote {} out of {} (iop {}{}), errno {}: {}",
                    dev_id,
                    ci.fd(),
                    rb,
                    bytes,
                    iov.len(),
                    if iov.len() > 1024 { " > IOV_MAX" } else { "" },
                    ci.last_errno(),
                    ci.last_error()
                );
            }
        }
        // The written packets become null packets in the adapter buffer.
        for &(s, l) in &runs {
            for j in (s..s + l).step_by(DVB_FRAME) {
                set_pid_ts(&mut ad.buf[j..], 0x1FFF);
            }
        }
        self.push_ts_to_adapter(dev, ad, &dd_ad);
    }

    /// `ddci_ts`: every enabled DDCI processes a non-DDCI adapter's buffer.
    pub fn ddci_ts(
        &mut self,
        ad: &mut TsBuffer,
        programs: &mut ProgramTable,
        ci: &mut dyn CiWriter,
    ) {
        if self.get_ddci(i32::from(ad.id)).is_some() {
            return;
        }
        let devs: Vec<usize> = self
            .devices
            .iter()
            .filter(|(_, d)| d.enabled)
            .map(|(&i, _)| i)
            .collect();
        for dev in devs {
            self.process_ts(ad, dev, programs, ci);
        }
    }

    /// `ddci_read_sec_data`: CAM output into the device FIFO.
    pub fn push_from_cam(&mut self, dev: usize, ts: &[u8]) {
        if let Some(d) = self.devices.get_mut(&dev) {
            let left = d.fifo.push(ts);
            if left == 0 {
                ms_log::log!("dropping {} bytes for ddci_adapter {}", left, d.id);
            }
            ms_log::debugm!(
                "pushed {} bytes to the adapter buffer from {} [write_index {}]",
                left,
                ts.len(),
                d.fifo.write_index
            );
            let buffered = &d.fifo.raw_data()[..188.min(d.fifo.raw_data().len())];
            ms_log::dump_packets!("DDCI ->FIFO ", buffered, 0);
            ms_log::dump_packets!("SOURCE ", &ts[..188.min(ts.len())], 0);
        }
    }

    /// `ddci_process_cat`: maps the CAT EMM pids once per device.
    pub fn process_cat(&mut self, ad: u16, section: &[u8], dev: usize) {
        if section.first() != Some(&1) {
            return;
        }
        let Some(d) = self.devices.get(&dev) else {
            return;
        };
        if !d.enabled {
            ms_log::log!("DDCI {} no longer enabled, not processing CAT", d.id);
            return;
        }
        if d.cat_processed || d.disable_cat || section.len() < 8 {
            return;
        }
        let cat_len = (usize::from(section[1] & 0x0F) << 8) | usize::from(section[2]);
        let cat_ver = (section[5] & 0x3E) >> 1;
        ms_log::log!("CAT DDCI {} ver {}, len {}", d.id, cat_ver, cat_len);
        if cat_len > 1500 {
            return;
        }
        let b = &section[8..];
        let mut emm = Vec::new();
        let mut i = 0;
        let mut k = 0;
        while i + 4 < cat_len && i + 1 < b.len() {
            let es_len = usize::from(b[i + 1]) + 2;
            if b[i] == 9 && i + 5 < b.len() {
                let caid = u16::from(b[i + 2]) * 256 + u16::from(b[i + 3]);
                let pid = (u16::from(b[i + 4] & 0x1F) << 8) | u16::from(b[i + 5]);
                ms_log::log!("CAT pos {} caid {:04X}, pid {}", k, caid, pid);
                emm.push(pid);
            }
            i += es_len;
            k += 1;
        }
        let duplicate = emm
            .iter()
            .find(|&&p| d.get_ddci_pid(i32::from(p)).is_some())
            .copied();
        if let Some(p) = duplicate {
            ms_log::log!("CAT pid {} already in use, skipping CAT", p);
            self.devices.get_mut(&dev).unwrap().cat_processed = true;
            return;
        }
        let d = self.devices.get_mut(&dev).unwrap();
        // Rebuild this adapter's EMM set so dropped EMMs lose sweep protection.
        d.emm_pids.retain(|&(a, _)| a != ad);
        let pmt0 = d.pmt[0].id;
        for &p in &emm {
            self.add_pid_mapping_table(ad, p, pmt0, dev, true);
            self.devices.get_mut(&dev).unwrap().emm_pids.insert((ad, p));
        }
        self.devices.get_mut(&dev).unwrap().cat_processed = true;
    }

    /// `ddci_process_cat` reached through the section filter added for pid 1.
    pub fn process_cat_filter(&mut self, filter: usize, section: &[u8]) {
        if let Some(&(ad, dev)) = self.filters.get(&filter) {
            self.process_cat(ad, section, dev);
        }
    }

    /// `save_channels`: never overwrites an existing ddci.conf.
    pub fn save_channels(&self, path: &Path) -> io::Result<()> {
        if path.exists() {
            ms_log::log!("{} already exists, not saving", path.display());
            return Ok(());
        }
        let mut s = String::from(
            "# Format: SID: DDCI1,DDCI2\n# Use # for comments\n# To decrypt channel with sid SID it will try in order DDCI1 then DDCI2\n# To prevent the SID from using any DDCI, include an empty list (eg: SID: )\n# When a new channel it will be used, minisatip will determine the matching DDCIs based on their reported CA IDs\n",
        );
        for c in self.channels.values() {
            let list: Vec<String> = c.ddci.iter().map(u8::to_string).collect();
            s.push_str(&format!("{}:    {} # {}\n", c.sid, list.join(","), c.name));
        }
        std::fs::write(path, s)
    }

    /// `load_channels`: loaded entries are locked to their DDCI list.
    pub fn load_channels(&mut self, path: &Path) -> io::Result<usize> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e),
        };
        let mut n = 0;
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("");
            let Some((sid, list)) = line.split_once(':') else {
                continue;
            };
            let sid = parse_int(sid, 0);
            if sid <= 0 || sid > 0xFFFF {
                continue;
            }
            let c = self.channels.entry(sid as u16).or_default();
            c.sid = sid as u16;
            c.locked = true;
            n += 1;
            let mut shown = String::new();
            for (i, item) in list.trim().split(',').enumerate() {
                let v = parse_int(item, -1);
                if v != -1 {
                    // C++ separates on the argument index, even after a skip.
                    if i > 0 {
                        shown.push(',');
                    }
                    shown.push_str(&v.to_string());
                    c.ddci.push(v as u8);
                }
            }
            ms_log::logm!("Adding channel {}: DDCIs: {}", c.sid, shown);
        }
        ms_log::log!("Loaded {} channels from {}", n, path.display());
        Ok(n)
    }
}

/// `count_new` in `ddci_process_pmt`: counts the mappings a call creates.
fn count_new_mapping(d: &DdciDevice, ad: u16, pid: u16, pmt: &Program, mapped: &mut usize) {
    if !d
        .mapping
        .get(&(ad, pid))
        .is_some_and(|m| m.pmt.contains(&pmt.id))
    {
        *mapped += 1;
    }
}

/// `parse_int`: a leading decimal integer, else `default`.
fn parse_int(s: &str, default: i64) -> i64 {
    let s = s.trim();
    let end = s
        .char_indices()
        .find(|&(i, ch)| !(ch.is_ascii_digit() || (i == 0 && ch == '-')))
        .map_or(s.len(), |(i, _)| i);
    s[..end].parse().unwrap_or(default)
}
