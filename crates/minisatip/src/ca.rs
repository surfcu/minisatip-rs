//! CA wiring on the control thread (`pmt_process_stream`): sections feed the PMT
//! tables, which offer services to the CA drivers; CWs go to the data planes.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use ms_ca::{CaEvent, CaService, FilterId, TablesResult};
use ms_ca_drivers::descramble::{
    CipherRegistry, DescramblerCommand, DescramblerStage, DescramblerStats, StageConfig,
};
use ms_ca_drivers::dvbapi::transport::{self, Stream};
use ms_ca_drivers::dvbapi::{DvbapiClient, DvbapiConfig, PmtStatus, PollAction};
use ms_ca_drivers::en50221::AdapterInfo;
use ms_config::Options;

use crate::ddci::{DdciCapture, DdciHub, DdciSink, DdciStage};
use crate::dvbca::{Dvbca, DvbcaSettings, DvbcaSink, Notice, PendingOffers};
use ms_ca_drivers::ddci::{CamInfo, DDCI_SID};
use ms_net::dataplane::{AdapterHandle, Command};
use ms_sched::pmt::{
    CaSink, FILTER_ADD_REMOVE, FILTER_CRC, FILTER_EMM, FILTER_REVERSE, FilterTarget, PmtState,
};
use ms_sched::{MAX_ADAPTERS, PidState, Scheduler};
use ms_ts::{SectionAssembler, TsStage, crc32_mpeg2};
use ms_tuner::PidSet;
use std::collections::BTreeSet;

/// `connect_dvbapi` runs from a 1 s timer (dvbapi.cpp).
const DVBAPI_POLL_MS: i64 = 1000;
/// `dvbapi_ts` runs the watchdog at most every 100 ms per adapter.
const WATCHDOG_MS: i64 = 100;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const WRITE_TIMEOUT: Duration = Duration::from_millis(200);
/// PID that requests the whole transport stream.
const PID_ALL: i32 = 8192;

/// CA state of one adapter as last pushed to its data plane.
struct AdapterCa {
    descrambler: Sender<DescramblerCommand>,
    services: BTreeMap<usize, Vec<u16>>,
    ca_active: bool,
    pids_all: bool,
    stats: DescramblerStats,
    pending_stats: Option<Receiver<DescramblerStats>>,
}

/// Owns every CA driver and the section routing of all adapters.
pub struct CaRuntime {
    origin: Instant,
    stage_config: StageConfig,
    adapters: BTreeMap<usize, AdapterCa>,
    /// Section PIDs last sent to each data plane.
    section_pids: BTreeMap<usize, Vec<u16>>,
    router: SectionRouter,
    dvbapi: Option<Dvbapi>,
    dvbca: Option<DvbcaState>,
    ddci: Option<DdciLink>,
    dvbca_settings: DvbcaSettings,
    /// `--hw-descrambler`: cipher registry for `(aid, pa, fe)` and the CA id.
    hw: Option<(HwRegistryFactory, i32)>,
    /// Physical adapter and frontend of each DVB adapter.
    hw_nodes: BTreeMap<usize, (i32, i32)>,
    /// PMTs offered to a CA since the last drain (`disable_cw` in `send_pmt_to_ca`).
    offered: Rc<RefCell<Vec<(usize, usize)>>>,
    next_poll: i64,
    next_watchdog: i64,
}

impl CaRuntime {
    /// CA settings from the command line (`-o`, `--send-all-ecm`, `-E`).
    pub fn new(opts: &Options) -> Self {
        let origin = Instant::now();
        let offered = Rc::new(RefCell::new(Vec::new()));
        let cfg = DvbapiConfig::from_options(opts);
        let dvbapi = cfg.is_configured().then(|| Dvbapi::new(cfg));
        Self {
            origin,
            stage_config: StageConfig {
                drop_encrypted: opts.drop_encrypted,
                pids_all_no_dec: opts.pids_all_no_dec != 0,
            },
            adapters: BTreeMap::new(),
            section_pids: BTreeMap::new(),
            router: SectionRouter::default(),
            dvbapi,
            dvbca: None,
            ddci: None,
            dvbca_settings: DvbcaSettings::from_options(opts),
            hw: None,
            hw_nodes: BTreeMap::new(),
            offered,
            next_poll: 0,
            next_watchdog: 0,
        }
    }

    fn now(&self) -> i64 {
        self.origin.elapsed().as_millis() as i64
    }

    /// The clock the CA sinks use inside scheduler calls (`getTick()`).
    fn set_driver_time(&self, now: i64) {
        if let Some(dvbapi) = self.dvbapi.as_ref() {
            dvbapi.shared.borrow_mut().client.set_time(now);
        }
    }

    /// In-place stages for a new adapter data plane, in C++ order:
    /// `ddci_ts` (via `tables_ca_ts`), then decrypt and null marking.
    pub fn stages(&mut self, aid: usize) -> Vec<Box<dyn TsStage>> {
        let mut stages: Vec<Box<dyn TsStage>> = Vec::new();
        if let Some(link) = self.ddci.as_ref() {
            stages.push(Box::new(DdciStage::new(link.hub.clone(), aid)));
        }
        let registry = match (self.hw.as_ref(), self.hw_nodes.get(&aid)) {
            (Some((factory, _)), Some(&(pa, fe))) => factory(aid, pa, fe),
            _ => CipherRegistry::software(),
        };
        let (sender, stage) = DescramblerStage::new(registry, self.stage_config, self.origin);
        self.adapters.insert(
            aid,
            AdapterCa {
                descrambler: sender,
                services: BTreeMap::new(),
                ca_active: false,
                pids_all: false,
                stats: DescramblerStats::default(),
                pending_stats: None,
            },
        );
        stages.push(Box::new(stage));
        stages
    }

