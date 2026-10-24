//! EN 50221 CI slots (`dvbca`) on a worker thread, as slot resets block; CAPMTs
//! are built by `ms_ca::CaDevice` like `dvbca_process_pmt`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use ms_ca::{CaCaps, CaDriver, CaService, EsStream, Program, ProgramTable, TablesResult};
use ms_ca_drivers::en50221::device::{
    CaDevice as CiDevice, CaPmtSink, CaState, CcKeyBytes, HostEvent,
};
use ms_ca_drivers::en50221::{AdapterInfo, CaOpener, CaRegistry, Clock, InitResult};
use ms_config::Options;

/// `DEFAULT_CA_PMT` (ca.h:11).
const DEFAULT_CA_PMT: usize = 1;
/// Worker poll period for CAM replies.
const POLL: Duration = Duration::from_millis(10);
/// `ca_timeout` and `ca_reconnect` run every second.
const TIMER_MS: i64 = 1000;
/// Longest wait of the control thread for a CAPMT result; later results
/// are picked up by the next offer of the same PMT.
const REPLY_TIMEOUT: Duration = Duration::from_millis(20);

/// Per-slot settings from `-3`, `-C` and `-c`.
#[derive(Clone, Debug)]
pub struct DvbcaSettings {
    pins: BTreeMap<usize, String>,
    force_ci: BTreeSet<usize>,
    /// `(max_ca_pmt, multiple_pmt, forced caids)`.
    channels: BTreeMap<usize, (usize, usize, Vec<u16>)>,
    /// `opts.enigma`: `/dev/ciN` with bare SPDUs.
    enigma: bool,
    /// `opts.cache_dir` for the CI+ authdata files.
    cache_dir: String,
}

impl Default for DvbcaSettings {
    fn default() -> Self {
        Self {
            pins: BTreeMap::new(),
            force_ci: BTreeSet::new(),
            channels: BTreeMap::new(),
            enigma: false,
            cache_dir: "/var/cache/minisatip".to_owned(),
        }
    }
}

impl DvbcaSettings {
    pub fn from_options(opts: &Options) -> Self {
        let mut settings = Self::default();
        for pin in &opts.ca_pins {
            settings
                .pins
                .insert(usize::from(pin.adapter), pin.pin.clone());
        }
        settings
            .force_ci
            .extend(opts.force_ci.iter().map(|a| usize::from(*a)));
        for c in &opts.ca_channels {
            let caids = c.caids.iter().map(|caid| *caid as u16).collect();
            settings.channels.insert(
                usize::from(c.adapter),
                (
                    usize::try_from(c.max_ca_pmt).unwrap_or(0),
                    usize::try_from(c.multiple_pmt).unwrap_or(0),
                    caids,
                ),
            );
        }
        settings.enigma = opts.enigma;
        settings.cache_dir = opts.cache_dir.clone();
        settings
    }

    /// `(max_ca_pmt, multiple_pmt)` of a slot (`-c`, else `DEFAULT_CA_PMT`).
    pub fn limits(&self, id: usize) -> (usize, usize) {
        self.channels
            .get(&id)
            .map_or((DEFAULT_CA_PMT, 0), |(max, multiple, _)| (*max, *multiple))
    }
}

/// Messages to the worker.
enum Request {
    Init(AdapterInfo),
    AddPmt {
        program: Box<Program>,
        update: bool,
        reply: Sender<TablesResult>,
    },
    DelPmt {
        adapter: usize,
        pmt_id: i32,
    },
    /// The adapters' pid tables for the `added_only` CAPMT filter.
    RequestedPids(BTreeSet<(u16, u16)>),
    Stop,
}

/// Reports from the worker to the control thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    /// `dvbca_init_dev` succeeded: the CA handles this adapter.
    Usable(usize),
    /// CA_INFO: the CAM descrambles these CAIDs (`add_caid_mask`).
    Caids(usize, Vec<u16>),
    /// `ca_release_pmts`: these PMTs must be offered again.
    Released(Vec<i32>),
    /// The device's `ca_sym` fields changed (status page).
    State(usize, DeviceView),
    /// `send_cw_to_all_pmts`: a CI+ key for these PMT ids. The bytes are never printed.
    CcKey {
        pmt_ids: Vec<usize>,
        parity: u8,
        key: CcKeyBytes,
        iv: CcKeyBytes,
    },
    /// `disable_cws_for_all_pmts`: stop descrambling these PMTs.
    DisableCws(Vec<usize>),
}

/// `ca_devices[i]` fields read by `ca_sym` (ca.cpp:4234-4242).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeviceView {
    pub enabled: bool,
    /// `CA_STATE_*`: 0 inactive, 1 active, 2 initialized.
    pub state: i32,
    pub caids: Vec<u16>,
    pub ci_name: String,
}

/// Control-thread handle of the worker.
pub struct Dvbca {
    requests: Sender<Request>,
    notices: Receiver<Notice>,
    thread: Option<JoinHandle<()>>,
}

impl Dvbca {
    /// Starts the worker with `opener` for the CA device nodes.
    pub fn spawn(
        settings: DvbcaSettings,
        opener: Box<dyn CaOpener + Send>,
        clock: Arc<dyn Clock>,
    ) -> std::io::Result<Self> {
        let (requests, rx) = mpsc::channel();
        let (tx, notices) = mpsc::channel();
        let thread = thread::Builder::new()
            // ca.cpp:4076 names the CI poller thread `CA_poll`.
            .name("CA_poll".to_owned())
            .spawn(move || {
                ms_log::set_thread_name("CA_poll");
                Worker::new(settings, opener, clock, tx).run(&rx)
            })?;
        Ok(Self {
            requests,
            notices,
            thread: Some(thread),
        })
    }

    /// `dvbca_init_dev` for an opened adapter; the result arrives as a notice.
    pub fn init_dev(&self, info: AdapterInfo) {
        let _ = self.requests.send(Request::Init(info));
    }

    /// Next pending notice.
    pub fn try_notice(&self) -> Option<Notice> {
        self.notices.try_recv().ok()
    }

    /// A handle over a scripted notice channel, for the control-thread tests.
    #[cfg(test)]
    pub(crate) fn from_notices(notices: Receiver<Notice>) -> Self {
        let (requests, _) = mpsc::channel();
        Self {
            requests,
            notices,
            thread: None,
        }
    }

