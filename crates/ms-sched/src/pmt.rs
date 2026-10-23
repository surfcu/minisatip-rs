//! PMT claim arbitration, CA handover and PSI section handling.
//!
//! This is the Rust counterpart of `pmt.cpp`'s allocation half. Claims are
//! stored on the scheduler PID table (`PidEntry::pmt`), exactly like
//! `SPid::pmt`; the PMT and filter slot tables live next to it because they
//! are adapter state. CAPMT delivery goes through [`CaSink`], whose result
//! type is [`ms_ca::TablesResult`], so the DVBAPI/CAM device and reconciler
//! keep ownership of protocol details.

use std::collections::BTreeSet;

use ms_ca::TablesResult;
use ms_ca::{
    CaDescriptor as CaServiceDescriptor, CaService, SdtInfo, StreamPid as CaServiceStream,
};
use ms_psi::{CrcCheck, Pmt as ParsedPmt, SDT_ACTUAL_TABLE_ID, Sdt};

use crate::{AdapterType, PidEntry, PidState, Scheduler, StreamId};

/// `DEFAULT_LOG` of `pmt.cpp` (`#define DEFAULT_LOG LOG_PMT`).
const DEFAULT_LOG: u32 = ms_log::LOG_PMT;

/// PMT slots (`MAX_PMT`).
pub const MAX_PMT: usize = 25_600;
/// CA descriptors kept per PMT (`MAX_CAID`).
pub const MAX_CAID: usize = 20;
/// Section filters (`MAX_FILTERS`).
pub const MAX_FILTERS: usize = 200;
/// Filter flag: add/remove the PID with the filter (`FILTER_ADD_REMOVE`).
pub const FILTER_ADD_REMOVE: i32 = 1;
/// Filter flag: survive retuning (`FILTER_PERMANENT`).
pub const FILTER_PERMANENT: i32 = 4;
/// Filter flag: invert the mask match (`FILTER_REVERSE`).
pub const FILTER_REVERSE: i32 = 8;
/// Filter flag: verify the section CRC (`FILTER_CRC`).
pub const FILTER_CRC: i32 = 16;
/// Filter flag: EMM sections, matched one by one after assembly (`FILTER_EMM`).
pub const FILTER_EMM: i32 = 32;
/// Subscription marker DDCI uses on CI adapters (`DDCI_SID`).
pub const DDCI_SID: StreamId = 257; // MAX_STREAMS + 1 (stream.h:13)

/// PMT lifecycle state (`PMT_STOPPED`..`PMT_CACHED`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PmtState {
    /// Not running (`PMT_STOPPED`).
    #[default]
    Stopped,
    /// Started, waiting for the election to promote it (`PMT_STARTING`).
    Starting,
    /// Actively serving its claimed PIDs (`PMT_RUNNING`).
    Running,
    /// Being torn down (`PMT_STOPPING`).
    Stopping,
    /// Retired by PAT but reusable (`PMT_CACHED`).
    Cached,
}

/// One descriptor (`descriptor_t`): tag and payload without tag/length.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Descriptor {
    /// Descriptor tag.
    pub tag: u8,
    /// Payload bytes.
    pub data: Vec<u8>,
}

impl Descriptor {
    /// CA_system_id of a CA descriptor.
    pub fn ca_system_id(&self) -> Option<u16> {
        let data = self.data.get(..2)?;
        Some(u16::from_be_bytes([data[0], data[1]]))
    }

    /// CA_PID of a CA descriptor.
    pub fn ca_pid(&self) -> Option<u16> {
        let data = self.data.get(..4)?;
        Some((((data[2] & 0x1F) as u16) << 8) | u16::from(data[3]))
    }
}

/// One elementary stream of a PMT (`SStreamPid`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamPid {
    /// PMT stream type.
    pub stream_type: i32,
    /// Elementary PID.
    pub pid: i32,
    /// Audio stream.
    pub is_audio: bool,
    /// Video stream.
    pub is_video: bool,
    /// ES descriptors.
    pub descriptors: Vec<Descriptor>,
}

/// One CA descriptor attached to a PMT (`SPMTCA`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PmtCa {
    /// CA system id.
    pub id: u16,
    /// ECM PID.
    pub pid: u16,
    /// Private descriptor data.
    pub private_data: Vec<u8>,
}

/// One PMT object (`SPMT`).
#[derive(Clone, Debug)]
pub struct Pmt {
    /// Slot is in use (`enabled`).
    pub enabled: bool,
    /// Slot ownership, distinct from `enabled` during deletion (`allocated`).
    pub allocated: bool,
    /// Slot id.
    pub id: usize,
    /// Service id.
    pub sid: i32,
    /// PMT PID.
    pub pid: i32,
    /// PCR PID, zeroed when it names an elementary stream.
    pub pcr_pid: i32,
    /// Owning adapter.
    pub adapter: i32,
    /// Last parsed version.
    pub version: i32,
    /// Number of valid entries in [`Pmt::ca`].
    pub caids: usize,
    /// Reusable CA descriptor entries.
    pub ca: Vec<PmtCa>,
    /// Program-level descriptors, deduplicated.
    pub descriptors: Vec<Descriptor>,
    /// Elementary streams in PMT order plus an independent PCR entry.
    pub stream_pids: Vec<StreamPid>,
    /// CAs this PMT was successfully sent to.
    pub ca_mask: u64,
    /// CAs that rejected the PMT with no-retry.
    pub disabled_ca_mask: u64,
    /// CAs holding a registration that must be closed on stop.
    pub ca_registered_mask: u64,
    /// Lifecycle state.
    pub state: PmtState,
    /// Elected into the active set this pass.
    pub best: bool,
    /// Section filter carrying this PMT, `-1` when none.
    pub filter: i32,
    /// Service name from the SDT.
    pub name: String,
    /// Provider name from the SDT.
    pub provider: String,
    /// SDT service fields (`has_sdt` and `sdt_*`, pmt.cpp:2276-2282).
    pub sdt: Option<SdtInfo>,
}

impl Pmt {
    fn new(id: usize) -> Self {
        Self {
            enabled: false,
            allocated: false,
            id,
            sid: 0,
            pid: 0,
            pcr_pid: 0,
            adapter: -1,
            version: -1,
            caids: 0,
            ca: Vec::new(),
            descriptors: Vec::new(),
            stream_pids: Vec::new(),
            ca_mask: 0,
            disabled_ca_mask: 0,
            ca_registered_mask: 0,
            state: PmtState::Stopped,
            best: false,
            filter: -1,
            name: String::new(),
            provider: String::new(),
            sdt: None,
        }
    }
}

/// What a section filter carries (`SFilter::opaque`/callback).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilterTarget {
    /// Not a PMT/PAT filter.
    Other,
    /// PAT filter (`process_pat`).
    Pat,
    /// PMT filter; the id is known after the first parse.
    Pmt(Option<usize>),
    /// SDT filter.
    Sdt,
}

/// One section filter (`SFilter`).
#[derive(Clone, Debug)]
pub struct Filter {
    /// Filter is active.
    pub enabled: bool,
    /// Slot ownership.
    pub allocated: bool,
    /// Slot id.
    pub id: usize,
    /// Filtered PID.
    pub pid: i32,
    /// Owning adapter.
    pub adapter: i32,
    /// Processing target.
    pub target: FilterTarget,
    /// `FILTER_*` flags.
    pub flags: i32,
    /// Match bytes.
    pub filter: [u8; 16],
    /// Match mask.
    pub mask: [u8; 16],
}

impl Filter {
    fn new(id: usize) -> Self {
        Self {
            enabled: false,
            allocated: false,
            id,
            pid: 0,
            adapter: 0,
            target: FilterTarget::Other,
            flags: 0,
            filter: [0; 16],
            mask: [0; 16],
        }
    }
}

/// CA operation interface used by the PMT tables.
///
/// A production sink forwards to the CAM/DVBAPI layer (`ms_ca::CaDevice` or
/// a reconciler) for CAPMT add/update/remove and elementary PID
/// announcements; tests use counters. The bridge lives outside this crate so
/// that ms-sched keeps depending only on the ms-ca interfaces.
pub trait CaSink {
    /// Offers a PMT to the CA; `update` is true for a re-send.
    fn add_pmt(&mut self, service: &CaService, update: bool) -> TablesResult;
    /// Withdraws a previously registered PMT.
    fn del_pmt(&mut self, _service: &CaService) -> TablesResult {
        TablesResult::Ok
    }
    /// Announces a claimed elementary/data PID; `true` invalidates the PMT so
    /// its next section is sent again as an update (`dvbca_add_pid`).
    fn add_pid(&mut self, _service: &CaService, _pid: i32) -> bool {
        false
    }
    /// Removes a PID from the CA.
    fn del_pid(&mut self, _service: &CaService, _pid: i32) {}
    /// Whether this CA accepts the service's CAIDs (no filter means yes).
    fn accepts_service(&self, _service: &CaService) -> bool {
        true
    }
}

/// PMT slots, filter slots and registered CAs.
pub struct PmtTables {
    pub(crate) pmts: Vec<Pmt>,
    pub(crate) filters: Vec<Filter>,
    pub(crate) ca: Vec<Option<Box<dyn CaSink>>>,
    pub(crate) emulate_pids_all: bool,
    pub(crate) pmt_scan: bool,
    /// `ad->drop_encrypted` (from `opts`): no null pid in the pids=all emulation.
    pub(crate) drop_encrypted: bool,
}

impl Default for PmtTables {
    fn default() -> Self {
        Self::new()
    }
}