    /// Registers `hw_ca_op` and the hardware ciphers (`init_hw_descrambler`).
    pub fn attach_hw<D>(&mut self, sched: &mut Scheduler, hw: crate::hw::Hw<D>)
    where
        D: ms_ca_drivers::hw_descrambler::CaDevice + Send + 'static,
    {
        let ca_id = sched.add_ca(Box::new(Offering::new(hw.sink(), &self.offered)));
        for aid in 0..MAX_ADAPTERS {
            sched.init_ca_for_adapter(aid, ca_id);
        }
        self.hw = Some((Box::new(move |aid, pa, fe| hw.registry(aid, pa, fe)), ca_id));
    }

    /// The `/dev/dvb/adapter<pa>/ca<fe>` node of a DVB adapter, before [`Self::stages`].
    pub fn set_hw_node(&mut self, aid: usize, pa: i32, fe: i32) {
        self.hw_nodes.insert(aid, (pa, fe));
    }

    /// Registers the DDCI CA (`ddci_init`); call before spawning tuner adapters.
    pub fn attach_ddci(&mut self, sched: &mut Scheduler, hub: DdciHub) {
        let sink = DdciSink { hub: hub.clone() };
        let ca_id = sched.add_ca(Box::new(Offering::new(sink, &self.offered)));
        // ddci_init_dev accepts every adapter.
        for aid in 0..MAX_ADAPTERS {
            sched.init_ca_for_adapter(aid, ca_id);
        }
        self.ddci = Some(DdciLink {
            hub,
            ca_id,
            mirrored: BTreeSet::new(),
            cat_filters: BTreeMap::new(),
        });
    }

    /// Stages of a DDCI adapter: its batches feed the device FIFO.
    pub fn ci_stages(&mut self, aid: usize) -> Vec<Box<dyn TsStage>> {
        match self.ddci.as_ref() {
            Some(link) => vec![Box::new(DdciCapture::new(link.hub.clone(), aid))],
            None => Vec::new(),
        }
    }

    /// Mirrors DDCI's PID subscriptions and CAT filters into the scheduler
    /// (DDCI calls `mark_pid_add`/`add_filter` directly in C++).
    fn sync_ddci(&mut self, sched: &mut Scheduler) {
        let Some(link) = self.ddci.as_mut() else {
            return;
        };
        {
            let mut state = link.hub.lock();
            for (aid, adapter) in state.ddci.adapters.iter_mut() {
                adapter.transponder_id = sched.adapter_transponder_id(usize::from(*aid));
            }
        }
        let wanted: BTreeSet<(usize, u16)> = link.hub.ddci_pids().into_iter().collect();
        let mut touched = BTreeSet::new();
        for &(aid, pid) in wanted.difference(&link.mirrored) {
            sched.mark_pid_add(aid as i32, DDCI_SID, i32::from(pid));
            touched.insert(aid);
        }
        for &(aid, pid) in link.mirrored.difference(&wanted) {
            sched.unmark_pid(aid, DDCI_SID, i32::from(pid));
            touched.insert(aid);
        }
        link.mirrored = wanted;
        let filters: BTreeMap<usize, u16> = link
            .hub
            .lock()
            .ddci
            .filters
            .iter()
            .map(|(id, (aid, _))| (*id, *aid))
            .collect();
        let known: BTreeSet<usize> = link.cat_filters.values().map(|(id, _)| *id).collect();
        for (&id, &aid) in &filters {
            if !known.contains(&id) {
                let fid = sched.add_filter(i32::from(aid), 1, FilterTarget::Other, FILTER_CRC);
                if fid >= 0 {
                    link.cat_filters.insert(fid, (id, i32::from(aid)));
                }
            }
        }
        let gone: Vec<i32> = link
            .cat_filters
            .iter()
            .filter(|(_, (id, _))| !filters.contains_key(id))
            .map(|(fid, _)| *fid)
            .collect();
        for fid in gone {
            link.cat_filters.remove(&fid);
            sched.del_filter(fid);
        }
        for aid in touched {
            sched.update_adapter_pids(aid);
        }
    }

    /// CAM state of a DDCI adapter as `ddci.cpp` reads it from `ca_devices[]`.
    fn ddci_cam(&self, aid: usize, update: impl FnOnce(&mut CamInfo, &mut Vec<(u16, u16)>)) {
        let Some(link) = self.ddci.as_ref() else {
            return;
        };
        let (max_ca_pmt, multiple_pmt) = self.dvbca_settings.limits(aid);
        let mut state = link.hub.lock();
        if !state.ddci.devices.contains_key(&aid) {
            return;
        }
        let cam = state.ddci.cams.entry(aid).or_insert(CamInfo {
            enabled: false,
            initialized: false,
            multiple_pmt,
            max_ca_pmt,
        });
        let mut cam_info = *cam;
        let mut masks = state.ddci.caid_masks.remove(&aid).unwrap_or_default();
        update(&mut cam_info, &mut masks);
        state.ddci.cams.insert(aid, cam_info);
        state.ddci.caid_masks.insert(aid, masks);
        let max = state.ddci.max_pmt_for_ca(aid);
        if let Some(device) = state.ddci.devices.get_mut(&aid) {
            // ddci_post_init caps max_channels at MAX_CHANNELS_ON_CI.
            device.max_channels = max.min(ms_ca_drivers::ddci::MAX_CHANNELS_ON_CI);
        }
    }

    /// Registers the EN 50221 CA (`dvbca_init`, first in `tables_init`).
    pub fn attach_dvbca(&mut self, sched: &mut Scheduler, dvbca: Dvbca) {
        let dvbca = Rc::new(dvbca);
        let caids = Rc::new(RefCell::new(BTreeMap::new()));
        let pending = PendingOffers::default();
        let sink = DvbcaSink::new(Rc::clone(&dvbca), Rc::clone(&caids), Rc::clone(&pending));
        let ca_id = sched.add_ca(Box::new(Offering::new(sink, &self.offered)));
        self.dvbca = Some(DvbcaState {
            dvbca,
            caids,
            pending,
            ca_id,
        });
    }