    /// Queues `dvbca_process_pmt`; `None` when the worker is gone.
    fn add_pmt(&self, program: Program, update: bool) -> Option<Receiver<TablesResult>> {
        let (reply, rx) = mpsc::channel();
        self.requests
            .send(Request::AddPmt {
                program: Box::new(program),
                update,
                reply,
            })
            .ok()
            .map(|()| rx)
    }

    fn del_pmt(&self, adapter: usize, pmt_id: i32) {
        let _ = self.requests.send(Request::DelPmt { adapter, pmt_id });
    }

    /// Replaces the requested `(adapter, pid)` set the worker's CAPMTs filter on.
    pub fn set_requested_pids(&self, pids: BTreeSet<(u16, u16)>) {
        let _ = self.requests.send(Request::RequestedPids(pids));
    }

    /// Withdraws pending offers `keep` rejects; the worker runs requests in
    /// order, so a late CAPMT is followed by its release.
    pub fn sweep_pending(&self, pending: &PendingOffers, keep: impl Fn(usize, &Program) -> bool) {
        let stale: Vec<usize> = pending
            .borrow()
            .iter()
            .filter(|(id, offer)| !keep(**id, &offer.program))
            .map(|(id, _)| *id)
            .collect();
        for id in stale {
            if let Some(offer) = pending.borrow_mut().remove(&id) {
                self.del_pmt(usize::from(offer.program.adapter), id as i32);
            }
        }
    }
}

impl Drop for Dvbca {
    fn drop(&mut self) {
        let _ = self.requests.send(Request::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// An offer the worker has not answered yet, with the program it was for.
pub struct PendingOffer {
    rx: Receiver<TablesResult>,
    program: Program,
}

/// Unanswered offers by PMT id, shared with the runtime's sweep (DEV-214).
pub type PendingOffers = std::rc::Rc<std::cell::RefCell<BTreeMap<usize, PendingOffer>>>;

/// The `dvbca` `SCA_op` registered in the scheduler.
pub struct DvbcaSink {
    dvbca: std::rc::Rc<Dvbca>,
    /// CAIDs reported by each CAM (`ca[dvbca_id].ad_info`).
    caids: std::rc::Rc<std::cell::RefCell<BTreeMap<usize, Vec<u16>>>>,
    pending: PendingOffers,
}

impl DvbcaSink {
    pub fn new(
        dvbca: std::rc::Rc<Dvbca>,
        caids: std::rc::Rc<std::cell::RefCell<BTreeMap<usize, Vec<u16>>>>,
        pending: PendingOffers,
    ) -> Self {
        Self {
            dvbca,
            caids,
            pending,
        }
    }
}

impl ms_sched::pmt::CaSink for DvbcaSink {
    /// `dvbca_add_pid` (ca.cpp:4040): the next PMT section re-sends the CAPMT.
    fn add_pid(&mut self, service: &CaService, pid: i32) -> bool {
        ms_log::log!(
            "PMT {}, pid {} was added, invalidating pmt",
            service.pmt_id,
            pid
        );
        true
    }

    fn add_pmt(&mut self, service: &CaService, update: bool) -> TablesResult {
        let program = program_from_service(service);
        let id = service.pmt_id;
        // A late answer for the same program is the result of the CAPMT already
        // sent; an offer for another program in the slot is released first.
        let mut pending = self.pending.borrow_mut();
        if let Some(offer) = pending.get(&id) {
            if same_offer(&offer.program, &program) {
                return match offer.rx.try_recv() {
                    Ok(result) => {
                        pending.remove(&id);
                        result
                    }
                    Err(mpsc::TryRecvError::Empty) => TablesResult::Retry,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        pending.remove(&id);
                        TablesResult::NoRetry
                    }
                };
            }
            if let Some(old) = pending.remove(&id) {
                self.dvbca
                    .del_pmt(usize::from(old.program.adapter), id as i32);
            }
        }
        let Some(rx) = self.dvbca.add_pmt(program.clone(), update) else {
            return TablesResult::NoRetry;
        };
        match rx.recv_timeout(REPLY_TIMEOUT) {
            Ok(result) => result,
            Err(_) => {
                pending.insert(id, PendingOffer { rx, program });
                TablesResult::Retry
            }
        }
    }

    fn del_pmt(&mut self, service: &CaService) -> TablesResult {
        self.pending.borrow_mut().remove(&service.pmt_id);
        self.dvbca.del_pmt(service.adapter, service.pmt_id as i32);
        TablesResult::Ok
    }

    /// `send_pmt_to_ca`: a CAM that reported CAIDs only gets matching PMTs.
    fn accepts_service(&self, service: &CaService) -> bool {
        self.caids
            .borrow()
            .get(&service.adapter)
            .is_none_or(|caids| {
                caids.is_empty() || service.ca.iter().any(|ca| caids.contains(&ca.caid))
            })
    }
}

/// Whether `a` and `b` are the same CA offer. SDT names and fields arrive
/// independently of the PMT and do not change the CAPMT, so they are ignored.
fn same_offer(a: &Program, b: &Program) -> bool {
    let strip = |p: &Program| Program {
        name: String::new(),
        provider: String::new(),
        sdt: None,
        ..p.clone()
    };
    strip(a) == strip(b)
}

/// The CAPMT view of a scheduler PMT.
pub(crate) fn program_from_service(service: &CaService) -> Program {
    let mut program = Program {
        id: service.pmt_id as i32,
        adapter: service.adapter as u16,
        sid: service.sid,
        pid: service.pmt_pid,
        pcr_pid: service.pcr_pid,
        name: service.name.clone(),
        provider: service.provider.clone(),
        sdt: service.sdt,
        ..Program::default()
    };
    for ca in &service.ca {
        program.add_caid(ca.caid, ca.pid, &ca.private_data);
    }
    program.streams = service
        .streams
        .iter()
        .filter(|s| s.stream_type != 0)
        .map(|s| {
            let av = service.av_pids.contains(&s.pid);
            EsStream {
                stream_type: s.stream_type,
                pid: s.pid,
                is_audio: av && !ms_psi::is_video_stream_type(s.stream_type),
                is_video: av && ms_psi::is_video_stream_type(s.stream_type),
                descriptors: Vec::new(),
            }
        })
        .collect();
    program
}

/// `CaDriver` over one CI slot's CA support session.
struct SlotDriver<'a>(&'a mut CiDevice);

impl CaDriver for SlotDriver<'_> {
    fn capabilities(&self) -> CaCaps {
        CaCaps::default()
    }