impl PmtTables {
    /// Creates empty PMT/filter tables.
    pub fn new() -> Self {
        Self {
            pmts: (0..MAX_PMT).map(Pmt::new).collect(),
            filters: (0..MAX_FILTERS).map(Filter::new).collect(),
            ca: Vec::new(),
            emulate_pids_all: false,
            pmt_scan: false,
            drop_encrypted: false,
        }
    }

    /// Enables `pids=all` expansion for the next `update_pids` passes.
    pub fn set_emulate_pids_all(&mut self, enabled: bool) {
        self.emulate_pids_all = enabled;
    }

    /// Creates PMT filters with `FILTER_ADD_REMOVE` (`--pmt-scan`).
    pub fn set_pmt_scan(&mut self, enabled: bool) {
        self.pmt_scan = enabled;
    }
}

impl Scheduler {
    fn adapter_type(&self, aid: i32) -> AdapterType {
        usize::try_from(aid)
            .ok()
            .and_then(|index| self.adapter(index))
            .map_or(AdapterType::Unknown, |adapter| adapter.config.adapter_type)
    }

    fn adapter_index(&self, aid: i32) -> Option<usize> {
        let index = usize::try_from(aid).ok()?;
        self.adapter(index)?;
        Some(index)
    }

    /// PMT with this id, only while enabled (`get_pmt`).
    pub fn pmt(&self, id: i32) -> Option<&Pmt> {
        let index = usize::try_from(id).ok()?;
        self.pmt.pmts.get(index).filter(|pmt| pmt.enabled)
    }

    /// Raw PMT slot, including disabled slots (`pmts[]`).
    pub fn pmt_slot(&self, id: i32) -> Option<&Pmt> {
        let index = usize::try_from(id).ok()?;
        self.pmt.pmts.get(index)
    }

    /// Mutable raw PMT slot.
    pub fn pmt_slot_mut(&mut self, id: i32) -> Option<&mut Pmt> {
        let index = usize::try_from(id).ok()?;
        self.pmt.pmts.get_mut(index)
    }

    /// Raw filter slot.
    pub fn filter_slot(&self, id: i32) -> Option<&Filter> {
        let index = usize::try_from(id).ok()?;
        self.pmt.filters.get(index)
    }

    /// Allocates a PMT slot (`pmt_add`).
    pub fn pmt_add(&mut self, adapter: i32, sid: i32, pmt_pid: i32) -> i32 {
        let Some(slot) = self.pmt.pmts.iter().position(|pmt| !pmt.allocated) else {
            ms_log::log_and_return!(-1, "PMT buffer is full, could not add new pmts");
        };
        let id = slot as i32;
        let pmt = &mut self.pmt.pmts[slot];
        pmt.enabled = true;
        pmt.allocated = true;
        pmt.sid = sid;
        pmt.pid = pmt_pid;
        pmt.adapter = adapter;
        pmt.pcr_pid = 0;
        pmt.version = -1;
        pmt.filter = -1;
        pmt.state = PmtState::Stopped;
        pmt.ca_mask = 0;
        pmt.disabled_ca_mask = 0;
        pmt.ca_registered_mask = 0;
        pmt.best = false;
        pmt.name.clear();
        pmt.provider.clear();
        pmt.sdt = None;
        pmt.caids = 0;
        pmt.ca.clear();
        pmt.descriptors.clear();
        pmt.stream_pids.clear();
        ms_log::log!(
            "returning new pmt {} for adapter {}, pmt pid {}, sid {} {:04X}",
            id,
            adapter,
            pmt_pid,
            sid,
            sid
        );
        id
    }

    /// Deletes a PMT and closes its CA registrations (`pmt_del`).
    pub fn pmt_del(&mut self, id: i32) {
        let Some(pmt) = self.pmt(id) else {
            return;
        };
        ms_log::log!("deleting PMT {}, name {} ", pmt.id, pmt.name);
        let adapter = pmt.adapter;
        self.close_pmt_for_cas(adapter, id);
        let (allocated, filter) = {
            let Some(pmt) = self.pmt_slot_mut(id) else {
                return;
            };
            if !pmt.enabled {
                return;
            }
            pmt.enabled = false;
            pmt.allocated = false;
            pmt.sid = 0;
            pmt.pid = 0;
            pmt.adapter = -1;
            pmt.caids = 0;
            pmt.ca.clear();
            pmt.descriptors.clear();
            pmt.stream_pids.clear();
            (pmt.allocated, pmt.filter)
        };
        let _ = allocated;
        self.release_pmt_claims(adapter, id);
        if filter >= 0 {
            self.del_filter(filter);
        }
        if let Some(pmt) = self.pmt_slot_mut(id) {
            pmt.filter = -1;
        }
    }

    /// The adapter's active PMT ids (`ad->active_pmt[0..active_pmts]`).
    pub fn active_pmt_ids(&self, aid: usize) -> &[i32] {
        self.adapter(aid).map_or(&[][..], |adapter| {
            &adapter.active_pmt[..adapter.active_pmts]
        })
    }

    /// Adds a PMT to the adapter's active list (`pmt_add_active_pmt`).
    pub fn pmt_add_active_pmt(&mut self, aid: i32, pmt_id: i32) {
        let Some(pmt) = self.pmt(pmt_id) else {
            return;
        };
        let revive = pmt.state == PmtState::Cached;
        let Some(adapter) = usize::try_from(aid)
            .ok()
            .and_then(|index| self.adapter_mut(index))
        else {
            return;
        };
        if adapter.active_pmt[..adapter.active_pmts].contains(&pmt_id) {
            return;
        }
        if adapter.active_pmts >= crate::MAX_PMT_FOR_ADAPTER {
            ms_log::log!(
                "adapter {} active PMT list full, dropping PMT {}",
                aid,
                pmt_id
            );
            return;
        }
        adapter.active_pmt[adapter.active_pmts] = pmt_id;
        adapter.active_pmts += 1;
        if revive && let Some(pmt) = self.pmt_slot_mut(pmt_id) {
            pmt.state = PmtState::Stopped;
        }
    }

    /// PMT for `(sid, pid)` on `aid`, enabled or not (`get_pmt_for_sid_pid`).
    pub fn get_pmt_for_sid_pid(&self, aid: i32, sid: i32, pid: i32) -> Option<&Pmt> {
        self.pmt
            .pmts
            .iter()
            .find(|pmt| pmt.enabled && pmt.adapter == aid && pmt.sid == sid && pmt.pid == pid)
    }

    /// PMT for `sid` on `aid`, scanning every slot (`get_all_pmt_for_sid`).
    pub fn get_all_pmt_for_sid(&self, aid: i32, sid: i32) -> Option<&Pmt> {
        self.pmt
            .pmts
            .iter()
            .find(|pmt| pmt.enabled && pmt.adapter == aid && pmt.sid == sid)
    }

    /// PMT for `sid` in the active list (`get_pmt_for_sid`).
    pub fn get_pmt_for_sid(&self, aid: i32, sid: i32) -> Option<&Pmt> {
        let adapter = usize::try_from(aid).ok().and_then(|i| self.adapter(i))?;
        adapter.active_pmt[..adapter.active_pmts]
            .iter()
            .filter_map(|id| self.pmt(*id))
            .find(|pmt| pmt.sid == sid)
    }

    /// Adds/reuses a CA descriptor entry (`pmt_add_caid`).
    pub fn pmt_add_caid(&mut self, pmt_id: i32, caid: u16, capid: u16, data: &[u8]) {
        let Some(pmt) = self.pmt_slot_mut(pmt_id) else {
            return;
        };
        if !pmt.enabled {
            return;
        }
        if pmt.ca[..pmt.caids]
            .iter()
            .any(|ca| ca.id == caid && ca.pid == capid)
        {
            ms_log::logm!(
                "pmt_add_caid: CAID {:04X} CAPID {} already exists in PMT {}",
                caid,
                capid,
                pmt_id
            );
            return;
        }
        if pmt.caids >= MAX_CAID {
            ms_log::log!("Too many CAIDs for pmt {}, discarding {:04X}", pmt_id, caid);
            return;
        }
        ms_log::log!(
            "PMT {} PI pos {} caid {:04X} => pid {:04X} ({}), index {}",
            pmt_id,
            pmt.caids + 1,
            caid,
            capid,
            capid,
            pmt.caids
        );
        if pmt.ca.len() <= pmt.caids {
            pmt.ca.push(PmtCa::default());
        }
        let entry = &mut pmt.ca[pmt.caids];
        entry.id = caid;
        entry.pid = capid;
        entry.private_data.clear();
        entry.private_data.extend_from_slice(data);
        pmt.caids += 1;
        pmt.ca_mask = 0;
        pmt.disabled_ca_mask = 0;
    }

    /// Appends a PMT (`pmt_add_stream_pid`).
    pub fn pmt_add_stream_pid(
        &mut self,
        pmt_id: i32,
        pid: i32,
        stream_type: i32,
        is_audio: bool,
        is_video: bool,
    ) -> i32 {
        let Some(pmt) = self.pmt_slot_mut(pmt_id) else {
            return -1;
        };
        if !pmt.enabled {
            return -1;
        }
        pmt.stream_pids.push(StreamPid {
            stream_type,
            pid,
            is_audio,
            is_video,
            descriptors: Vec::new(),
        });
        pmt.stream_pids.len() as i32 - 1
    }