    /// `dvbca_init_dev` for an opened DVB adapter.
    pub fn dvbca_init_dev(&self, info: AdapterInfo) {
        if let Some(state) = self.dvbca.as_ref() {
            state.dvbca.init_dev(info);
        }
    }

    fn dvbca_notices(&mut self, sched: &mut Scheduler) {
        let Some(state) = self.dvbca.as_ref() else {
            return;
        };
        // An unanswered offer whose PMT stopped or changed is released.
        state.dvbca.sweep_pending(&state.pending, |id, program| {
            sched.pmt(id as i32).is_some_and(|pmt| {
                matches!(pmt.state, PmtState::Running | PmtState::Starting)
                    && pmt.adapter == i32::from(program.adapter)
                    && pmt.sid == i32::from(program.sid)
                    && pmt.pid == i32::from(program.pid)
            })
        });
        while let Some(notice) = state.dvbca.try_notice() {
            match notice {
                Notice::Usable(aid) => {
                    sched.init_ca_for_adapter(aid, state.ca_id);
                    self.ddci_cam(aid, |cam, _| cam.enabled = true);
                }
                Notice::Caids(aid, caids) => {
                    state.caids.borrow_mut().insert(aid, caids.clone());
                    self.ddci_cam(aid, |cam, masks| {
                        cam.initialized = true;
                        *masks = caids.iter().map(|c| (*c, 0xFFFF)).collect();
                    });
                }
                Notice::Released(ids) => {
                    for pmt_id in ids {
                        sched.clear_pmt_ca_masks(pmt_id, state.ca_id, true);
                        let aid = sched
                            .pmt(pmt_id)
                            .and_then(|pmt| usize::try_from(pmt.adapter).ok());
                        if let Some(ca) = aid.and_then(|aid| self.adapters.get(&aid)) {
                            let _ = ca.descrambler.send(DescramblerCommand::DisableCw {
                                pmt_id: pmt_id as usize,
                            });
                        }
                    }
                }
            }
        }
    }

    /// An adapter joined the scheduler: registered CAs may handle it (`pmt_init_device`).
    pub fn adapter_added(&mut self, sched: &mut Scheduler, aid: usize) {
        if let Some(id) = self.dvbapi.as_ref().and_then(|d| d.ca_id) {
            sched.init_ca_for_adapter(aid, id);
        }
        if let Some(link) = self.ddci.as_ref() {
            sched.init_ca_for_adapter(aid, link.ca_id);
        }
        if let Some((_, ca_id)) = self.hw.as_ref() {
            sched.init_ca_for_adapter(aid, *ca_id);
        }
    }

    /// An adapter left the scheduler; its data plane is gone.
    pub fn adapter_removed(&mut self, aid: usize) {
        self.adapters.remove(&aid);
        self.section_pids.remove(&aid);
        self.hw_nodes.remove(&aid);
        self.router.forget_adapter(aid as i32);
    }

    /// Packets tapped from a batch (`process_filters`), then `start_active_pmts`.
    pub fn on_table_packets(
        &mut self,
        sched: &mut Scheduler,
        handles: &[Option<AdapterHandle>],
        aid: usize,
        generation: u64,
        packets: &[u8],
    ) {
        let current = handles
            .get(aid)
            .and_then(Option::as_ref)
            .map_or(generation, AdapterHandle::tune_generation);
        // A batch read before the last tune belongs to the previous mux.
        if generation < current {
            return;
        }
        let now = self.now();
        self.set_driver_time(now);
        for packet in packets.as_chunks::<188>().0 {
            for (filter, section) in self.router.feed(sched, aid as i32, packet) {
                self.dispatch(sched, filter, &section, now);
            }
        }
        sched.start_active_pmts(aid as i32);
        self.after_scheduler(sched, handles);
    }

    fn dispatch(&mut self, sched: &mut Scheduler, filter: i32, section: &[u8], now: i64) {
        let Some(target) = sched.filter_slot(filter).map(|f| f.target) else {
            return;
        };
        match target {
            FilterTarget::Pat => {
                sched.process_pat(filter, section);
            }
            FilterTarget::Pmt(_) => {
                sched.process_pmt(filter, section);
            }
            // DEV-212: SDT names are not parsed yet.
            FilterTarget::Sdt => {}
            FilterTarget::Other => {
                if let Some(link) = self.ddci.as_ref()
                    && let Some(&(id, _)) = link.cat_filters.get(&filter)
                {
                    link.hub.lock().ddci.process_cat_filter(id, section);
                    return;
                }
                let Some(dvbapi) = self.dvbapi.as_ref() else {
                    return;
                };
                let Some(&id) = dvbapi.filters.get(&filter) else {
                    return;
                };
                let shared = Rc::clone(&dvbapi.shared);
                let pmt = shared.borrow().client.pmt_for_filter(id);
                let cw_valid = pmt.is_some_and(|pmt_id| self.cw_valid(pmt_id));
                shared
                    .borrow_mut()
                    .client
                    .deliver_section(id, section, cw_valid, now);
            }
        }
    }

    /// `pmt->cw != NULL` as last reported by the adapter's descrambler.
    fn cw_valid(&self, pmt_id: usize) -> bool {
        self.adapters.values().any(|ca| {
            ca.stats
                .services
                .iter()
                .any(|s| s.pmt_id == pmt_id && s.cw.is_some())
        })
    }