    fn has_ca_session(&self) -> bool {
        CaPmtSink::has_ca_session(self.0)
    }

    fn send_ca_pmt(&mut self, body: &[u8]) -> bool {
        CaPmtSink::send_ca_pmt(self.0, body)
    }
}

/// Every valid PMT id of the CAPMT slots (`send_cw_to_all_pmts`).
fn capmt_pmt_ids(capmt: &ms_ca::CaDevice) -> Vec<usize> {
    let mut ids = Vec::new();
    for slot in capmt.capmt.iter().take(capmt.max_ca_pmt) {
        for id in std::iter::once(slot.pmt_id).chain(slot.other_id) {
            if ms_ca::capmt::pmt_id_is_valid(id)
                && let Ok(id) = usize::try_from(id)
            {
                ids.push(id);
            }
        }
    }
    ids
}

struct Worker {
    settings: DvbcaSettings,
    registry: CaRegistry,
    opener: Box<dyn CaOpener + Send>,
    clock: Arc<dyn Clock>,
    capmt: BTreeMap<usize, ms_ca::CaDevice>,
    programs: ProgramTable,
    adapters: BTreeMap<usize, AdapterInfo>,
    usable: BTreeSet<usize>,
    /// Last `DeviceView` sent per device.
    views: BTreeMap<usize, DeviceView>,
    /// `d->linked_adapter`: the adapter each enigma2 CI is routed to; C++
    /// zero-allocates the device, so a CI starts linked to adapter 0.
    linked: BTreeMap<usize, i32>,
    notices: Sender<Notice>,
    next_timer: i64,
}

impl Worker {
    fn new(
        settings: DvbcaSettings,
        opener: Box<dyn CaOpener + Send>,
        clock: Arc<dyn Clock>,
        notices: Sender<Notice>,
    ) -> Self {
        let mut registry = CaRegistry::new(Arc::clone(&clock));
        registry.enigma = settings.enigma;
        registry.set_cache_dir(&settings.cache_dir);
        for (id, pin) in &settings.pins {
            registry.device(*id).set_pin(pin.as_bytes());
        }
        for id in &settings.force_ci {
            registry.device(*id).force_ci = true;
        }
        for (id, (_, _, caids)) in &settings.channels {
            if !caids.is_empty() {
                registry.device(*id).set_forced_caids(caids);
            }
        }
        Self {
            settings,
            registry,
            opener,
            clock,
            capmt: BTreeMap::new(),
            programs: ProgramTable::new(),
            adapters: BTreeMap::new(),
            usable: BTreeSet::new(),
            views: BTreeMap::new(),
            linked: BTreeMap::new(),
            notices,
            next_timer: 0,
        }
    }

    fn run(mut self, requests: &Receiver<Request>) {
        loop {
            match requests.recv_timeout(POLL) {
                Ok(Request::Stop) | Err(RecvTimeoutError::Disconnected) => return,
                Ok(request) => self.handle(request),
                Err(RecvTimeoutError::Timeout) => {}
            }
            self.service();
        }
    }

    fn handle(&mut self, request: Request) {
        match request {
            Request::Init(info) => {
                let Ok(id) = usize::try_from(info.id) else {
                    return;
                };
                self.adapters.insert(id, info);
                // A Retry leaves the slot enabled for the reconnect poller.
                let result = self.registry.init_dev(&info, self.opener.as_mut());
                // ca.cpp:3936-3941: on enigma2 an adapter without its own CI
                // uses the registered ones, routed to it per PMT.
                let own_ci = self.registry.get(info.id).is_some_and(CiDevice::has_link);
                if matches!(result, InitResult::Ok)
                    && self.registry.enigma
                    && !own_ci
                    && self.usable.insert(id)
                {
                    let _ = self.notices.send(Notice::Usable(id));
                }
            }
            Request::AddPmt {
                program,
                update,
                reply,
            } => {
                let _ = reply.send(self.add_pmt(*program, update));
            }
            Request::DelPmt { adapter, pmt_id } => {
                // get_capmt_for_pmt: the CI holding the PMT, which on enigma2
                // need not be the adapter's own.
                let device = self.device_with_pmt(pmt_id).unwrap_or(adapter);
                if let (Some(capmt), Some(dev)) = (
                    self.capmt.get_mut(&device),
                    self.registry.get_mut(device as i32),
                ) {
                    capmt.del_pmt(pmt_id, &self.programs, &mut SlotDriver(dev));
                }
                self.programs.del(pmt_id);
            }
            Request::RequestedPids(pids) => self.programs.set_requested_pids(pids),
            Request::Stop => {}
        }
    }

    /// `dvbca_process_pmt` (ca.cpp:1232-1280). On enigma2 the CI is the one
    /// whose CAIDs match, routed to the PMT's adapter first.
    fn add_pmt(&mut self, program: Program, update: bool) -> TablesResult {
        let adapter = usize::from(program.adapter);
        let pmt_id = program.id;
        let mut device = adapter;
        // None: the CI was not moved for this call (`prev_linked == -2`).
        let mut prev_linked = None;
        if self.registry.enigma {
            let Some(found) = self.find_dvbca_for_pmt(&program) else {
                return if self.any_ca_initializing() {
                    TablesResult::Retry
                } else {
                    TablesResult::NoRetry
                };
            };
            device = found;
            let linked = self.linked.get(&device).copied().unwrap_or(0);
            prev_linked = Some(linked);
            // Detach the CI from the previous adapter.
            if linked != adapter as i32 {
                self.set_tuner_input(-1, linked);
            }
            self.set_tuner_input(device as i32, adapter as i32);
            self.set_input_ci(device, adapter as i32);
            self.linked.insert(device, adapter as i32);
        }
        let (Some(dev), Some(capmt)) = (
            self.registry.get_mut(device as i32),
            self.capmt.get_mut(&device),
        ) else {
            return TablesResult::NoRetry;
        };
        if dev.state != CaState::Initialized {
            self.restore_ci_link(device, adapter as i32, prev_linked);
            ms_log::log!("CAM not yet initialized");
            return TablesResult::Retry;
        }
        self.programs.insert(program);
        let result = capmt.process_pmt(pmt_id, update, &self.programs, &mut SlotDriver(dev));
        // ca.cpp:1265,1276: a PMT that found no place puts the CI back.
        if result == TablesResult::Retry && capmt.slot_of(pmt_id).is_none() {
            self.restore_ci_link(device, adapter as i32, prev_linked);
        }
        result
    }