    /// Adds descriptor bytes to a PMT, deduplicating and registering CA
    /// descriptors (`pmt_add_descriptors`).
    pub fn pmt_add_descriptors(&mut self, pmt_id: i32, bytes: &[u8]) {
        for descriptor in ms_psi::iter_descriptors(bytes).flatten() {
            let entry = Descriptor {
                tag: descriptor.tag,
                data: descriptor.data,
            };
            let duplicate = self
                .pmt_slot(pmt_id)
                .is_some_and(|pmt| pmt.descriptors.contains(&entry));
            if duplicate {
                ms_log::logm!(
                    "PMT {} already has descriptor with type {}, len {}",
                    pmt_id,
                    entry.tag,
                    entry.data.len()
                );
                continue;
            }
            if let Some(pmt) = self.pmt_slot_mut(pmt_id) {
                pmt.descriptors.push(entry.clone());
            }
            self.add_ca_from_descriptor(pmt_id, &entry);
        }
    }

    /// Adds descriptor bytes to one stream (`pmt_add_stream_pid_descriptors`).
    pub fn pmt_add_stream_pid_descriptors(&mut self, pmt_id: i32, pid: i32, bytes: &[u8]) {
        for descriptor in ms_psi::iter_descriptors(bytes).flatten() {
            let entry = Descriptor {
                tag: descriptor.tag,
                data: descriptor.data,
            };
            let duplicate = self
                .pmt_slot(pmt_id)
                .and_then(|pmt| pmt.stream_pids.iter().find(|stream| stream.pid == pid))
                .is_some_and(|stream| stream.descriptors.contains(&entry));
            if duplicate {
                ms_log::logm!(
                    "PMT {} pid {} already has descriptor with type {}, len {}",
                    pmt_id,
                    pid,
                    entry.tag,
                    entry.data.len()
                );
                continue;
            }
            if let Some(stream) = self
                .pmt_slot_mut(pmt_id)
                .and_then(|pmt| pmt.stream_pids.iter_mut().find(|stream| stream.pid == pid))
            {
                stream.descriptors.push(entry.clone());
            }
            self.add_ca_from_descriptor(pmt_id, &entry);
        }
    }

    fn add_ca_from_descriptor(&mut self, pmt_id: i32, descriptor: &Descriptor) {
        if descriptor.tag != 0x09 {
            return;
        }
        let Some(caid) = descriptor.ca_system_id() else {
            return;
        };
        let Some(capid) = descriptor.ca_pid() else {
            return;
        };
        let private = descriptor.data.get(4..).unwrap_or(&[]).to_vec();
        self.pmt_add_caid(pmt_id, caid, capid, &private);
    }

    /// Releases the PID claims of exactly this PMT (`release_pmt_claims`).
    pub(crate) fn release_pmt_claims(&mut self, aid: i32, pmt_id: i32) {
        let Some(adapter) = usize::try_from(aid)
            .ok()
            .and_then(|index| self.adapter_mut(index))
        else {
            return;
        };
        for entry in &mut adapter.pids {
            if entry.pmt == pmt_id {
                entry.pmt = -1;
            }
        }
    }

    fn pid_subscribed(&self, aid: i32, entry: &PidEntry) -> bool {
        let adapter_type = self.adapter_type(aid);
        (adapter_type == AdapterType::Ci && entry.sids.contains(&DDCI_SID))
            || entry.sids.iter().any(|sid| *sid >= 0 && *sid != DDCI_SID)
    }

    fn live_av(&self, pmt_id: i32) -> bool {
        let Some(pmt) = self.pmt(pmt_id) else {
            return false;
        };
        let aid = pmt.adapter;
        let streams: Vec<(i32, bool)> = pmt
            .stream_pids
            .iter()
            .map(|stream| (stream.pid, stream.is_audio || stream.is_video))
            .collect();
        streams.into_iter().any(|(pid, is_av)| {
            if !is_av {
                return false;
            }
            self.find_pid(aid, pid).is_some_and(|entry| {
                matches!(entry.state, PidState::Active | PidState::New)
                    && self.pid_subscribed(aid, entry)
            })
        })
    }

    fn start_candidate(&self, pmt_id: i32) -> bool {
        let Some(pmt) = self.pmt(pmt_id) else {
            return false;
        };
        let aid = pmt.adapter;
        if pmt.state != PmtState::Running
            && !self.find_pid(aid, pmt.pid).is_some_and(|entry| {
                entry.state == PidState::Active && self.pid_subscribed(aid, entry)
            })
        {
            return false;
        }
        let streams: Vec<(i32, bool)> = pmt
            .stream_pids
            .iter()
            .map(|stream| (stream.pid, stream.is_audio || stream.is_video))
            .collect();
        streams.into_iter().any(|(pid, is_av)| {
            is_av
                && self.find_pid(aid, pid).is_some_and(|entry| {
                    entry.state == PidState::Active && self.pid_subscribed(aid, entry)
                })
        })
    }

    fn holds_av_claim(&self, pmt_id: i32) -> bool {
        let Some(pmt) = self.pmt(pmt_id) else {
            return false;
        };
        let aid = pmt.adapter;
        let streams: Vec<(i32, bool)> = pmt
            .stream_pids
            .iter()
            .map(|stream| (stream.pid, stream.is_audio || stream.is_video))
            .collect();
        streams.into_iter().any(|(pid, is_av)| {
            is_av
                && self.find_pid(aid, pid).is_some_and(|entry| {
                    entry.state == PidState::Active
                        && entry.pmt == pmt_id
                        && self.pid_subscribed(aid, entry)
                })
        })
    }

    fn same_descriptors(a: &[Descriptor], b: &[Descriptor]) -> bool {
        if a.len() != b.len() {
            return false;
        }
        let mut used = vec![false; b.len()];
        for da in a {
            let Some(index) = b
                .iter()
                .enumerate()
                .find(|(index, db)| !used[*index] && da == *db)
                .map(|(index, _)| index)
            else {
                return false;
            };
            used[index] = true;
        }
        true
    }

    fn same_stream_pid(a: &StreamPid, b: &StreamPid) -> bool {
        a.pid == b.pid
            && a.stream_type == b.stream_type
            && a.is_audio == b.is_audio
            && a.is_video == b.is_video
            && Self::same_descriptors(&a.descriptors, &b.descriptors)
    }

    fn same_stream_pids(&self, a: i32, b: i32) -> bool {
        let (Some(a), Some(b)) = (self.pmt(a), self.pmt(b)) else {
            return false;
        };
        if a.stream_pids.len() != b.stream_pids.len()
            || !Self::same_descriptors(&a.descriptors, &b.descriptors)
        {
            return false;
        }
        let mut used = vec![false; b.stream_pids.len()];
        for sa in &a.stream_pids {
            let Some(index) = b
                .stream_pids
                .iter()
                .enumerate()
                .find(|(index, sb)| !used[*index] && Self::same_stream_pid(sa, sb))
                .map(|(index, _)| index)
            else {
                return false;
            };
            used[index] = true;
        }
        true
    }

    fn pmt_subscribed(&self, pmt_id: i32) -> bool {
        let Some(pmt) = self.pmt(pmt_id) else {
            return false;
        };
        let aid = pmt.adapter;
        let own_ok = self.find_pid(aid, pmt.pid).is_some_and(|entry| {
            entry.state == PidState::Active && self.pid_subscribed(aid, entry)
        });
        if !own_ok {
            return false;
        }
        pmt.stream_pids
            .iter()
            .filter(|stream| stream.is_audio || stream.is_video)
            .any(|stream| {
                self.find_pid(aid, stream.pid).is_some_and(|entry| {
                    entry.state == PidState::Active && self.pid_subscribed(aid, entry)
                })
            })
    }

    fn handover_claims_from_lower_priority(&mut self, pmt_id: i32) {
        let Some(pmt) = self.pmt(pmt_id) else {
            return;
        };
        let aid = pmt.adapter;
        let order = self.find_pid(aid, pmt.pid).map_or(u32::MAX, |entry| {
            if entry.order == 0 {
                u32::MAX
            } else {
                entry.order
            }
        });
        let streams: Vec<(i32, bool)> = pmt
            .stream_pids
            .iter()
            .map(|stream| (stream.pid, stream.is_audio || stream.is_video))
            .collect();
        for (pid, is_av) in streams {
            if !is_av {
                continue;
            }
            let Some(entry) = self.find_pid(aid, pid) else {
                continue;
            };
            if entry.state != PidState::Active || entry.pmt < 0 || entry.pmt == pmt_id {
                continue;
            }
            let old_id = entry.pmt;
            let Some(old) = self.pmt(old_id) else {
                continue;
            };
            let old_order = self
                .find_pid(aid, old.pid)
                .map_or(u32::MAX, |entry| entry.order);
            if order >= old_order {
                continue;
            }
            if !matches!(old.state, PmtState::Running | PmtState::Starting) {
                continue;
            }
            ms_log::log!(
                "PMT {} takes over from lower-priority PMT {}",
                pmt_id,
                old_id
            );
            self.stop_pmt(old_id);
            self.release_pmt_claims(aid, old_id);
        }
    }