    /// One control-loop pass: DVBAPI socket, timers, events and data-plane sync.
    pub fn tick(&mut self, sched: &mut Scheduler, handles: &[Option<AdapterHandle>]) {
        let now = self.now();
        self.set_driver_time(now);
        self.collect_stats();
        self.dvbca_notices(sched);
        if let Some(dvbapi) = self.dvbapi.as_mut() {
            dvbapi.service_socket(now);
            if now >= self.next_poll {
                self.next_poll = now + DVBAPI_POLL_MS;
                let active =
                    (0..MAX_ADAPTERS).any(|aid| sched.adapter(aid).is_some_and(|a| a.sid_cnt > 0));
                dvbapi.poll(now, active);
            }
        }
        if now >= self.next_watchdog {
            self.next_watchdog = now + WATCHDOG_MS;
            self.watchdog(sched, now);
        }
        self.after_scheduler(sched, handles);
    }

    /// Handles driver events until none are left, then pushes the new state
    /// to the data planes.
    pub fn after_scheduler(&mut self, sched: &mut Scheduler, handles: &[Option<AdapterHandle>]) {
        loop {
            let event = self
                .dvbapi
                .as_ref()
                .and_then(|d| d.shared.borrow_mut().client.next_event());
            let Some(event) = event else {
                break;
            };
            self.handle_dvbapi_event(sched, event);
        }
        self.sync_ddci(sched);
        self.sync(sched, handles);
        let offered: Vec<(usize, usize)> = self.offered.borrow_mut().drain(..).collect();
        for (aid, pmt_id) in offered {
            if let Some(ca) = self.adapters.get(&aid) {
                let _ = ca
                    .descrambler
                    .send(DescramblerCommand::DisableCw { pmt_id });
            }
        }
        if let Some(dvbapi) = self.dvbapi.as_mut() {
            dvbapi.flush(self.origin.elapsed().as_millis() as i64);
        }
    }

    fn handle_dvbapi_event(&mut self, sched: &mut Scheduler, event: CaEvent) {
        let Some(dvbapi) = self.dvbapi.as_mut() else {
            return;
        };
        match event {
            CaEvent::Ready => {
                // add_ca() first drops a stale registration of the same op (tables.cpp:64).
                if let Some(id) = dvbapi.ca_id.take() {
                    sched.del_ca(id);
                }
                let sink = DvbapiSink {
                    shared: Rc::clone(&dvbapi.shared),
                };
                let id = sched.add_ca(Box::new(Offering::new(sink, &self.offered)));
                dvbapi.ca_id = Some(id);
                for aid in 0..MAX_ADAPTERS {
                    sched.init_ca_for_adapter(aid, id);
                }
                for aid in 0..MAX_ADAPTERS {
                    if sched.adapter(aid).is_some() {
                        sched.start_active_pmts(aid as i32);
                    }
                }
            }
            CaEvent::Closed => {
                if let Some(id) = dvbapi.ca_id.take() {
                    sched.del_ca(id);
                }
            }
            CaEvent::Unsupported { build } => crate::sys::log_error(format_args!(
                "minisatip: dvbapi: OSCam build {build} is too old, not registering"
            )),
            CaEvent::SetFilter {
                id,
                adapter,
                pid,
                filter,
                mask,
                emm,
                ..
            } => {
                let flags = FILTER_ADD_REMOVE | if emm { FILTER_EMM } else { 0 };
                let fid = sched.add_filter_mask(
                    adapter as i32,
                    i32::from(pid),
                    FilterTarget::Other,
                    flags,
                    &filter,
                    &mask,
                );
                if fid >= 0 {
                    // A reused slot may still map an older DVBAPI filter.
                    if let Some(old) = dvbapi.filters.insert(fid, id) {
                        dvbapi.by_id.remove(&old);
                    }
                    dvbapi
                        .filter_owner
                        .insert(fid, (adapter as i32, i32::from(pid)));
                    dvbapi.by_id.insert(id, fid);
                    sched.update_adapter_pids(adapter);
                }
            }
            CaEvent::StopFilter { id } => {
                if let Some(fid) = dvbapi.by_id.remove(&id) {
                    dvbapi.filters.remove(&fid);
                    dvbapi.filter_owner.remove(&fid);
                    sched.del_filter(fid);
                }
            }
            CaEvent::ControlWord { pmt_id, .. } => {
                let Some(aid) = sched
                    .pmt(pmt_id as i32)
                    .and_then(|pmt| usize::try_from(pmt.adapter).ok())
                else {
                    return;
                };
                // The service must be known before its first CW arrives.
                self.sync_adapter(sched, aid);
                if let Some(ca) = self.adapters.get(&aid) {
                    let _ = ca.descrambler.send(DescramblerCommand::Ca(event));
                }
            }
            CaEvent::Restart { pmt_id } => {
                if let Some(id) = dvbapi.ca_id {
                    sched.restart_pmt_for_ca(id, pmt_id as i32);
                }
            }
            CaEvent::Gone => {}
        }
    }

    /// `dvbapi_descramble_watchdog` with the stamps of the last stats reply.
    fn watchdog(&mut self, sched: &Scheduler, now: i64) {
        let Some(dvbapi) = self.dvbapi.as_mut() else {
            return;
        };
        for (aid, ca) in &mut self.adapters {
            let statuses: Vec<PmtStatus> = ca
                .stats
                .services
                .iter()
                .filter_map(|s| {
                    let pmt = sched.pmt(s.pmt_id as i32)?;
                    Some(PmtStatus {
                        pmt_id: s.pmt_id,
                        adapter: usize::try_from(pmt.adapter).ok()?,
                        enabled: pmt.enabled,
                        running: pmt.state == PmtState::Running,
                        last_decrypt_ok: s.last_decrypt_ok,
                        last_scrambled_pkt: s.last_scrambled_pkt,
                    })
                })
                .collect();
            dvbapi
                .shared
                .borrow_mut()
                .client
                .watchdog(*aid, now, &statuses);
            if ca.pending_stats.is_none() {
                let (tx, rx) = mpsc::channel();
                if ca.descrambler.send(DescramblerCommand::Query(tx)).is_ok() {
                    ca.pending_stats = Some(rx);
                }
            }
        }
    }