    /// `find_dvbca_for_pmt` (ca.cpp:1186): the first initialised CI with a
    /// CAID that matches the program.
    fn find_dvbca_for_pmt(&self, program: &Program) -> Option<usize> {
        self.capmt.iter().find_map(|(id, capmt)| {
            let ready = self
                .registry
                .get(*id as i32)
                .is_some_and(|dev| dev.state == CaState::Initialized);
            let matched = capmt
                .caid_filter
                .iter()
                .any(|(caid, mask)| program.match_caid(*caid, *mask));
            (ready && matched).then(|| {
                ms_log::log!("Found CA {} for PMT {}", id, program.id);
                *id
            })
        })
    }

    /// `any_ca_initializing`: an enabled CI that has not finished its init.
    fn any_ca_initializing(&self) -> bool {
        self.adapters.keys().any(|id| {
            self.registry
                .get(*id as i32)
                .is_some_and(|dev| dev.enabled && dev.state != CaState::Initialized)
        })
    }

    /// The initialised CI whose CAPMT slots hold `pmt_id` (`get_capmt_for_pmt`).
    fn device_with_pmt(&self, pmt_id: i32) -> Option<usize> {
        self.capmt.iter().find_map(|(id, capmt)| {
            let ready = self
                .registry
                .get(*id as i32)
                .is_some_and(|dev| dev.enabled && dev.state == CaState::Initialized);
            (ready && capmt.slot_of(pmt_id).is_some()).then_some(*id)
        })
    }

    /// `restore_ci_link` (ca.cpp:1216): routes the CI back to the adapter it
    /// served before this call.
    fn restore_ci_link(&mut self, device: usize, adapter: i32, prev_linked: Option<i32>) {
        let Some(prev) = prev_linked.filter(|prev| *prev != adapter) else {
            return;
        };
        self.set_tuner_input(-1, adapter);
        if prev < 0 {
            self.linked.insert(device, -1);
            return;
        }
        self.set_tuner_input(device as i32, prev);
        self.set_input_ci(device, prev);
        self.linked.insert(device, prev);
    }

    /// `set_input_ci` (ca.cpp:1141): the CI reads from the adapter's tuner.
    fn set_input_ci(&mut self, ci: usize, adapter: i32) {
        let filename = format!("/proc/stb/tsmux/ci{ci}_input");
        let value = [b'A'.wrapping_add(adapter as u8)];
        ms_log::log!("set_input_ci: {} -> {}", filename, char::from(value[0]));
        if self.opener.write_file(&filename, &value).is_err() {
            ms_log::log!("set_input_ci open failed, {}", filename);
        }
    }

    /// `set_tuner_input` (ca.cpp:1155): the adapter reads through the CI, or
    /// straight from its tuner when `ci` is -1.
    fn set_tuner_input(&mut self, ci: i32, adapter: i32) {
        if adapter == -1 {
            return;
        }
        let value = if ci == -1 {
            vec![b'A'.wrapping_add(adapter as u8)]
        } else {
            format!("CI{ci}").into_bytes()
        };
        let filename = format!("/proc/stb/tsmux/input{adapter}");
        ms_log::log!(
            "set_tuner_input: {} -> {}",
            filename,
            String::from_utf8_lossy(&value)
        );
        if self.opener.write_file(&filename, &value).is_err() {
            ms_log::log!("set_tuner_input:  open failed, {}", filename);
        }
    }

    /// Reads the CAMs, runs the 1 s timers and forwards host events.
    fn service(&mut self) {
        let now = self.clock.tick_ms();
        let ids: Vec<usize> = self.adapters.keys().copied().collect();
        for id in &ids {
            let Some(dev) = self.registry.get_mut(*id as i32) else {
                continue;
            };
            if dev.has_link() && !dev.service() {
                dev.teardown();
            }
            if dev.take_close_request() {
                dev.teardown();
            }
        }
        if now >= self.next_timer {
            self.next_timer = now + TIMER_MS;
            for id in &ids {
                if let Some(dev) = self.registry.get_mut(*id as i32)
                    && dev.has_link()
                {
                    dev.timeout();
                }
            }
            let adapters = self.adapters.clone();
            self.registry
                .reconnect(|i| adapters.get(&i).copied(), self.opener.as_mut());
        }
        for id in ids {
            self.forward_events(id);
            self.publish_view(id);
        }
    }

    /// Sends the device's status-page fields when they changed.
    fn publish_view(&mut self, id: usize) {
        let Some(dev) = self.registry.get(id as i32) else {
            return;
        };
        let view = DeviceView {
            enabled: dev.enabled,
            state: match dev.state {
                ms_ca_drivers::en50221::CaState::Inactive => 0,
                ms_ca_drivers::en50221::CaState::Active => 1,
                ms_ca_drivers::en50221::CaState::Initialized => 2,
            },
            caids: dev.caids().to_vec(),
            ci_name: dev.ci_name(),
        };
        if self.views.get(&id) != Some(&view) {
            self.views.insert(id, view.clone());
            let _ = self.notices.send(Notice::State(id, view));
        }
    }