    /// Earliest subscriber wins each PID; a running PMT without its own PID
    /// sorts last. Never sends CAPMTs (`pmt_pid_updated_pids`).
    pub(crate) fn pmt_pid_updated_pids(&mut self, index: usize) {
        let aid = index as i32;
        let active: Vec<i32> = match self.adapters.get(index).and_then(Option::as_ref) {
            Some(adapter) => adapter.active_pmt[..adapter.active_pmts].to_vec(),
            None => return,
        };
        for id in &active {
            if let Some(pmt) = self.pmt_slot_mut(*id) {
                pmt.best = false;
            }
        }

        let mut choices: Vec<(u32, i32, i32)> = Vec::new();
        for id in active.iter().copied() {
            if !self.start_candidate(id) {
                continue;
            }
            let order = self
                .pmt(id)
                .and_then(|pmt| self.find_pid(aid, pmt.pid))
                .filter(|entry| entry.state == PidState::Active)
                .map_or(u32::MAX, |entry| {
                    if entry.order == 0 {
                        u32::MAX
                    } else {
                        entry.order
                    }
                });
            let sid = self.pmt(id).map_or(0, |pmt| pmt.sid);
            choices.push((order, sid, id));
        }
        choices.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));

        let mut alive = vec![true; choices.len()];
        for i in 0..choices.len() {
            if !alive[i] {
                continue;
            }
            let id = choices[i].2;
            if let Some(pmt) = self.pmt_slot_mut(id) {
                pmt.best = true;
            }
            for j in i + 1..choices.len() {
                if alive[j] && self.same_stream_pids(id, choices[j].2) {
                    alive[j] = false;
                    if let Some(pmt) = self.pmt_slot_mut(choices[j].2) {
                        pmt.best = false;
                    }
                }
            }
        }

        for id in active.iter().copied() {
            let Some(pmt) = self.pmt(id) else {
                continue;
            };
            if !matches!(pmt.state, PmtState::Running | PmtState::Starting) {
                continue;
            }
            if !pmt.best {
                ms_log::log!("Stopping started PMT {}: {}", id, pmt.name);
                self.stop_pmt(id);
                self.release_pmt_claims(aid, id);
            }
        }

        for (index_in_choices, (order, _, id)) in choices.iter().copied().enumerate() {
            if !alive[index_in_choices] {
                continue;
            }
            let Some(pmt) = self.pmt(id) else {
                continue;
            };
            let streams: Vec<i32> = pmt.stream_pids.iter().map(|stream| stream.pid).collect();
            let state = pmt.state;
            for pid in streams {
                let Some(entry) = self.find_pid(aid, pid) else {
                    continue;
                };
                if entry.state != PidState::Active
                    || !self.pid_subscribed(aid, entry)
                    || entry.pmt == id
                {
                    continue;
                }
                if entry.pmt >= 0 {
                    let holder_id = entry.pmt;
                    let holder_order = self
                        .pmt(entry.pmt)
                        .and_then(|holder| self.find_pid(aid, holder.pid))
                        .filter(|entry| entry.state == PidState::Active)
                        .map_or(u32::MAX, |entry| entry.order);
                    if order >= holder_order {
                        continue;
                    }
                    ms_log::logm!("PMT {} steals pid {} from PMT {}", id, pid, holder_id);
                }
                if let Some(entry) = self.find_pid_mut(aid, pid) {
                    entry.pmt = id;
                    entry.is_decrypted = false;
                }
                ms_log::logm!("PMT {} claimed pid {}", id, pid);
                if state != PmtState::Stopped {
                    self.tables_add_pid(id, pid);
                }
            }
            if !self.holds_av_claim(id) {
                if matches!(state, PmtState::Running | PmtState::Starting) {
                    self.stop_pmt(id);
                    self.release_pmt_claims(aid, id);
                }
                continue;
            }
            if state == PmtState::Stopped {
                self.start_pmt(id);
            }
            if let Some(pmt) = self.pmt_slot_mut(id)
                && pmt.state == PmtState::Starting
            {
                pmt.state = PmtState::Running;
            }
        }
    }

    /// Re-elects and sends pending CAPMTs (`start_active_pmts`).
    pub fn start_active_pmts(&mut self, aid: i32) {
        let Some(index) = self.adapter_index(aid) else {
            return;
        };
        self.pmt_pid_updated_pids(index);
        let active: Vec<i32> = match self.adapter(index) {
            Some(adapter) => adapter.active_pmt[..adapter.active_pmts].to_vec(),
            None => return,
        };
        for id in active {
            let Some(pmt) = self.pmt(id) else {
                continue;
            };
            if !matches!(pmt.state, PmtState::Running | PmtState::Starting) {
                continue;
            }
            let resend = self
                .adapter(index)
                .is_some_and(|adapter| adapter.ca_mask != (pmt.disabled_ca_mask | pmt.ca_mask));
            if resend {
                self.send_pmt_to_cas(aid, id);
            }
        }
    }

    /// Starts a PMT and arms its section filter (`start_pmt`).
    pub fn start_pmt(&mut self, id: i32) {
        let (filter, adapter) = match self.pmt(id) {
            Some(pmt) => {
                ms_log::logm!(
                    "starting PMT {}, pid {}, sid {}, filter {} for channel: {}",
                    pmt.id,
                    pmt.pid,
                    pmt.sid,
                    pmt.filter,
                    pmt.name
                );
                (pmt.filter, pmt.adapter)
            }
            None => return,
        };
        if let Some(pmt) = self.pmt_slot_mut(id) {
            pmt.state = PmtState::Starting;
        }
        if filter >= 0 {
            self.set_filter_flags(filter, FILTER_CRC);
        }
        let _ = adapter;
    }

    /// Stops a PMT and closes its CA registrations (`stop_pmt`).
    pub fn stop_pmt(&mut self, id: i32) {
        let (filter, adapter) = match self.pmt(id) {
            Some(pmt) => {
                if pmt.state == PmtState::Stopped {
                    return;
                }
                ms_log::logm!(
                    "stopping PMT {} pid {} sid {} filter {} for channel {}",
                    pmt.id,
                    pmt.pid,
                    pmt.sid,
                    pmt.filter,
                    pmt.name
                );
                (pmt.filter, pmt.adapter)
            }
            None => return,
        };
        if let Some(pmt) = self.pmt_slot_mut(id) {
            pmt.state = PmtState::Stopping;
        }
        if filter >= 0 {
            self.set_filter_flags(filter, 0);
        }
        self.close_pmt_for_cas(adapter, id);
        if let Some(pmt) = self.pmt_slot_mut(id) {
            pmt.state = PmtState::Stopped;
        }
    }

    /// Tells every registered CA to drop this PMT (`pmt_pid_del`/stop path).
    pub(crate) fn pmt_pid_del(&mut self, index: usize, pid: i32) {
        let aid = index as i32;
        if self.find_pid(aid, pid).is_none() {
            return;
        }
        let active: Vec<i32> = match self.adapter(index) {
            Some(adapter) => adapter.active_pmt[..adapter.active_pmts].to_vec(),
            None => return,
        };
        for id in active {
            let Some(own) = self.pmt(id) else {
                continue;
            };
            if own.pid != pid || !matches!(own.state, PmtState::Running | PmtState::Starting) {
                continue;
            }
            if self.live_av(id) {
                ms_log::log!("PMT {} keeps running without pid {}", id, pid);
                continue;
            }
            self.stop_pmt(id);
            self.release_pmt_claims(aid, id);
        }
        let pmt_id = match self.find_pid(aid, pid) {
            Some(entry) => entry.pmt,
            None => return,
        };
        if pmt_id < 0 {
            return;
        }
        let (pmt_pid, streams): (i32, Vec<i32>) = match self.pmt(pmt_id) {
            Some(pmt) => {
                ms_log::logm!(
                    "pmt_pid_del: pid {} adapter {} pmt {}, channel {}",
                    pid,
                    aid,
                    pmt_id,
                    pmt.name
                );
                (
                    pmt.pid,
                    pmt.stream_pids.iter().map(|stream| stream.pid).collect(),
                )
            }
            None => return,
        };
        self.tables_del_pid(pmt_id, pid);
        let mut other_live = false;
        for stream_pid in streams {
            if stream_pid != pid
                && self
                    .find_pid(aid, stream_pid)
                    .is_some_and(|entry| matches!(entry.state, PidState::Active | PidState::New))
            {
                ms_log::logm!(
                    "found active pid {} for pmt id {}, pid {}",
                    stream_pid,
                    pmt_id,
                    pmt_pid
                );
                other_live = true;
            }
        }
        if !other_live {
            self.stop_pmt(pmt_id);
            self.release_pmt_claims(aid, pmt_id);
        }
    }

    /// Expands each `(PMT, stream)` PID for every subscription of PID 8192
    /// (`emulate_add_all_pids`).
    pub(crate) fn emulate_add_all_pids(&mut self, index: usize) {
        if !self.pmt.emulate_pids_all {
            return;
        }
        let aid = index as i32;
        let Some(p_all) = self.find_pid(aid, crate::PID_ALL) else {
            return;
        };
        let sids: Vec<StreamId> = p_all.sids.iter().copied().collect();
        let mut existing: BTreeSet<(i32, StreamId)> = BTreeSet::new();
        if let Some(adapter) = self.adapter(index) {
            for entry in &adapter.pids {
                if matches!(entry.state, PidState::Active | PidState::New) {
                    for sid in &entry.sids {
                        existing.insert((entry.pid, *sid));
                    }
                }
            }
        }
        let active: Vec<i32> = match self.adapter(index) {
            Some(adapter) => adapter.active_pmt[..adapter.active_pmts].to_vec(),
            None => Vec::new(),
        };
        for sid in sids {
            for id in &active {
                let Some(pmt) = self.pmt(*id) else {
                    continue;
                };
                let pmt_pid = pmt.pid;
                let streams: Vec<i32> = pmt.stream_pids.iter().map(|stream| stream.pid).collect();
                if !existing.contains(&(pmt_pid, sid)) {
                    self.mark_pid_add(aid, sid, pmt_pid);
                }
                for stream in streams {
                    if existing.contains(&(stream, sid)) {
                        continue;
                    }
                    ms_log::log!(
                        "emulate_add_all_pids: adding pid {} sid {} to emulate all pids",
                        stream,
                        sid
                    );
                    self.mark_pid_add(aid, sid, stream);
                }
            }
            for enforced in [0, 1, 16, 17, 18, 20, 21] {
                if !existing.contains(&(enforced, sid)) {
                    ms_log::log!(
                        "emulate_add_all_pids: adding (enforced) pid {} sid {} to emulate all pids",
                        enforced,
                        sid
                    );
                    self.mark_pid_add(aid, sid, enforced);
                }
            }
            if !self.pmt.drop_encrypted {
                ms_log::log!("emulate_add_all_pids: adding (enforced) pid 8191 (NULL) too");
                if !existing.contains(&(8191, sid)) {
                    self.mark_pid_add(aid, sid, 8191);
                }
            }
        }
    }

    /// Registers a CA sink (`add_ca`) and returns its id.
    pub fn add_ca(&mut self, sink: Box<dyn CaSink>) -> i32 {
        if let Some(slot) = self.pmt.ca.iter().position(Option::is_none) {
            self.pmt.ca[slot] = Some(sink);
            return slot as i32;
        }
        let id = self.pmt.ca.len() as i32;
        self.pmt.ca.push(Some(sink));
        id
    }

    /// Unregisters a CA and clears its masks (`del_ca`).
    pub fn del_ca(&mut self, id: i32) {
        let Ok(slot) = usize::try_from(id) else {
            return;
        };
        if slot >= self.pmt.ca.len() {
            return;
        }
        self.pmt.ca[slot] = None;
        let mask = 1u64 << slot;
        for adapter in self.adapters.iter_mut().flatten() {
            adapter.ca_mask &= !mask;
        }
        for pmt in &mut self.pmt.pmts {
            if pmt.enabled {
                pmt.ca_mask &= !mask;
                pmt.ca_registered_mask &= !mask;
            }
        }
    }

    /// Lets CA `ca` handle the adapter once its `ca_init_dev` accepted it
    /// (`tables_init_ca_for_device`).
    pub fn init_ca_for_adapter(&mut self, aid: usize, ca: i32) {
        let registered = usize::try_from(ca)
            .ok()
            .and_then(|slot| self.pmt.ca.get(slot))
            .is_some_and(Option::is_some);
        if let Some(adapter) = self.adapter_mut(aid)
            && registered
            && adapter.ca_mask & (1u64 << ca) == 0
        {
            ms_log::logl!(ms_log::LOG_TABLES, "CA {} will handle adapter {}", ca, aid);
            adapter.ca_mask |= 1u64 << ca;
        }
    }

    /// Clears every CA of the adapter (`tables_close_device`).
    pub fn close_ca_for_adapter(&mut self, aid: usize) {
        if let Some(adapter) = self.adapter_mut(aid) {
            adapter.ca_mask = 0;
        }
    }

    /// CAs handling the adapter (`ad->ca_mask`).
    pub fn adapter_ca_mask(&self, aid: usize) -> u64 {
        self.adapter(aid).map_or(0, |adapter| adapter.ca_mask)
    }

    /// Clears one CA's bits of a PMT so it is offered again
    /// (`tables_clear_pmt_ca_masks`, used by `ca_release_pmts`).
    pub fn clear_pmt_ca_masks(&mut self, pmt_id: i32, ca: i32, clear_disabled: bool) {
        let Ok(slot) = u32::try_from(ca) else {
            return;
        };
        let mask = 1u64.checked_shl(slot).unwrap_or(0);
        if let Some(pmt) = self.pmt_slot_mut(pmt_id)
            && pmt.enabled
        {
            pmt.ca_mask &= !mask;
            pmt.ca_registered_mask &= !mask;
            if clear_disabled {
                pmt.disabled_ca_mask &= !mask;
            }
        }
    }

    /// Drops the PMT's registration with one CA so the next
    /// `start_active_pmts` offers it again (`close_pmt_for_ca`).
    pub fn restart_pmt_for_ca(&mut self, ca: i32, pmt_id: i32) {
        let Some(aid) = self.pmt(pmt_id).map(|pmt| pmt.adapter) else {
            return;
        };
        self.close_pmt_for_ca(ca, aid, pmt_id);
    }

    /// Arms the PAT/SDT filters and drops the previous mux's PMTs (`pmt_tune`).
    pub fn pmt_tune(&mut self, aid: i32) {
        let Some(index) = self.adapter_index(aid) else {
            return;
        };
        let (pat, sdt) = match self.adapters[index].as_ref() {
            Some(adapter) => (adapter.pat_filter, adapter.sdt_filter),
            None => return,
        };
        if pat == -1 {
            let id = self.add_filter(aid, 0, FilterTarget::Pat, FILTER_PERMANENT | FILTER_CRC);
            if let Some(adapter) = self.adapters[index].as_mut() {
                adapter.pat_filter = id;
            }
        }
        if sdt == -1 {
            let id = self.add_filter(aid, 17, FilterTarget::Sdt, FILTER_PERMANENT | FILTER_CRC);
            if let Some(adapter) = self.adapters[index].as_mut() {
                adapter.sdt_filter = id;
            }
        }
        self.clear_pmt_for_adapter(aid);
    }

    /// PIDs of the adapter's enabled filters, ascending.
    pub fn section_pids(&self, aid: i32) -> Vec<u16> {
        let pids: BTreeSet<u16> = self
            .pmt
            .filters
            .iter()
            .filter(|filter| filter.enabled && filter.adapter == aid)
            .filter_map(|filter| u16::try_from(filter.pid).ok())
            .collect();
        pids.into_iter().collect()
    }

    /// Enabled filters of one adapter PID in slot order (the `next_filter` chain).
    pub fn filters_for_pid(&self, aid: i32, pid: i32) -> Vec<i32> {
        self.pmt
            .filters
            .iter()
            .filter(|filter| filter.enabled && filter.adapter == aid && filter.pid == pid)
            .map(|filter| filter.id as i32)
            .collect()
    }

    /// Offers the PMT to every enabled CA (`send_pmt_to_cas`).
    pub fn send_pmt_to_cas(&mut self, aid: i32, pmt_id: i32) {
        let Some(pmt) = self.pmt(pmt_id) else {
            return;
        };
        if pmt.caids == 0 {
            return;
        }
        let (sent, disabled) = (pmt.ca_mask, pmt.disabled_ca_mask);
        let ad_ca = self
            .adapter_index(aid)
            .and_then(|index| self.adapters[index].as_ref())
            .map_or(u32::MAX - 1, |adapter| adapter.ca_mask as u32);
        ms_log::log!(
            "Sending PMT {} to all CAs: ad_ca_mask {:X}, pmt_ca_mask {:X}, disabled_ca_mask {:X}",
            pmt_id,
            ad_ca,
            sent,
            disabled
        );
        for slot in 0..self.pmt.ca.len() {
            self.send_pmt_to_ca(slot as i32, aid, pmt_id);
        }
    }

    fn send_pmt_to_ca(&mut self, slot: i32, aid: i32, pmt_id: i32) {
        let mask = 1u64 << slot;
        let Some(adapter_index) = self.adapter_index(aid) else {
            return;
        };
        let Some(service) = self.ca_service(pmt_id) else {
            return;
        };
        let (adapter_mask, disabled, sent, registered) = {
            let Some(pmt) = self.pmt(pmt_id) else {
                return;
            };
            let adapter_mask = self.adapters[adapter_index]
                .as_ref()
                .map_or(0, |adapter| adapter.ca_mask);
            (
                adapter_mask,
                pmt.disabled_ca_mask,
                pmt.ca_mask,
                pmt.ca_registered_mask,
            )
        };
        if adapter_mask & mask == 0 || disabled & mask != 0 || sent & mask != 0 {
            return;
        }
        let Some(sink) = self.pmt.ca.get_mut(slot as usize).and_then(Option::as_mut) else {
            return;
        };
        let update = registered & mask != 0;
        let accepted = sink.accepts_service(&service);
        if accepted {
            ms_log::logl!(
                ms_log::LOG_TABLES,
                "PMT {} -> CA {}: {}",
                pmt_id,
                slot,
                if update { "update" } else { "add" }
            );
        }
        let result = if accepted {
            sink.add_pmt(&service, update)
        } else {
            TablesResult::NoRetry
        };
        let Some(pmt) = self.pmt_slot_mut(pmt_id) else {
            return;
        };
        if pmt.disabled_ca_mask & mask != 0 || pmt.ca_mask & mask != 0 {
            return;
        }
        let mut stale = false;
        match result {
            TablesResult::Ok => {
                pmt.ca_mask |= mask;
                pmt.ca_registered_mask |= mask;
            }
            TablesResult::NoRetry => {
                pmt.disabled_ca_mask |= mask;
                stale = pmt.ca_registered_mask & mask != 0;
            }
            TablesResult::Retry => {}
        }
        if stale {
            self.close_pmt_for_ca(slot, aid, pmt_id);
        }
        if accepted {
            let result_code = match result {
                TablesResult::Ok => 0,
                TablesResult::Retry => 1,
                TablesResult::NoRetry => 2,
            };
            if let Some(pmt) = self.pmt(pmt_id) {
                ms_log::logl!(
                    ms_log::LOG_TABLES,
                    "In processing PMT {}, ca {}, CA matched {}, update {}, ca_pmt_add returned {}, new ca_mask {} new disabled_ca_mask {}",
                    pmt_id,
                    slot,
                    1,
                    u8::from(update),
                    result_code,
                    pmt.ca_mask,
                    pmt.disabled_ca_mask
                );
            }
        }
    }

    fn close_pmt_for_ca(&mut self, slot: i32, aid: i32, pmt_id: i32) {
        let mask = 1u64 << slot;
        let Some(service) = self.ca_service(pmt_id) else {
            return;
        };
        let Some(adapter) = self
            .adapter_index(aid)
            .and_then(|i| self.adapters[i].as_ref())
        else {
            return;
        };
        let registered = self
            .pmt(pmt_id)
            .is_some_and(|pmt| pmt.ca_registered_mask & mask != 0);
        if adapter.ca_mask & mask == 0 || !registered {
            return;
        }
        ms_log::logl!(
            ms_log::LOG_TABLES,
            "Closing pmt {} for ca {} and adapter {}",
            pmt_id,
            slot,
            aid
        );
        let Some(Some(sink)) = self.pmt.ca.get_mut(slot as usize) else {
            return;
        };
        let _ = sink.del_pmt(&service);
        if let Some(pmt) = self.pmt_slot_mut(pmt_id)
            && pmt.ca_registered_mask & mask != 0
        {
            pmt.ca_mask &= !mask;
            pmt.ca_registered_mask &= !mask;
        }
    }

    fn close_pmt_for_cas(&mut self, aid: i32, pmt_id: i32) {
        let registered = self
            .pmt(pmt_id)
            .is_some_and(|pmt| pmt.ca_registered_mask != 0);
        if !registered {
            return;
        }
        ms_log::logl!(
            ms_log::LOG_TABLES,
            "Closing pmt {} for adapter {}",
            pmt_id,
            aid
        );
        for slot in 0..self.pmt.ca.len() {
            self.close_pmt_for_ca(slot as i32, aid, pmt_id);
        }
    }

    fn tables_add_pid(&mut self, pmt_id: i32, pid: i32) {
        let Some(service) = self.ca_service(pmt_id) else {
            return;
        };
        let mask = self.pmt(pmt_id).map_or(0, |pmt| pmt.ca_mask);
        let mut invalidate = false;
        for (slot, sink) in self.pmt.ca.iter_mut().enumerate() {
            if mask & (1u64 << slot) != 0
                && let Some(sink) = sink
            {
                invalidate |= sink.add_pid(&service, pid);
            }
        }
        if invalidate && let Some(pmt) = self.pmt_slot_mut(pmt_id) {
            pmt.version = -1;
        }
    }

    fn tables_del_pid(&mut self, pmt_id: i32, pid: i32) {
        let Some(service) = self.ca_service(pmt_id) else {
            return;
        };
        let mask = self.pmt(pmt_id).map_or(0, |pmt| pmt.ca_mask);
        for (slot, sink) in self.pmt.ca.iter_mut().enumerate() {
            if mask & (1u64 << slot) != 0
                && let Some(sink) = sink
            {
                sink.del_pid(&service, pid);
            }
        }
    }

    fn ca_service(&self, pmt_id: i32) -> Option<CaService> {
        let pmt = self.pmt(pmt_id)?;
        let tsid = self
            .adapter_index(pmt.adapter)
            .and_then(|index| self.adapters[index].as_ref())
            .map_or(0, |adapter| adapter.transponder_id);
        Some(CaService {
            pmt_id: pmt_id as usize,
            adapter: usize::try_from(pmt.adapter).unwrap_or(0),
            hw_adapter: 0,
            hw_demux: 0,
            tsid,
            sid: pmt.sid as u16,
            pmt_pid: pmt.pid as u16,
            pcr_pid: pmt.pcr_pid as u16,
            ca: pmt.ca[..pmt.caids]
                .iter()
                .map(|ca| CaServiceDescriptor {
                    caid: ca.id,
                    pid: ca.pid,
                    private_data: ca.private_data.clone(),
                })
                .collect(),
            streams: pmt
                .stream_pids
                .iter()
                .map(|stream| CaServiceStream {
                    stream_type: stream.stream_type as u8,
                    pid: stream.pid as u16,
                })
                .collect(),
            av_pids: pmt
                .stream_pids
                .iter()
                .filter(|stream| stream.is_audio || stream.is_video)
                .map(|stream| stream.pid as u16)
                .collect(),
            name: pmt.name.clone(),
            provider: pmt.provider.clone(),
            sdt: pmt.sdt,
        })
    }

    /// The scheduler half of `ddci_update_pmt_name` (ddci.cpp:1020-1032): names
    /// the first PMT for `sid` on CI adapter `aid` unless it is `source` itself
    /// or already carries `name`. Returns the renamed PMT id.
    pub fn name_pmt_for_sid(
        &mut self,
        aid: i32,
        sid: i32,
        source: i32,
        name: &str,
        provider: &str,
    ) -> Option<i32> {
        let pmt = self
            .pmt
            .pmts
            .iter_mut()
            .find(|pmt| pmt.enabled && pmt.adapter == aid && pmt.sid == sid)?;
        if pmt.id as i32 == source || pmt.name == name {
            return None;
        }
        pmt.name = name.to_owned();
        pmt.provider = provider.to_owned();
        Some(pmt.id as i32)
    }

    /// `process_sdt` (pmt.cpp:2246-2291): SDT fields and, once, the service and
    /// provider names of the adapter's PMTs.
    pub fn process_sdt(&mut self, filter: i32, section: &[u8]) -> i32 {
        if section.first() != Some(&SDT_ACTUAL_TABLE_ID) {
            return 0;
        }
        let Some(aid) = self.filter_slot(filter).map(|f| f.adapter) else {
            return 0;
        };
        let Ok(sdt) = Sdt::parse(section, CrcCheck::Skip) else {
            return 0;
        };
        ms_log::debugm!(
            "Processing SDT for transponder {} ({:x}) with length {}, sdt[5] {:02X}",
            sdt.transport_stream_id,
            sdt.transport_stream_id,
            section.len(),
            section.get(5).copied().unwrap_or(0)
        );
        for service in &sdt.services {
            let sid = i32::from(service.service_id);
            let Some(pmt) = self
                .pmt
                .pmts
                .iter_mut()
                .find(|pmt| pmt.enabled && pmt.adapter == aid && pmt.sid == sid)
            else {
                ms_log::debugm!("process_sdt: no PMT found for sid {} ({:X})", sid, sid);
                continue;
            };
            let previous = pmt.sdt.map_or(0, |s| s.service_type);
            pmt.sdt = Some(SdtInfo {
                service_type: service.service_type.unwrap_or(previous),
                running_status: service.running_status,
                ca_mode: u8::from(service.free_ca_mode),
                eit_schedule: u8::from(service.eit_schedule),
                eit_pf: u8::from(service.eit_present_following),
                tsid: sdt.transport_stream_id,
                onid: sdt.original_network_id,
            });
            if pmt.name.is_empty()
                && let Some(name) = &service.service_name
            {
                pmt.provider = service
                    .provider_name
                    .as_deref()
                    .map_or_else(String::new, |p| String::from_utf8_lossy(p).into_owned());
                pmt.name = String::from_utf8_lossy(name).into_owned();
                ms_log::log!(
                    "SDT PMT {}: name {} provider {}, sid: {} ({:X})",
                    pmt.id,
                    pmt.name,
                    pmt.provider,
                    sid,
                    sid
                );
            }
        }
        0
    }

    /// Adds a section filter (`add_filter`).
    pub fn add_filter(&mut self, aid: i32, pid: i32, target: FilterTarget, flags: i32) -> i32 {
        self.add_filter_mask(aid, pid, target, flags, &[0; 16], &[0; 16])
    }

    /// Adds a masked section filter (`add_filter_mask`).
    pub fn add_filter_mask(
        &mut self,
        aid: i32,
        pid: i32,
        target: FilterTarget,
        flags: i32,
        filter: &[u8; 16],
        mask: &[u8; 16],
    ) -> i32 {
        if !(0..=8191).contains(&pid) {
            ms_log::log_and_return!(-1, "add_filter_mask failed, pid {}", pid);
        }
        let Some(slot) = self.pmt.filters.iter().position(|filter| !filter.allocated) else {
            ms_log::log_and_return!(-1, "add_filter_mask failed");
        };
        let id = slot as i32;
        {
            let entry = &mut self.pmt.filters[slot];
            entry.enabled = true;
            entry.allocated = true;
            entry.pid = pid;
            entry.adapter = aid;
            entry.target = target;
            entry.flags = flags;
            entry.filter = *filter;
            entry.mask = *mask;
        }
        if flags & FILTER_ADD_REMOVE != 0 && self.find_pid(aid, pid).is_none() {
            let _ = self.mark_pid_add(aid, crate::PID_STREAM_ID_UNDEFINED, pid);
        }
        let master_filter =
            self.find_pid(aid, pid).map_or(
                id,
                |entry| if entry.filter >= 0 { entry.filter } else { id },
            );
        if let Some(pid_entry) = self.find_pid_mut(aid, pid) {
            pid_entry.filter = id;
        }
        let mask_len = mask
            .iter()
            .rposition(|&byte| byte != 0)
            .map_or(0, |index| index + 1);
        ms_log::log!(
            "new filter {} added for adapter {}, pid {}, flags {}, mask_len {}, master_filter {}",
            id,
            aid,
            pid,
            flags,
            mask_len,
            master_filter
        );
        id
    }

    /// Removes a filter (`del_filter`).
    pub fn del_filter(&mut self, id: i32) {
        ms_log::log!("deleting filter {}", id);
        let Ok(slot) = usize::try_from(id) else {
            return;
        };
        let Some(filter) = self.pmt.filters.get(slot) else {
            return;
        };
        if !filter.enabled {
            return;
        }
        let (aid, pid) = (filter.adapter, filter.pid);
        self.set_filter_flags(id, 0);
        if let Some(filter) = self.pmt.filters.get_mut(slot) {
            filter.enabled = false;
            filter.allocated = false;
            filter.target = FilterTarget::Other;
            filter.flags = 0;
        }
        if let Some(entry) = self.find_pid_mut(aid, pid)
            && entry.filter == id
        {
            entry.filter = -1;
        }
        let max_filters = self
            .pmt
            .filters
            .iter()
            .rposition(|filter| filter.enabled)
            .map_or(0, |index| index + 1) as i32;
        ms_log::log!(
            "deleted filter {}, ad {}, pid {}, max filters {}",
            id,
            aid,
            pid,
            max_filters
        );
    }

    fn active_add_remove_filters(&self, aid: i32, pid: i32) -> usize {
        self.pmt
            .filters
            .iter()
            .filter(|filter| {
                filter.enabled
                    && filter.adapter == aid
                    && filter.pid == pid
                    && filter.flags & (FILTER_ADD_REMOVE | FILTER_PERMANENT) != 0
            })
            .count()
    }

    /// Replaces a filter's flags (`set_filter_flags`), including the
    /// PID add/delete side effects of `set_filter_flags_locked`.
    pub fn set_filter_flags(&mut self, id: i32, flags: i32) {
        let Ok(slot) = usize::try_from(id) else {
            // C++ `LOG_AND_RETURN(0, ...)`; the function returns `()`.
            ms_log::log!("set_filter_flags: filter {} not found", id);
            return;
        };
        let Some(entry) = self.pmt.filters.get_mut(slot) else {
            // C++ `LOG_AND_RETURN(0, ...)`; the function returns `()`.
            ms_log::log!("set_filter_flags: filter {} not found", id);
            return;
        };
        if !entry.enabled {
            // C++ `LOG_AND_RETURN(0, ...)`; the function returns `()`.
            ms_log::log!("set_filter_flags: filter {} not found", id);
            return;
        }
        entry.flags = flags;
        let (aid, pid) = (entry.adapter, entry.pid);
        if flags & FILTER_ADD_REMOVE != 0 {
            if self.find_pid(aid, pid).is_none() {
                let _ = self.mark_pid_add(aid, crate::PID_STREAM_ID_UNDEFINED, pid);
            }
        } else if flags == 0 && self.active_add_remove_filters(aid, pid) == 0 {
            let delete = self
                .find_pid(aid, pid)
                .is_some_and(|entry| entry.state != PidState::Deleted && entry.sids.is_empty());
            if delete {
                let Ok(index) = usize::try_from(aid) else {
                    return;
                };
                self.mark_pid_deleted(index, crate::PID_STREAM_ID_UNDEFINED, Some(pid));
                self.update_pids(index);
            } else {
                let streams = self
                    .find_pid(aid, pid)
                    .map_or(u64::MAX, |entry| entry.sids.len() as u64);
                ms_log::logm!("pid not found or pid in use by {} streams", streams);
            }
        }
    }

    /// Replaces a filter's match bytes (`set_filter_mask`).
    pub fn set_filter_mask(&mut self, id: i32, filter: &[u8; 16], mask: &[u8; 16]) {
        let Ok(slot) = usize::try_from(id) else {
            // C++ `LOG_AND_RETURN(0, ...)`; the function returns `()`.
            ms_log::log!("set_filter_mask: filter {} not found", id);
            return;
        };
        let Some(entry) = self.pmt.filters.get_mut(slot) else {
            // C++ `LOG_AND_RETURN(0, ...)`; the function returns `()`.
            ms_log::log!("set_filter_mask: filter {} not found", id);
            return;
        };
        if !entry.enabled {
            ms_log::logm!("set_filter_mask: filter {} not found", id);
        }
        entry.filter = *filter;
        entry.mask = *mask;
    }

    /// Caches a PMT and releases its CA/claims (`cache_pmt_for_adapter`).
    pub fn cache_pmt_for_adapter(&mut self, aid: i32, pmt_id: i32) {
        self.close_pmt_for_cas(aid, pmt_id);
        self.release_pmt_claims(aid, pmt_id);
        let filter = {
            let Some(pmt) = self.pmt_slot_mut(pmt_id) else {
                return;
            };
            pmt.disabled_ca_mask = 0;
            pmt.ca_mask = 0;
            pmt.ca_registered_mask = 0;
            pmt.state = PmtState::Cached;
            pmt.filter
        };
        if let Some(adapter) = usize::try_from(aid)
            .ok()
            .and_then(|index| self.adapter_mut(index))
        {
            for entry in &mut adapter.active_pmt[..adapter.active_pmts] {
                if *entry == pmt_id {
                    *entry = -1;
                }
            }
        }
        if filter >= 0 {
            self.del_filter(filter);
        }
        if let Some(pmt) = self.pmt_slot_mut(pmt_id) {
            pmt.filter = -1;
        }
    }

    /// Caches every active PMT of the adapter (`cache_pmts_for_adapter`).
    pub fn cache_pmts_for_adapter(&mut self, aid: i32) {
        let Some(index) = self.adapter_index(aid) else {
            return;
        };
        let active: Vec<i32> = self.adapters[index]
            .as_ref()
            .map(|adapter| adapter.active_pmt[..adapter.active_pmts].to_vec())
            .unwrap_or_default();
        for id in active {
            self.cache_pmt_for_adapter(aid, id);
        }
    }

    /// Caches the PMTs and drops non-permanent filters (`clear_pmt_for_adapter`).
    pub fn clear_pmt_for_adapter(&mut self, aid: i32) {
        self.cache_pmts_for_adapter(aid);
        let permanent: Vec<i32> = self
            .pmt
            .filters
            .iter()
            .filter(|filter| {
                filter.enabled && filter.adapter == aid && filter.flags & FILTER_PERMANENT == 0
            })
            .map(|filter| filter.id as i32)
            .collect();
        for id in permanent {
            self.del_filter(id);
        }
        let (pat_filter, sdt_filter) = match self.adapter_index(aid) {
            Some(index) => match self.adapters[index].as_mut() {
                Some(adapter) => {
                    adapter.active_pmts = 0;
                    adapter.pat_processed = false;
                    adapter.transponder_id = 0;
                    adapter.pat_ver = 0;
                    (adapter.pat_filter, adapter.sdt_filter)
                }
                None => return,
            },
            None => return,
        };
        for filter in [pat_filter, sdt_filter] {
            if filter >= 0 {
                self.set_filter_mask(filter, &[0; 16], &[0; 16]);
                self.set_filter_flags(filter, FILTER_PERMANENT | FILTER_CRC);
            }
        }
    }

    /// Processes a PAT section (`process_pat`).
    pub fn process_pat(&mut self, filter: i32, section: &[u8]) -> i32 {
        if section.len() < 12 || section[0] != 0 {
            return 0;
        }
        let Ok(slot) = usize::try_from(filter) else {
            return 0;
        };
        let Some(entry) = self.pmt.filters.get(slot) else {
            return 0;
        };
        if !entry.enabled {
            return 0;
        }
        let aid = entry.adapter;
        let tid = u16::from_be_bytes([section[3], section[4]]);
        let ver = i32::from((section[5] & 0x3e) >> 1);
        let Some(index) = self.adapter_index(aid) else {
            return 0;
        };
        let Some(adapter) = self.adapters[index].as_ref() else {
            return 0;
        };
        if !adapter.config.enabled {
            ms_log::log_and_return!(0, "Adapter {} no longer enabled, not processing PAT", aid);
        }
        if adapter.transponder_id == tid && adapter.pat_ver == ver {
            ms_log::log_and_return!(0, "PAT AD {}: tsid {} version {}", aid, tid, ver);
        }
        if adapter.pat_processed && adapter.transponder_id == tid && adapter.pat_ver != ver {
            ms_log::log!(
                "PAT AD {} new version for transponder {}, version {}",
                aid,
                adapter.transponder_id,
                ver
            );
        }
        let new_transponder = adapter.pat_processed && adapter.transponder_id != tid;
        if new_transponder {
            ms_log::log!(
                "PAT AD {} new transponder {} (old {}), version {}",
                aid,
                tid,
                adapter.transponder_id,
                adapter.pat_ver
            );
            self.clear_pmt_for_adapter(aid);
        }
        if let Some(adapter) = self.adapters[index].as_mut() {
            adapter.pat_ver = ver;
            adapter.transponder_id = tid;
        }
        let mut match_bytes = [0u8; 16];
        let mut match_mask = [0u8; 16];
        match_bytes[1] = section[3];
        match_mask[1] = 0xFF;
        match_bytes[2] = section[4];
        match_mask[2] = 0xFF;
        match_bytes[3] = section[5];
        match_mask[3] = 0xFF;
        self.set_filter_mask(filter, &match_bytes, &match_mask);
        self.set_filter_flags(filter, FILTER_PERMANENT | FILTER_REVERSE | FILTER_CRC);

        let pat_len = section.len() as i32 - 13;
        ms_log::log!(
            "PAT Adapter {}, Transponder ID {}, version {}, len {}",
            aid,
            tid,
            ver,
            pat_len
        );

        if let Some(adapter) = self.adapters[index].as_mut() {
            adapter.active_pmts = 0;
        }
        let mut seen = BTreeSet::new();
        let end = section.len().saturating_sub(4);
        let mut offset = 8;
        while offset + 4 <= end {
            let sid = i32::from(u16::from_be_bytes([section[offset], section[offset + 1]]));
            let pid = i32::from(
                (((section[offset + 2] & 0x1F) as u16) << 8) | u16::from(section[offset + 3]),
            );
            let existing = self.get_pmt_for_sid_pid(aid, sid, pid).map(|pmt| pmt.id);
            ms_log::log!(
                "Adapter {}, PMT {} sid {} ({:04X}), pid {}",
                aid,
                existing.map_or(-1, |id| id as i32),
                sid,
                sid,
                pid
            );
            if let Some(existing) = existing {
                seen.insert(existing);
            }
            if sid > 0 {
                if let Some(existing) = existing {
                    self.pmt_add_active_pmt(aid, existing as i32);
                }
                let current_filter = self
                    .pmt(existing.map_or(-1, |id| id as i32))
                    .map_or(-1, |pmt| pmt.filter);
                if current_filter == -1 {
                    let mut filter_bytes = [0u8; 16];
                    let mut mask_bytes = [0u8; 16];
                    filter_bytes[1] = section[offset];
                    mask_bytes[1] = 0xFF;
                    filter_bytes[2] = section[offset + 1];
                    mask_bytes[2] = 0xFF;
                    let flags = if self.pmt.pmt_scan && existing.is_none() {
                        FILTER_ADD_REMOVE | FILTER_CRC
                    } else {
                        0
                    };
                    let id = self.add_filter_mask(
                        aid,
                        pid,
                        FilterTarget::Pmt(existing),
                        flags,
                        &filter_bytes,
                        &mask_bytes,
                    );
                    if let Some(existing) = existing
                        && id >= 0
                        && let Some(pmt) = self.pmt_slot_mut(existing as i32)
                    {
                        pmt.filter = id;
                    }
                }
            }
            offset += 4;
        }

        let retire: Vec<i32> = self
            .pmt
            .pmts
            .iter()
            .filter(|pmt| {
                pmt.enabled
                    && pmt.adapter == aid
                    && pmt.state != PmtState::Cached
                    && !seen.contains(&pmt.id)
            })
            .map(|pmt| pmt.id as i32)
            .collect();
        for id in retire {
            let Some(pmt) = self.pmt(id) else {
                continue;
            };
            ms_log::log!(
                "Caching PMT {} ({}) and filter {} as it is not present in the new PAT",
                pmt.id,
                pmt.name,
                pmt.filter
            );
            if self.adapter_type(aid) == AdapterType::Ci {
                self.pmt_del(id);
            } else {
                self.cache_pmt_for_adapter(aid, id);
            }
        }
        self.update_pids(index);
        if let Some(adapter) = self.adapters[index].as_mut() {
            adapter.pat_processed = true;
        }
        0
    }

    /// Processes a PMT section (`process_pmt`).
    pub fn process_pmt(&mut self, filter: i32, section: &[u8]) -> i32 {
        if section.len() < 12 || section[0] != 2 {
            return 0;
        }
        let Ok(slot) = usize::try_from(filter) else {
            ms_log::log_and_return!(0, "process_pmt: filter {} not found", filter);
        };
        let Some(entry) = self.pmt.filters.get(slot) else {
            ms_log::log_and_return!(0, "process_pmt: filter {} not found", filter);
        };
        if !entry.enabled {
            ms_log::log_and_return!(0, "process_pmt: filter {} not found", filter);
        }
        let (aid, pid, target) = (entry.adapter, entry.pid, entry.target);
        let sid = i32::from(u16::from_be_bytes([section[3], section[4]]));
        let version = i32::from((section[5] & 0x3e) >> 1);
        let pmt_id = match target {
            FilterTarget::Pmt(Some(id)) => {
                if self.pmt(id as i32).is_none() {
                    ms_log::log!("PMT {} does not exist", id);
                    return 0;
                }
                id as i32
            }
            _ => {
                let id = self.pmt_add(aid, sid, pid);
                if id < 0 {
                    ms_log::log!("PMT {} does not exist", -1);
                    return 0;
                }
                if let Some(entry) = self.pmt.filters.get_mut(slot) {
                    entry.target = FilterTarget::Pmt(Some(id as usize));
                }
                id
            }
        };
        if self.pmt(pmt_id).is_none() {
            ms_log::log!("PMT {} does not exist", pmt_id);
            return 0;
        }
        if let Some(pmt) = self.pmt_slot_mut(pmt_id) {
            pmt.filter = filter;
        }
        self.pmt_add_active_pmt(aid, pmt_id);
        if self.pmt(pmt_id).is_some_and(|pmt| pmt.version == version) {
            return 0;
        }
        if self.find_pid(aid, pid).is_none() {
            return -1;
        }
        let was_running = self
            .pmt(pmt_id)
            .is_some_and(|pmt| matches!(pmt.state, PmtState::Running | PmtState::Starting));
        let old_registered = if was_running {
            self.pmt(pmt_id).map_or(0, |pmt| pmt.ca_registered_mask)
        } else {
            self.release_pmt_claims(aid, pmt_id);
            0
        };
        if let Some(pmt) = self.pmt_slot_mut(pmt_id) {
            pmt.caids = 0;
        }
        let parsed = match ParsedPmt::parse(section, CrcCheck::Skip) {
            Ok(parsed) => parsed,
            Err(_) => return 0,
        };
        let pmt_len = section.len() as i32 - 3;
        let pi_len = (i32::from(section[10] & 0x0F) << 8) | i32::from(section[11]);
        let old_ver = self.pmt(pmt_id).map_or(-1, |pmt| pmt.version);
        if let Some(pmt) = self.pmt_slot_mut(pmt_id) {
            pmt.sid = sid;
            pmt.version = version;
            pmt.pcr_pid = i32::from(parsed.pcr_pid_header);
            pmt.descriptors.clear();
            pmt.stream_pids.clear();
        }
        if let Some(pmt) = self.pmt(pmt_id) {
            let channel = if pmt.name.is_empty() {
                ""
            } else {
                " channel: "
            };
            let update = if old_ver != -1 && old_ver != version {
                " UPDATE"
            } else {
                ""
            };
            ms_log::log!(
                "new PMT {} AD {}, pid: {:04X} ({}), filter {}, len {}, pi_len {}, ver {}, pcr {}, sid {:04X} ({}){}{}{}",
                pmt_id,
                aid,
                pid,
                pid,
                filter,
                pmt_len,
                pi_len,
                version,
                i32::from(parsed.pcr_pid_header),
                pmt.sid,
                pmt.sid,
                channel,
                pmt.name,
                update
            );
        }
        for descriptor in &parsed.program_info {
            self.add_ca_from_descriptor(
                pmt_id,
                &Descriptor {
                    tag: descriptor.tag,
                    data: descriptor.data.clone(),
                },
            );
            if let Some(pmt) = self.pmt_slot_mut(pmt_id)
                && !pmt.descriptors.iter().any(|existing| {
                    existing.tag == descriptor.tag && existing.data == descriptor.data
                })
            {
                pmt.descriptors.push(Descriptor {
                    tag: descriptor.tag,
                    data: descriptor.data.clone(),
                });
            }
        }
        for stream in &parsed.streams {
            if let Some(pmt) = self.pmt_slot_mut(pmt_id) {
                pmt.stream_pids.push(StreamPid {
                    stream_type: i32::from(stream.stream_type),
                    pid: i32::from(stream.elementary_pid),
                    is_audio: stream.is_audio,
                    is_video: stream.is_video,
                    descriptors: stream
                        .descriptors
                        .iter()
                        .map(|descriptor| Descriptor {
                            tag: descriptor.tag,
                            data: descriptor.data.clone(),
                        })
                        .collect(),
                });
            }
            if stream.is_audio || stream.is_video {
                for descriptor in &stream.descriptors {
                    self.add_ca_from_descriptor(
                        pmt_id,
                        &Descriptor {
                            tag: descriptor.tag,
                            data: descriptor.data.clone(),
                        },
                    );
                }
            }
        }

        if was_running {
            if let Some(pmt) = self.pmt_slot_mut(pmt_id) {
                pmt.ca_registered_mask = old_registered;
            }
            let caids = self.pmt(pmt_id).map_or(0, |pmt| pmt.caids);
            if caids == 0 {
                self.close_pmt_for_cas(aid, pmt_id);
            } else if let Some(pmt) = self.pmt_slot_mut(pmt_id) {
                pmt.ca_mask = 0;
                pmt.disabled_ca_mask = 0;
            }
        }

        if self.pmt_subscribed(pmt_id) {
            self.handover_claims_from_lower_priority(pmt_id);
        }
        if self
            .pmt(pmt_id)
            .is_some_and(|pmt| pmt.state == PmtState::Stopped)
        {
            self.set_filter_flags(filter, 0);
        }
        0
    }

    /// Drops every PMT slot (`free_all_pmts`).
    pub fn free_all_pmts(&mut self) {
        for pmt in &mut self.pmt.pmts {
            pmt.enabled = false;
            pmt.allocated = false;
            pmt.caids = 0;
            pmt.ca.clear();
            pmt.descriptors.clear();
            pmt.stream_pids.clear();
        }
    }

    /// Drops every filter slot (`free_filters`).
    pub fn free_filters(&mut self) {
        for filter in &mut self.pmt.filters {
            filter.enabled = false;
            filter.allocated = false;
            filter.target = FilterTarget::Other;
        }
    }
}

#[cfg(test)]
#[path = "pmt_tests.rs"]
mod tests;