    fn collect_stats(&mut self) {
        for ca in self.adapters.values_mut() {
            let Some(rx) = ca.pending_stats.as_ref() else {
                continue;
            };
            match rx.try_recv() {
                Ok(stats) => {
                    ca.stats = stats;
                    ca.pending_stats = None;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => ca.pending_stats = None,
            }
        }
    }

    fn sync(&mut self, sched: &Scheduler, handles: &[Option<AdapterHandle>]) {
        let aids: Vec<usize> = self.adapters.keys().copied().collect();
        for aid in aids {
            self.sync_adapter(sched, aid);
        }
        self.forget_stale_filters(sched);
        // Every data plane with a handle taps its filtered PIDs, CI adapters included.
        for (aid, handle) in handles.iter().enumerate() {
            let Some(handle) = handle else {
                continue;
            };
            let pids = sched.section_pids(aid as i32);
            if self.section_pids.get(&aid) != Some(&pids)
                && handle.send(Command::SetSectionPids(PidSet::from_pids(
                    pids.iter().copied(),
                )))
            {
                self.section_pids.insert(aid, pids);
            }
        }
        self.router.retain(sched);
    }

    /// Drops filter mappings whose scheduler slot was freed or reused, e.g. by
    /// `clear_pmt_for_adapter` on a tune; C++ keeps dangling ids that never fire.
    fn forget_stale_filters(&mut self, sched: &Scheduler) {
        let live = |fid: i32, aid: i32, pid: i32| {
            sched.filter_slot(fid).is_some_and(|f| {
                f.enabled && f.target == FilterTarget::Other && f.adapter == aid && f.pid == pid
            })
        };
        if let Some(dvbapi) = self.dvbapi.as_mut() {
            let stale: Vec<i32> = dvbapi
                .filter_owner
                .iter()
                .filter(|(fid, (aid, pid))| !live(**fid, *aid, *pid))
                .map(|(fid, _)| *fid)
                .collect();
            for fid in stale {
                dvbapi.filter_owner.remove(&fid);
                if let Some(id) = dvbapi.filters.remove(&fid) {
                    dvbapi.by_id.remove(&id);
                }
            }
        }
        if let Some(link) = self.ddci.as_mut() {
            link.cat_filters.retain(|fid, (_, aid)| live(*fid, *aid, 1));
        }
    }

    /// Pushes claims (`SPid::pmt`), the CA mask and the section PIDs of one adapter.
    fn sync_adapter(&mut self, sched: &Scheduler, aid: usize) {
        let Some(ca) = self.adapters.get_mut(&aid) else {
            return;
        };
        let mut wanted: BTreeMap<usize, Vec<u16>> = BTreeMap::new();
        let mut pids_all = false;
        let mut ca_active = false;
        if let Some(adapter) = sched.adapter(aid) {
            for entry in adapter.pid_entries() {
                if !matches!(entry.state, PidState::Active | PidState::New) {
                    continue;
                }
                if entry.pid == PID_ALL && !entry.sids.is_empty() {
                    pids_all = true;
                }
                let running = sched
                    .pmt(entry.pmt)
                    .is_some_and(|pmt| matches!(pmt.state, PmtState::Running | PmtState::Starting));
                if running && let Ok(pid) = u16::try_from(entry.pid) {
                    wanted.entry(entry.pmt as usize).or_default().push(pid);
                }
            }
            ca_active = sched.adapter_ca_mask(aid) != 0;
        }
        let stale: Vec<usize> = ca
            .services
            .keys()
            .filter(|id| !wanted.contains_key(id))
            .copied()
            .collect();
        for pmt_id in stale {
            ca.services.remove(&pmt_id);
            let _ = ca
                .descrambler
                .send(DescramblerCommand::RemoveService { pmt_id });
        }
        for (pmt_id, mut pids) in wanted {
            pids.sort_unstable();
            if ca.services.get(&pmt_id) != Some(&pids) {
                let _ = ca.descrambler.send(DescramblerCommand::UpdatePids {
                    pmt_id,
                    pids: pids.clone(),
                });
                ca.services.insert(pmt_id, pids);
            }
        }
        if ca.ca_active != ca_active {
            ca.ca_active = ca_active;
            let _ = ca
                .descrambler
                .send(DescramblerCommand::SetCaActive(ca_active));
        }
        if ca.pids_all != pids_all {
            ca.pids_all = pids_all;
            let _ = ca
                .descrambler
                .send(DescramblerCommand::SetPidsAll(pids_all));
        }
    }

    /// Closes the DVBAPI connection on shutdown.
    pub fn shutdown(&mut self) {
        if let Some(dvbapi) = self.dvbapi.as_mut() {
            dvbapi.conn = None;
        }
    }
}

/// Per-filter section assembly and matching (`process_filter`).
#[derive(Default)]
struct SectionRouter {
    filters: BTreeMap<i32, RouterFilter>,
}

struct RouterFilter {
    adapter: i32,
    pid: i32,
    matched: bool,
    assembler: SectionAssembler,
    emm: EmmAssembler,
}

impl RouterFilter {
    fn new(adapter: i32, pid: i32) -> Self {
        Self {
            adapter,
            pid,
            matched: false,
            assembler: SectionAssembler::new(false),
            emm: EmmAssembler::default(),
        }
    }
}

/// `assemble_emm`: whole 183-byte payloads; one buffer may hold several EMMs.
#[derive(Default)]
struct EmmAssembler {
    data: Vec<u8>,
}

impl EmmAssembler {
    /// `FILTER_PACKET_SIZE`.
    const MAX: usize = 4000;