    fn forward_events(&mut self, id: usize) {
        let (max_ca_pmt, multiple) = self.settings.limits(id);
        loop {
            let Some(dev) = self.registry.get_mut(id as i32) else {
                return;
            };
            if dev.enabled && dev.has_link() && self.usable.insert(id) {
                let _ = self.notices.send(Notice::Usable(id));
            }
            let Some(event) = dev.next_event() else {
                return;
            };
            let capmt = self
                .capmt
                .entry(id)
                .or_insert_with(|| ms_ca::CaDevice::new(id as i32, max_ca_pmt, multiple));
            match event {
                HostEvent::Opened => {
                    *capmt = ms_ca::CaDevice::new(id as i32, max_ca_pmt, multiple);
                    capmt.state = ms_ca::CaState::Inactive;
                }
                HostEvent::CaInfo { caids } => {
                    capmt.state = ms_ca::CaState::Initialized;
                    capmt.caid_filter = caids.iter().map(|c| (*c, 0xFFFF)).collect();
                    for slot in capmt.capmt.iter_mut() {
                        slot.capmt_hash = 0;
                        slot.capmt_hash_valid = false;
                        slot.forget_sent();
                    }
                    let _ = self.notices.send(Notice::Caids(id, caids));
                }
                HostEvent::ReleasePmts => {
                    capmt.state = ms_ca::CaState::Inactive;
                    let released = capmt.release_pmts();
                    if !released.is_empty() {
                        let _ = self.notices.send(Notice::Released(released));
                    }
                }
                HostEvent::CaSessionClosed => capmt.state = ms_ca::CaState::Inactive,
                // The C++ MMI logs live in APP_MMI_handler (en50221/apps.rs).
                HostEvent::MmiMenu { .. } | HostEvent::MmiEnquiry { .. } => {}
                HostEvent::CcKey { parity, key, iv } => {
                    let pmt_ids = capmt_pmt_ids(capmt);
                    let _ = self.notices.send(Notice::CcKey {
                        pmt_ids,
                        parity,
                        key,
                        iv,
                    });
                }
                HostEvent::CcDisableCws => {
                    let pmt_ids = capmt_pmt_ids(capmt);
                    let _ = self.notices.send(Notice::DisableCws(pmt_ids));
                }
                HostEvent::CloseRequested => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ms_ca::{CaDescriptor, StreamPid};
    use ms_ca_drivers::en50221::link::{CA_CI_LINK, CA_CI_MODULE_PRESENT, CA_CI_MODULE_READY};
    use ms_ca_drivers::en50221::{CaLink, ManualClock, SlotInfo};
    use ms_fakecam::{CamConfig, FakeCam};
    use ms_sched::pmt::CaSink;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::io;
    use std::rc::Rc;
    use std::sync::Mutex;

    // ca.cpp:3932-3934: with opts.enigma the worker opens /dev/ciN, the
    // adapter id, instead of /dev/dvb/adapterN/caM.
    #[test]
    fn the_registry_follows_opts_enigma() {
        struct Paths(Arc<Mutex<Vec<String>>>);
        impl CaOpener for Paths {
            fn open(&mut self, path: &str) -> io::Result<Box<dyn CaLink>> {
                self.0.lock().unwrap().push(path.to_owned());
                Err(io::ErrorKind::NotFound.into())
            }
        }
        for (enigma, expected) in [(false, "/dev/dvb/adapter1/ca2"), (true, "/dev/ci3")] {
            let opts = Options {
                enigma,
                ..Options::default()
            };
            let paths = Arc::new(Mutex::new(Vec::new()));
            let dvbca = Dvbca::spawn(
                DvbcaSettings::from_options(&opts),
                Box::new(Paths(Arc::clone(&paths))),
                Arc::new(ManualClock::new(0, 0)),
            )
            .unwrap();
            dvbca.init_dev(AdapterInfo::dvb(3, 1, 2));
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while paths.lock().unwrap().is_empty() {
                assert!(std::time::Instant::now() < deadline, "no open");
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            assert_eq!(paths.lock().unwrap()[0], expected);
        }
    }
    use std::time::Instant;

    /// A CI slot whose CAM is an `ms-fakecam` instance.
    struct CamLink {
        cam: Arc<Mutex<FakeCam>>,
        rx: VecDeque<Vec<u8>>,
    }

    impl CaLink for CamLink {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let Some(msg) = self.rx.pop_front() else {
                return Err(io::ErrorKind::WouldBlock.into());
            };
            buf[..msg.len()].copy_from_slice(&msg);
            Ok(msg.len())
        }

        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if let Some(reply) = self.cam.lock().unwrap().handle_tpdu(buf) {
                self.rx.push_back(reply);
            }
            Ok(buf.len())
        }

        fn reset(&mut self) -> io::Result<()> {
            Ok(())
        }

        fn slot_info(&mut self) -> io::Result<SlotInfo> {
            Ok(SlotInfo {
                kind: CA_CI_LINK,
                flags: CA_CI_MODULE_PRESENT | CA_CI_MODULE_READY,
            })
        }

        fn sleep_ms(&mut self, _: u32) {}
    }

    struct CamOpener(Arc<Mutex<FakeCam>>);

    impl CaOpener for CamOpener {
        fn open(&mut self, _: &str) -> io::Result<Box<dyn CaLink>> {
            Ok(Box::new(CamLink {
                cam: Arc::clone(&self.0),
                rx: VecDeque::new(),
            }))
        }

        fn has_ciplus_root_cert(&self) -> bool {
            false
        }
    }

    fn service(caid: u16) -> CaService {
        CaService {
            pmt_id: 3,
            adapter: 0,
            sid: 0x64,
            pmt_pid: 0x100,
            ca: vec![CaDescriptor {
                caid,
                pid: 0x500,
                private_data: Vec::new(),
            }],
            streams: vec![StreamPid {
                stream_type: 2,
                pid: 0x101,
            }],
            av_pids: vec![0x101],
            ..CaService::default()
        }
    }

    fn wait_notice(dvbca: &Dvbca, want: impl Fn(&Notice) -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            match dvbca.try_notice() {
                Some(notice) if want(&notice) => return true,
                Some(_) => {}
                None => thread::sleep(Duration::from_millis(5)),
            }
        }
        false
    }

    // A name or SDT arriving during an offer must not release and re-send it.
    #[test]
    fn sdt_fields_do_not_change_an_offer() {
        let mut named = service(0x0B00);
        named.name = "Service 1".to_owned();
        named.sdt = Some(ms_ca::SdtInfo::default());
        let (a, b) = (
            program_from_service(&service(0x0B00)),
            program_from_service(&named),
        );
        assert_ne!(a, b);
        assert!(same_offer(&a, &b));
        assert!(!same_offer(&a, &program_from_service(&service(0x0100))));
    }

    #[test]
    fn capmt_reaches_the_fake_cam_through_the_sink() {
        let cam = Arc::new(Mutex::new(FakeCam::new(
            CamConfig::default(),
            Arc::new(ms_fakecam::ManualClock::new(0)),
        )));
        let mut opts = Options::default();
        opts.ca_pins.push(ms_config::CaPin {
            adapter: 0,
            pin: "1234".to_owned(),
        });
        let dvbca = Rc::new(
            Dvbca::spawn(
                DvbcaSettings::from_options(&opts),
                Box::new(CamOpener(Arc::clone(&cam))),
                Arc::new(ManualClock::new(0, 1_700_000_000)),
            )
            .unwrap(),
        );
        dvbca.init_dev(AdapterInfo::dvb(0, 0, 0));
        assert!(wait_notice(&dvbca, |n| *n == Notice::Usable(0)));
        assert!(wait_notice(&dvbca, |n| *n == Notice::Caids(0, vec![0x0B00])));
        // ca_sym: the status page reads enabled, state, CAIDs and the CAM name.
        assert!(wait_notice(&dvbca, |n| matches!(
            n,
            Notice::State(0, view)
                if view.enabled
                    && view.state == 2
                    && view.caids == [0x0B00]
                    && !view.ci_name.is_empty()
        )));

        let caids = Rc::new(RefCell::new(BTreeMap::from([(0, vec![0x0B00])])));
        let mut sink = DvbcaSink::new(Rc::clone(&dvbca), caids, Rc::default());
        assert!(!sink.accepts_service(&service(0x0100)));
        assert!(sink.accepts_service(&service(0x0B00)));
        assert_eq!(sink.add_pmt(&service(0x0B00), false), TablesResult::Ok);
        let programs: Vec<u16> = cam
            .lock()
            .unwrap()
            .programs()
            .programs()
            .iter()
            .map(|p| p.number())
            .collect();
        assert_eq!(programs, [0x64]);
    }

    /// Records the host control files the worker writes; no CI device opens.
    struct ProcLog(Arc<Mutex<Vec<(String, String)>>>);

    impl CaOpener for ProcLog {
        fn open(&mut self, _: &str) -> io::Result<Box<dyn CaLink>> {
            Err(io::ErrorKind::NotFound.into())
        }

        fn write_file(&mut self, path: &str, value: &[u8]) -> io::Result<()> {
            let value = String::from_utf8_lossy(value).into_owned();
            self.0.lock().unwrap().push((path.to_owned(), value));
            Ok(())
        }
    }

    type Writes = Arc<Mutex<Vec<(String, String)>>>;

    fn enigma_worker() -> (Worker, Writes, Receiver<Notice>) {
        let writes = Writes::default();
        let (tx, rx) = mpsc::channel();
        let opts = Options {
            enigma: true,
            ..Options::default()
        };
        let worker = Worker::new(
            DvbcaSettings::from_options(&opts),
            Box::new(ProcLog(Arc::clone(&writes))),
            Arc::new(ManualClock::new(0, 0)),
            tx,
        );
        (worker, writes, rx)
    }

    /// An initialised CI on `id` that reported `caid`, but with no CA session.
    fn ready_ci(worker: &mut Worker, id: usize, caid: u16) {
        let dev = worker.registry.device(id);
        dev.enabled = true;
        dev.state = CaState::Initialized;
        let mut capmt = ms_ca::CaDevice::new(id as i32, 1, 0);
        capmt.state = ms_ca::CaState::Initialized;
        capmt.caid_filter = vec![(caid, 0xFFFF)];
        worker.capmt.insert(id, capmt);
        worker
            .adapters
            .insert(id, AdapterInfo::dvb(id as i32, 0, id as u32));
    }

    fn program_on(adapter: u16, caid: u16) -> Program {
        let mut program = program_from_service(&service(caid));
        program.adapter = adapter;
        program
    }

    fn pairs(writes: &Writes) -> Vec<(String, String)> {
        std::mem::take(&mut *writes.lock().unwrap())
    }

    fn proc(name: &str, value: &str) -> (String, String) {
        (format!("/proc/stb/tsmux/{name}"), value.to_owned())
    }

    // ca.cpp:1238-1256 and 1216-1227: the matching CI is routed to the PMT's
    // adapter, detached from the previous one, and put back when the PMT
    // finds no place.
    #[test]
    fn enigma_routes_the_matching_ci_and_restores_it_on_failure() {
        let (mut worker, writes, _rx) = enigma_worker();
        ready_ci(&mut worker, 0, 0x0604);
        // A new CI starts linked to adapter 0 (alloc_ca_device zeroes it).
        // ca.cpp:1276: the only CAPMT slot is taken, so the PMT finds no place.
        worker.capmt.get_mut(&0).unwrap().capmt[0].pmt_id = 99;
        assert_eq!(
            worker.add_pmt(program_on(3, 0x0604), false),
            TablesResult::Retry
        );
        assert_eq!(
            pairs(&writes),
            [
                proc("input0", "A"),
                proc("input3", "CI0"),
                proc("ci0_input", "D"),
                proc("input3", "D"),
                proc("input0", "CI0"),
                proc("ci0_input", "A"),
            ]
        );
        assert_eq!(worker.linked.get(&0), Some(&0));

        // A free slot but no CA session: the failed first send also restores.
        worker.capmt.get_mut(&0).unwrap().capmt[0].pmt_id = -1;
        worker.linked.insert(0, 5);
        assert_eq!(
            worker.add_pmt(program_on(3, 0x0604), false),
            TablesResult::Retry
        );
        assert_eq!(worker.capmt[&0].slot_of(3), None);
        assert_eq!(
            pairs(&writes),
            [
                proc("input5", "F"),
                proc("input3", "CI0"),
                proc("ci0_input", "D"),
                proc("input3", "D"),
                proc("input5", "CI0"),
                proc("ci0_input", "F"),
            ]
        );
        assert_eq!(worker.linked.get(&0), Some(&5));
    }

    // find_dvbca_for_pmt / any_ca_initializing: no matching CI is final unless
    // another CI is still starting.
    #[test]
    fn enigma_without_a_matching_ci_retries_only_while_one_initialises() {
        let (mut worker, writes, _rx) = enigma_worker();
        ready_ci(&mut worker, 0, 0x0604);
        assert_eq!(
            worker.add_pmt(program_on(3, 0x0100), false),
            TablesResult::NoRetry
        );
        let starting = worker.registry.device(1);
        starting.enabled = true;
        worker.adapters.insert(1, AdapterInfo::dvb(1, 0, 1));
        assert_eq!(
            worker.add_pmt(program_on(3, 0x0100), false),
            TablesResult::Retry
        );
        assert!(pairs(&writes).is_empty(), "no CI, no routing");
    }

    // ca.cpp:3936-3941: with a CI registered, an adapter without its own CI
    // device is enabled for the CA.
    #[test]
    fn enigma_adapters_without_a_ci_use_the_registered_ones() {
        let (mut worker, _writes, rx) = enigma_worker();
        worker.handle(Request::Init(AdapterInfo::dvb(4, 0, 4)));
        assert!(rx.try_recv().is_err(), "no CI registered yet");
        worker.registry.has_ci = true;
        worker.handle(Request::Init(AdapterInfo::dvb(5, 0, 5)));
        assert_eq!(rx.try_recv().ok(), Some(Notice::Usable(5)));
    }

    // get_capmt_for_pmt: a PMT is found on whichever initialised CI holds it.
    #[test]
    fn a_pmt_is_found_on_the_ci_that_holds_it() {
        let (mut worker, _writes, _rx) = enigma_worker();
        ready_ci(&mut worker, 0, 0x0604);
        ready_ci(&mut worker, 2, 0x0B00);
        worker.capmt.get_mut(&2).unwrap().capmt[0].pmt_id = 7;
        assert_eq!(worker.device_with_pmt(7), Some(2));
        assert_eq!(worker.device_with_pmt(8), None);
    }

    #[test]
    fn missing_slot_is_never_usable() {
        struct NoDevice;
        impl CaOpener for NoDevice {
            fn open(&mut self, _: &str) -> io::Result<Box<dyn CaLink>> {
                Err(io::ErrorKind::NotFound.into())
            }
        }
        let dvbca = Rc::new(
            Dvbca::spawn(
                DvbcaSettings::default(),
                Box::new(NoDevice),
                Arc::new(ManualClock::new(0, 0)),
            )
            .unwrap(),
        );
        dvbca.init_dev(AdapterInfo::dvb(0, 0, 0));
        let mut sink = DvbcaSink::new(Rc::clone(&dvbca), Rc::default(), Rc::default());
        assert_eq!(sink.add_pmt(&service(0x0B00), false), TablesResult::NoRetry);
        assert_eq!(dvbca.try_notice(), None);
    }
}

#[cfg(test)]
mod option_tests {
    use super::*;
    use ms_config::{CaChannels, CaPin};