    fn push(&mut self, b: &[u8]) -> Option<&[u8]> {
        if b[4] == 0 && (0x82..=0x8F).contains(&b[5]) {
            self.data.clear();
        } else if self.data.len() + 183 > Self::MAX {
            self.data.clear();
            return None;
        }
        self.data.extend_from_slice(&b[5..188]);
        let len = emm_len(&self.data);
        (len > 0 && self.data.len() >= len).then(|| &self.data[..len])
    }
}

/// `getEMMlen`: bytes covered by consecutive 0x80-0x8F sections.
fn emm_len(b: &[u8]) -> usize {
    let mut i = 0;
    while i + 3 <= b.len() && (0x80..=0x8F).contains(&b[i]) {
        i += ((usize::from(b[i + 1] & 0xF) << 8) | usize::from(b[i + 2])) + 3;
    }
    i
}

impl SectionRouter {
    /// Feeds one packet to every filter on its PID; returns completed sections.
    fn feed(&mut self, sched: &Scheduler, aid: i32, b: &[u8]) -> Vec<(i32, Vec<u8>)> {
        let mut out = Vec::new();
        if b.len() < 188 || b[0] != 0x47 {
            return out;
        }
        let pid = (i32::from(b[1] & 0x1F) << 8) | i32::from(b[2]);
        for id in sched.filters_for_pid(aid, pid) {
            let Some(f) = sched.filter_slot(id) else {
                continue;
            };
            let state = self
                .filters
                .entry(id)
                .or_insert_with(|| RouterFilter::new(aid, pid));
            if state.adapter != aid || state.pid != pid {
                *state = RouterFilter::new(aid, pid);
            }
            let emm = f.flags & FILTER_EMM != 0;
            if b[1] & 0x40 != 0 {
                state.matched = !emm && match_filter(&f.filter, &f.mask, f.flags, &b[5..]);
            }
            if !state.matched && !emm {
                continue;
            }
            let assembled = if emm {
                state.emm.push(b)
            } else {
                state.assembler.assemble_packet(b)
            };
            let Some(section) = assembled else {
                continue;
            };
            if f.flags & FILTER_CRC != 0 && crc32_mpeg2(section) != 0 {
                continue;
            }
            if !emm {
                out.push((id, section.to_vec()));
                continue;
            }
            // EMM payloads hold several sections, matched one by one.
            let mut i = 0;
            while i + 3 <= section.len() {
                if !(0x80..=0x8F).contains(&section[i]) {
                    break;
                }
                let len =
                    (usize::from(section[i + 1] & 0xF) << 8) + usize::from(section[i + 2]) + 3;
                let end = (i + len).min(section.len());
                if match_filter(&f.filter, &f.mask, f.flags, &section[i..end]) {
                    out.push((id, section[i..end].to_vec()));
                }
                i += len;
            }
        }
        out
    }

    /// Drops state of filters that no longer exist.
    fn retain(&mut self, sched: &Scheduler) {
        self.filters.retain(|id, state| {
            sched
                .filter_slot(*id)
                .is_some_and(|f| f.enabled && f.adapter == state.adapter && f.pid == state.pid)
        });
    }

    fn forget_adapter(&mut self, aid: i32) {
        self.filters.retain(|_, state| state.adapter != aid);
    }
}

/// `match_filter`: byte 0 is the table id, the rest skips the length field.
fn match_filter(filter: &[u8; 16], mask: &[u8; 16], flags: i32, b: &[u8]) -> bool {
    let mask_len = mask.iter().rposition(|&m| m != 0).map_or(0, |i| i + 1);
    let mut matched = true;
    for i in 0..mask_len {
        let idx = if i == 0 { 0 } else { i + 2 };
        let want = filter[i] & mask[i];
        if b.get(idx).is_none_or(|&byte| byte & mask[i] != want) {
            matched = false;
            break;
        }
    }
    if flags & FILTER_REVERSE != 0 {
        !matched
    } else {
        matched
    }
}

type HwRegistryFactory = Box<dyn Fn(usize, i32, i32) -> CipherRegistry>;

/// The registered DDCI CA and what was mirrored into the scheduler.
struct DdciLink {
    hub: DdciHub,
    ca_id: i32,
    mirrored: BTreeSet<(usize, u16)>,
    /// Scheduler filter id -> DDCI CAT filter id and its adapter.
    cat_filters: BTreeMap<i32, (usize, i32)>,
}

/// The registered EN 50221 CA.
struct DvbcaState {
    dvbca: Rc<Dvbca>,
    caids: Rc<RefCell<BTreeMap<usize, Vec<u16>>>>,
    pending: PendingOffers,
    ca_id: i32,
}

/// DVBAPI state shared by the runtime and the scheduler's CA sink.
struct DvbapiShared {
    client: DvbapiClient,
}

/// The DVBAPI client, its socket and the filter ids it owns.
struct Dvbapi {
    shared: Rc<RefCell<DvbapiShared>>,
    cfg: DvbapiConfig,
    conn: Option<Stream>,
    connecting: Option<Receiver<io::Result<Stream>>>,
    ca_id: Option<i32>,
    /// Scheduler filter id → DVBAPI filter id, and back.
    filters: BTreeMap<i32, FilterId>,
    /// Adapter and PID each scheduler filter was created for.
    filter_owner: BTreeMap<i32, (i32, i32)>,
    by_id: BTreeMap<FilterId, i32>,
}

impl Dvbapi {
    fn new(cfg: DvbapiConfig) -> Self {
        Self {
            shared: Rc::new(RefCell::new(DvbapiShared {
                client: DvbapiClient::new(cfg.clone()),
            })),
            cfg,
            conn: None,
            connecting: None,
            ca_id: None,
            filters: BTreeMap::new(),
            filter_owner: BTreeMap::new(),
            by_id: BTreeMap::new(),
        }
    }

    /// `connect_dvbapi`: connects in a helper thread so the control loop never blocks.
    fn poll(&mut self, now: i64, adapters_active: bool) {
        let action = self.shared.borrow_mut().client.poll(now, adapters_active);
        if action == PollAction::Connect && self.connecting.is_none() && self.conn.is_none() {
            let (tx, rx) = mpsc::channel();
            let cfg = self.cfg.clone();
            let spawned = thread::Builder::new()
                .name("dvbapi-connect".to_owned())
                .spawn(move || {
                    let _ = tx.send(transport::connect(&cfg, CONNECT_TIMEOUT));
                });
            if spawned.is_ok() {
                self.connecting = Some(rx);
            }
        }
    }

    /// Completes a pending connect, reads everything available and writes queued messages.
    fn service_socket(&mut self, now: i64) {
        if let Some(rx) = self.connecting.as_ref() {
            match rx.try_recv() {
                Ok(Ok(stream)) => {
                    self.connecting = None;
                    if stream.set_nonblocking(true).is_ok() {
                        self.conn = Some(stream);
                        self.shared.borrow_mut().client.on_connected(now);
                    }
                }
                Ok(Err(error)) => {
                    self.connecting = None;
                    crate::sys::log_error(format_args!("minisatip: dvbapi connect: {error}"));
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.connecting = None,
            }
        }
        let Some(conn) = self.conn.as_mut() else {
            return;
        };
        let mut buf = [0u8; 8192];
        let mut closed = false;
        loop {
            match conn.read(&mut buf) {
                Ok(0) => {
                    closed = true;
                    break;
                }
                Ok(n) => self.shared.borrow_mut().client.on_data(&buf[..n], now),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => {
                    closed = true;
                    break;
                }
            }
        }
        if closed {
            self.close(now);
        }
    }

    /// Writes queued messages; a failed write or a close request ends the connection.
    fn flush(&mut self, now: i64) {
        loop {
            let message = self.shared.borrow_mut().client.take_output();
            let Some(message) = message else {
                break;
            };
            let Some(conn) = self.conn.as_mut() else {
                break;
            };
            if write_all(conn, &message).is_err() {
                self.close(now);
                return;
            }
        }
        if self.shared.borrow().client.close_requested() {
            self.close(now);
        }
    }

    fn close(&mut self, now: i64) {
        self.conn = None;
        self.shared.borrow_mut().client.on_closed(now);
    }
}

fn write_all(conn: &mut Stream, mut data: &[u8]) -> io::Result<()> {
    let deadline = Instant::now() + WRITE_TIMEOUT;
    while !data.is_empty() {
        match conn.write(data) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(n) => data = &data[n..],
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                if Instant::now() > deadline {
                    return Err(io::ErrorKind::TimedOut.into());
                }
                thread::sleep(Duration::from_millis(1));
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// The DVBAPI `SCA_op` (`dvbapi_add_pmt`/`dvbapi_del_pmt`).
struct DvbapiSink {
    shared: Rc<RefCell<DvbapiShared>>,
}

/// Records every offer so the PMT's next CW is re-validated (`disable_cw`, tables.cpp:330).
struct Offering<S> {
    inner: S,
    offered: Rc<RefCell<Vec<(usize, usize)>>>,
}

impl<S> Offering<S> {
    fn new(inner: S, offered: &Rc<RefCell<Vec<(usize, usize)>>>) -> Self {
        Self {
            inner,
            offered: Rc::clone(offered),
        }
    }
}

impl<S: CaSink> CaSink for Offering<S> {
    fn add_pmt(&mut self, service: &CaService, update: bool) -> TablesResult {
        let result = self.inner.add_pmt(service, update);
        self.offered
            .borrow_mut()
            .push((service.adapter, service.pmt_id));
        result
    }

    fn del_pmt(&mut self, service: &CaService) -> TablesResult {
        self.inner.del_pmt(service)
    }

    fn add_pid(&mut self, service: &CaService, pid: i32) {
        self.inner.add_pid(service, pid);
    }

    fn del_pid(&mut self, service: &CaService, pid: i32) {
        self.inner.del_pid(service, pid);
    }

    fn accepts_service(&self, service: &CaService) -> bool {
        self.inner.accepts_service(service)
    }
}

impl CaSink for DvbapiSink {
    fn add_pmt(&mut self, service: &CaService, update: bool) -> TablesResult {
        let mut shared = self.shared.borrow_mut();
        let now = shared.client.now();
        match shared.client.add_service(service, update, now) {
            Ok(()) => TablesResult::Ok,
            Err(_) => TablesResult::Retry,
        }
    }

    fn del_pmt(&mut self, service: &CaService) -> TablesResult {
        let mut shared = self.shared.borrow_mut();
        let now = shared.client.now();
        shared.client.remove_service(service.pmt_id, now);
        TablesResult::Ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_filter_skips_the_section_length() {
        let mut filter = [0u8; 16];
        let mut mask = [0u8; 16];
        filter[0] = 0x02;
        mask[0] = 0xFF;
        filter[1] = 0x00;
        filter[2] = 0x64;
        mask[1] = 0xFF;
        mask[2] = 0xFF;
        let section = [0x02, 0xB0, 0x20, 0x00, 0x64, 0xC1];
        assert!(match_filter(&filter, &mask, 0, &section));
        assert!(!match_filter(&filter, &mask, FILTER_REVERSE, &section));
        let other = [0x02, 0xB0, 0x20, 0x00, 0x65, 0xC1];
        assert!(!match_filter(&filter, &mask, 0, &other));
        assert!(match_filter(&[0; 16], &[0; 16], 0, &other));
    }
}

#[cfg(test)]
mod wiring_tests {
    use super::*;
    use ms_sched::pmt::{FILTER_CRC, FILTER_PERMANENT};
    use ms_sched::{AdapterConfig, AdapterTuner, TuneError};
    use ms_tuner::{DeliverySystem, TuneParams};

    struct NoTune;

    impl AdapterTuner for NoTune {
        fn tune(&mut self, _params: &TuneParams) -> Result<(), TuneError> {
            Ok(())
        }
    }

    fn sched() -> Scheduler {
        let mut sched = Scheduler::new();
        sched.add_adapter(
            0,
            AdapterConfig::new(DeliverySystem::DvbS2),
            Box::new(NoTune),
        );
        sched
    }

    fn packet(pid: u16, payload: &[u8]) -> Vec<u8> {
        let mut b = vec![0xFF; 188];
        b[..5].copy_from_slice(&[0x47, 0x40 | (pid >> 8) as u8, pid as u8, 0x10, 0]);
        b[5..5 + payload.len()].copy_from_slice(payload);
        b
    }

    fn section(mut body: Vec<u8>) -> Vec<u8> {
        let len = body.len() + 1;
        body[1] = 0xB0 | (len >> 8) as u8;
        body[2] = len as u8;
        let crc = crc32_mpeg2(&body);
        body.extend_from_slice(&crc.to_be_bytes());
        body
    }

    fn pat() -> Vec<u8> {
        section(vec![0, 0, 0, 0, 1, 0xC1, 0, 0, 0, 0x64, 0xE1, 0x00])
    }

    fn pmt() -> Vec<u8> {
        section(vec![
            2, 0, 0, 0, 0x64, 0xC1, 0, 0, 0xE1, 0x01, 0xF0, 0x06, //
            9, 4, 0x0B, 0x00, 0xE5, 0x00, //
            2, 0xE1, 0x01, 0xF0, 0,
        ])
    }

    #[test]
    fn router_drops_bad_crc_and_splits_packed_emms() {
        let mut sched = sched();
        let mut router = SectionRouter::default();
        let pat_filter = sched.add_filter(0, 0, FilterTarget::Pat, FILTER_PERMANENT | FILTER_CRC);
        let good = packet(0, &pat());
        assert_eq!(router.feed(&sched, 0, &good), vec![(pat_filter, pat())]);
        let mut bad = pat();
        bad[8] ^= 1;
        assert!(router.feed(&sched, 0, &packet(0, &bad)).is_empty());

        let mut filter = [0u8; 16];
        let mut mask = [0u8; 16];
        filter[0] = 0x82;
        mask[0] = 0xFF;
        let emm = sched.add_filter_mask(0, 0x600, FilterTarget::Other, FILTER_EMM, &filter, &mask);
        let mut payload = vec![0x82, 0x70, 0x02, 0xAA, 0xBB];
        payload.extend_from_slice(&[0x83, 0x70, 0x01, 0xCC]);
        payload.extend_from_slice(&[0x82, 0x70, 0x01, 0xDD]);
        let out = router.feed(&sched, 0, &packet(0x600, &payload));
        assert_eq!(
            out,
            vec![
                (emm, vec![0x82, 0x70, 0x02, 0xAA, 0xBB]),
                (emm, vec![0x82, 0x70, 0x01, 0xDD]),
            ],
            "every matching EMM of the payload is delivered"
        );
    }

    #[test]
    fn claims_reach_the_descrambler_as_a_diff() {
        let mut sched = sched();
        let mut ca = CaRuntime::new(&Options::default());
        let (tx, rx) = mpsc::channel();
        ca.adapters.insert(
            0,
            AdapterCa {
                descrambler: tx,
                services: BTreeMap::new(),
                ca_active: false,
                pids_all: false,
                stats: DescramblerStats::default(),
                pending_stats: None,
            },
        );
        for pid in [0, 0x100, 0x101] {
            sched.mark_pid_add(0, 1, pid);
        }
        sched.post_tune_adapter(0);
        let packets = [packet(0, &pat()), packet(0x100, &pmt())].concat();
        ca.on_table_packets(&mut sched, &[], 0, 0, &packets);
        let commands: Vec<DescramblerCommand> = rx.try_iter().collect();
        assert!(
            commands.iter().any(|c| matches!(
                c,
                DescramblerCommand::UpdatePids { pids, .. } if pids == &[0x101]
            )),
            "the running PMT's claim must be pushed"
        );
        // The same state again sends nothing.
        ca.after_scheduler(&mut sched, &[]);
        assert_eq!(rx.try_iter().count(), 0);
        // The client leaves: the PMT stops and its service goes away.
        for pid in [0x100, 0x101] {
            sched.unmark_pid(0, 1, pid);
        }
        sched.update_adapter_pids(0);
        ca.after_scheduler(&mut sched, &[]);
        let commands: Vec<DescramblerCommand> = rx.try_iter().collect();
        assert!(
            commands
                .iter()
                .any(|c| matches!(c, DescramblerCommand::RemoveService { .. }))
        );
    }

    #[test]
    fn stale_dvbapi_filter_ids_are_forgotten() {
        let opts = Options {
            dvbapi_host: "127.0.0.1".to_owned(),
            dvbapi_port: 9000,
            ..Options::default()
        };
        let mut sched = sched();
        let mut ca = CaRuntime::new(&opts);
        let fid = sched.add_filter(0, 0x500, FilterTarget::Other, FILTER_ADD_REMOVE);
        let dvbapi = ca.dvbapi.as_mut().unwrap();
        dvbapi.filters.insert(fid, FilterId(7));
        dvbapi.filter_owner.insert(fid, (0, 0x500));
        dvbapi.by_id.insert(FilterId(7), fid);
        ca.after_scheduler(&mut sched, &[]);
        assert!(ca.dvbapi.as_ref().unwrap().filters.contains_key(&fid));
        // A tune drops non-permanent filters and the slot is reused elsewhere.
        sched.del_filter(fid);
        let reused = sched.add_filter(0, 0x700, FilterTarget::Other, 0);
        assert_eq!(reused, fid);
        ca.after_scheduler(&mut sched, &[]);
        let dvbapi = ca.dvbapi.as_ref().unwrap();
        assert!(dvbapi.filters.is_empty() && dvbapi.by_id.is_empty());
    }
}