    #[test]
    fn ca_options_map_to_slot_settings() {
        let mut opts = Options::default();
        opts.ca_pins.push(CaPin {
            adapter: 1,
            pin: "0000".to_owned(),
        });
        opts.force_ci = vec![2];
        opts.ca_channels.push(CaChannels {
            adapter: 1,
            multiple_pmt: 2,
            max_ca_pmt: 3,
            caids: vec![0x0B00, 0x1702],
        });
        let settings = DvbcaSettings::from_options(&opts);
        assert_eq!(settings.pins.get(&1).map(String::as_str), Some("0000"));
        assert!(settings.force_ci.contains(&2));
        assert_eq!(settings.limits(1), (3, 2));
        assert_eq!(settings.limits(0), (DEFAULT_CA_PMT, 0));
        assert_eq!(settings.channels[&1].2, vec![0x0B00, 0x1702]);
    }
}

#[cfg(test)]
mod pending_tests {
    use super::*;
    use ms_sched::pmt::CaSink;
    use std::rc::Rc;

    /// A `Dvbca` whose worker is the test itself.
    fn manual() -> (Rc<Dvbca>, Receiver<Request>) {
        let (requests, rx) = mpsc::channel();
        let (_tx, notices) = mpsc::channel();
        let dvbca = Dvbca {
            requests,
            notices,
            thread: None,
        };
        (Rc::new(dvbca), rx)
    }

    fn service(sid: u16) -> CaService {
        CaService {
            pmt_id: 4,
            adapter: 0,
            sid,
            pmt_pid: 0x100,
            ..CaService::default()
        }
    }

    #[test]
    fn late_results_only_count_for_the_same_program() {
        let (dvbca, worker) = manual();
        let pending = PendingOffers::default();
        let mut sink = DvbcaSink::new(Rc::clone(&dvbca), Rc::default(), Rc::clone(&pending));

        // The worker is busy: the offer is pending and asks for a retry.
        assert_eq!(sink.add_pmt(&service(0x64), false), TablesResult::Retry);
        let Ok(Request::AddPmt { reply: first, .. }) = worker.try_recv() else {
            panic!("first CAPMT request");
        };
        assert_eq!(sink.add_pmt(&service(0x64), false), TablesResult::Retry);
        assert!(worker.try_recv().is_err(), "no second CAPMT while pending");

        // Another program in the same PMT slot: release the old, send the new.
        assert_eq!(sink.add_pmt(&service(0x65), false), TablesResult::Retry);
        assert!(matches!(
            worker.try_recv(),
            Ok(Request::DelPmt {
                adapter: 0,
                pmt_id: 4
            })
        ));
        let Ok(Request::AddPmt { program, reply, .. }) = worker.try_recv() else {
            panic!("CAPMT for the new program");
        };
        assert_eq!(program.sid, 0x65);
        // The stale answer is ignored; the new one is reported once.
        let _ = first.send(TablesResult::Ok);
        reply.send(TablesResult::Ok).unwrap();
        assert_eq!(sink.add_pmt(&service(0x65), false), TablesResult::Ok);
        assert!(pending.borrow().is_empty());
    }

    #[test]
    fn sweep_releases_offers_of_vanished_pmts() {
        let (dvbca, worker) = manual();
        let pending = PendingOffers::default();
        let mut sink = DvbcaSink::new(Rc::clone(&dvbca), Rc::default(), Rc::clone(&pending));
        assert_eq!(sink.add_pmt(&service(0x64), false), TablesResult::Retry);
        let _ = worker.try_recv();
        dvbca.sweep_pending(&pending, |_, program| program.sid == 0x64);
        assert_eq!(pending.borrow().len(), 1, "a live PMT keeps its offer");
        dvbca.sweep_pending(&pending, |_, _| false);
        assert!(pending.borrow().is_empty());
        assert!(matches!(
            worker.try_recv(),
            Ok(Request::DelPmt {
                adapter: 0,
                pmt_id: 4
            })
        ));
    }
}

/// `send_cw_to_all_pmts` end to end: the worker must forward a CI+ key for the
/// PMTs it has offered to the CAM.
#[cfg(all(test, feature = "ci-plus"))]
mod ciplus_worker_tests {
    use super::*;
    use ms_ca::{CaDescriptor, StreamPid};
    use ms_ca_drivers::en50221::link::{CA_CI_LINK, CA_CI_MODULE_PRESENT, CA_CI_MODULE_READY};
    use ms_ca_drivers::en50221::{CaLink, ManualClock, SlotInfo};
    use ms_fakecam::tags::CIPLUS_CC;
    use ms_fakecam::{CamConfig, FakeCam};
    use std::collections::VecDeque;
    use std::io;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    const UNIX: u64 = 1_700_000_000;

    struct CamLink {
        cam: Arc<Mutex<FakeCam>>,
        rx: VecDeque<Vec<u8>>,
    }

    impl CaLink for CamLink {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let Some(msg) = self.rx.pop_front() else {
                return Err(io::ErrorKind::WouldBlock.into());
            };
            assert!(msg.len() <= buf.len(), "R_TPDU fits the host buffer");
            buf[..msg.len()].copy_from_slice(&msg);
            Ok(msg.len())
        }

        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let mut cam = self.cam.lock().unwrap();
            if let Some(reply) = cam.handle_tpdu(buf) {
                self.rx.push_back(reply);
            }
            Ok(buf.len())
        }

        fn reset(&mut self) -> io::Result<()> {
            Ok(())
        }

        fn slot_info(&mut self) -> io::Result<SlotInfo> {
            Ok(SlotInfo {
                kind: CA_CI_LINK,
                flags: CA_CI_MODULE_PRESENT | CA_CI_MODULE_READY,
            })
        }

        fn sleep_ms(&mut self, _: u32) {}
    }

    struct CiOpener {
        cam: Arc<Mutex<FakeCam>>,
        cert_dir: PathBuf,
    }

    impl CaOpener for CiOpener {
        fn open(&mut self, _: &str) -> io::Result<Box<dyn CaLink>> {
            Ok(Box::new(CamLink {
                cam: Arc::clone(&self.cam),
                rx: VecDeque::new(),
            }))
        }

        fn ciplus_cert_dir(&self) -> String {
            self.cert_dir.to_string_lossy().into_owned()
        }

        fn has_ciplus_root_cert(&self) -> bool {
            true
        }
    }

    fn service() -> CaService {
        CaService {
            pmt_id: 3,
            adapter: 0,
            sid: 0x64,
            pmt_pid: 0x100,
            ca: vec![CaDescriptor {
                caid: 0x0B00,
                pid: 0x500,
                private_data: Vec::new(),
            }],
            streams: vec![StreamPid {
                stream_type: 2,
                pid: 0x101,
            }],
            av_pids: vec![0x101],
            ..CaService::default()
        }
    }

    fn drain(rx: &Receiver<Notice>) -> Vec<Notice> {
        std::iter::from_fn(|| rx.try_recv().ok()).collect()
    }

    #[test]
    fn ciplus_keys_are_forwarded_for_offered_pmts() {
        let pki = ms_fakecam::ciplus::test_pki::pki("dvbca-worker");
        let mut config = CamConfig::default();
        config.resources.push(CIPLUS_CC);
        config.ciplus = Some(pki.cam.clone());
        let cam = Arc::new(Mutex::new(FakeCam::new(
            config,
            Arc::new(ms_fakecam::ManualClock::new(0)),
        )));
        let clock = Arc::new(ManualClock::new(0, UNIX));
        let worker_clock: Arc<dyn Clock> = clock.clone();
        let (tx, rx) = mpsc::channel();
        let opts = Options {
            cache_dir: pki.cache_dir.to_string_lossy().into_owned(),
            ..Options::default()
        };
        let mut worker = Worker::new(
            DvbcaSettings::from_options(&opts),
            Box::new(CiOpener {
                cam: Arc::clone(&cam),
                cert_dir: pki.cert_dir.clone(),
            }),
            worker_clock,
            tx,
        );
        worker.handle(Request::Init(AdapterInfo::dvb(0, 0, 0)));
        worker.service();
        let _ = drain(&rx);

        let program = program_from_service(&service());
        let mut offered = false;
        for _ in 0..10 {
            let (reply, answers) = mpsc::channel();
            worker.handle(Request::AddPmt {
                program: Box::new(program.clone()),
                update: false,
                reply,
            });
            if answers.recv().ok() == Some(TablesResult::Ok) {
                offered = true;
                break;
            }
            worker.service();
        }
        assert!(offered, "the CAM accepts the offered PMT");
        let _ = drain(&rx);

        // The handshake already delivered a key in register 0; the update must
        // prove it is a new one and not a replay of the init key.
        let init = cam.lock().unwrap().ciplus_status().expect("CI+ CAM");
        let init_key = init.expected_key.expect("the handshake sent a key");
        let init_iv = init.expected_iv.expect("the handshake sent an IV");
        assert_eq!(init.expected_parity, Some(0), "the handshake key register");

        assert!(
            cam.lock().unwrap().send_sac_key_request(1, false),
            "the CAM sends a key update"
        );
        let mut notices = Vec::new();
        for _ in 0..5 {
            clock.advance(1001);
            worker.service();
            notices.extend(drain(&rx));
            if notices.iter().any(
                |notice| matches!(notice, Notice::CcKey { pmt_ids, .. } if !pmt_ids.is_empty()),
            ) {
                break;
            }
        }

        let disable = notices
            .iter()
            .position(|notice| matches!(notice, Notice::DisableCws(_)))
            .expect("disable_cws_for_all_pmts");
        let key_pos = notices
            .iter()
            .position(|notice| matches!(notice, Notice::CcKey { .. }))
            .expect("a CI+ key");
        assert!(
            disable < key_pos,
            "the SAC data request disables descrambling first"
        );
        let Notice::CcKey {
            pmt_ids,
            parity,
            key,
            iv,
        } = &notices[key_pos]
        else {
            panic!("matched above")
        };
        assert!(
            pmt_ids.contains(&(program.id as usize)),
            "the offered PMT is addressed: {pmt_ids:?}"
        );
        let status = cam.lock().unwrap().ciplus_status().expect("CI+ CAM");
        assert_eq!(
            *parity, 1,
            "the forwarded key is the post-init register 1 update"
        );
        assert_eq!(*parity, status.expected_parity.expect("parity"));
        assert!(
            key.0 == status.expected_key.expect("key"),
            "the forwarded key matches the CAM"
        );
        assert!(
            iv.0 == status.expected_iv.expect("iv"),
            "the forwarded IV matches the CAM"
        );
        assert_ne!(key.0, init_key, "the forwarded key is fresh");
        assert_ne!(iv.0, init_iv, "the forwarded IV is fresh");
        pki.remove();
    }
}
