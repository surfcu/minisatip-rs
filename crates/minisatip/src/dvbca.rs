//! EN 50221 CI slots (`dvbca`) on a worker thread, as slot resets block; CAPMTs
//! are built by `ms_ca::CaDevice` like `dvbca_process_pmt`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use ms_ca::{CaCaps, CaDriver, CaService, EsStream, Program, ProgramTable, TablesResult};
use ms_ca_drivers::en50221::device::{
    ApduSink, CaDevice as CiDevice, CaPmtSink, CaState, CcKeyBytes, HostEvent, UnsolicitedMmi,
};
use ms_ca_drivers::en50221::{
    AdapterInfo, CaOpener, CaRegistry, Clock, InitResult, TransponderKey,
};
use ms_config::Options;
use ms_net::web::CamMenuOp;

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
    /// `--ci-plus-recovery` in ms; 0 disables the CI+ key watchdog.
    ci_plus_recovery_ms: i64,
    /// `--cam-recovery` in ms; 0 disables the scrambled-output watchdog.
    pub cam_recovery_ms: i64,
    /// Zap guard of `--cam-recovery`; off with `--cam-recovery-no-zap-guard`.
    pub cam_recovery_zap_guard: bool,
    /// `--cam-menu-unsolicited`.
    unsolicited_mmi: UnsolicitedMmi,
    /// `--cam-menu-snapshot` paths.
    menu_snapshot: Vec<Vec<(u64, u8)>>,
    /// `--ddci-capture` APDU trace.
    apdu_sink: Option<ApduHook>,
}

/// An [`ApduSink`] that can sit in the `Debug` settings.
#[derive(Clone)]
struct ApduHook(ApduSink);

impl std::fmt::Debug for ApduHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApduHook(..)")
    }
}

impl Default for DvbcaSettings {
    fn default() -> Self {
        Self {
            pins: BTreeMap::new(),
            force_ci: BTreeSet::new(),
            channels: BTreeMap::new(),
            enigma: false,
            cache_dir: "/var/cache/minisatip".to_owned(),
            ci_plus_recovery_ms: 0,
            unsolicited_mmi: UnsolicitedMmi::default(),
            menu_snapshot: Vec::new(),
            apdu_sink: None,
            cam_recovery_ms: 0,
            cam_recovery_zap_guard: true,
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
        settings.ci_plus_recovery_ms = i64::from(opts.ci_plus_recovery).max(0) * 1000;
        settings.menu_snapshot = opts.cam_menu_snapshot.clone();
        settings.unsolicited_mmi = match opts.cam_menu_unsolicited.as_str() {
            "answer0" => UnsolicitedMmi::Answer0,
            "open" => UnsolicitedMmi::Open,
            _ => UnsolicitedMmi::Answer1,
        };
        settings.cam_recovery_ms = i64::from(opts.cam_recovery).max(0) * 1000;
        settings.cam_recovery_zap_guard = !opts.cam_recovery_no_zap_guard;
        settings
    }

    /// Sends every slot's APDU headers to `sink` (`--ddci-capture`).
    #[cfg_attr(not(feature = "diag-log"), allow(dead_code))]
    pub fn set_apdu_sink(&mut self, sink: ApduSink) {
        self.apdu_sink = Some(ApduHook(sink));
    }

    /// `(max_ca_pmt, multiple_pmt)` of a slot (`-c`, else `DEFAULT_CA_PMT`).
    pub fn limits(&self, id: usize) -> (usize, usize) {
        self.channels
            .get(&id)
            .map_or((DEFAULT_CA_PMT, 0), |(max, multiple, _)| (*max, *multiple))
    }
}

#[cfg(test)]
pub(crate) type SequencedScrambledWindow = (usize, u64, u64, u64, i64);

#[cfg(test)]
pub(crate) type ServiceWindowItem = (u16, Option<TransponderKey>, u64, u64);

#[cfg(test)]
pub(crate) type SequencedServiceWindowReport = (usize, u64, i64, Vec<ServiceWindowItem>);

/// Reads back the scrambled and service windows a test `Dvbca` was sent without dropping either.
#[cfg(test)]
pub(crate) struct WindowRecorder {
    rx: Receiver<Request>,
    scrambled: std::cell::RefCell<Vec<SequencedScrambledWindow>>,
    services: std::cell::RefCell<Vec<SequencedServiceWindowReport>>,
}

#[cfg(test)]
impl WindowRecorder {
    fn new(rx: Receiver<Request>) -> Self {
        Self {
            rx,
            scrambled: std::cell::RefCell::new(Vec::new()),
            services: std::cell::RefCell::new(Vec::new()),
        }
    }

    fn pump(&self) {
        for request in self.rx.try_iter() {
            match request {
                Request::ScrambledWindow {
                    adapter,
                    seq,
                    scrambled,
                    clear,
                    window_ms,
                } => self
                    .scrambled
                    .borrow_mut()
                    .push((adapter, seq, scrambled, clear, window_ms)),
                Request::ServiceWindows {
                    adapter,
                    seq,
                    window_ms,
                    services,
                } => self
                    .services
                    .borrow_mut()
                    .push((adapter, seq, window_ms, services)),
                _ => {}
            }
        }
    }

    /// The `(adapter, scrambled, clear, window_ms)` windows sent since the last drain, in order.
    pub(crate) fn windows(&self) -> Vec<(usize, u64, u64, i64)> {
        self.windows_with_seq()
            .into_iter()
            .map(|(adapter, _, scrambled, clear, window_ms)| (adapter, scrambled, clear, window_ms))
            .collect()
    }

    /// The `(adapter, seq, scrambled, clear, window_ms)` windows sent since the last drain.
    pub(crate) fn windows_with_seq(&self) -> Vec<SequencedScrambledWindow> {
        self.pump();
        std::mem::take(&mut *self.scrambled.borrow_mut())
    }

    /// The `(adapter, window_ms, services)` service windows sent since the last drain, in order.
    pub(crate) fn service_windows(&self) -> Vec<ServiceWindowReport> {
        self.service_windows_with_seq()
            .into_iter()
            .map(|(adapter, _, window_ms, services)| {
                (
                    adapter,
                    window_ms,
                    services
                        .into_iter()
                        .map(|(sid, _, scr, clr)| (sid, scr, clr))
                        .collect(),
                )
            })
            .collect()
    }

    /// The `(adapter, seq, window_ms, services)` service windows sent since the last drain.
    pub(crate) fn service_windows_with_seq(&self) -> Vec<SequencedServiceWindowReport> {
        self.pump();
        std::mem::take(&mut *self.services.borrow_mut())
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
    /// A `--cam-menu` command, in order with the CAPMT requests.
    Cam {
        ca: usize,
        op: CamMenuOp,
    },
    /// One window of an adapter's output: packets left scrambled and clear.
    ScrambledWindow {
        adapter: usize,
        seq: u64,
        scrambled: u64,
        clear: u64,
        window_ms: i64,
    },
    /// One window per service: its `(sid, tp_key, scrambled, clear)` counts.
    ServiceWindows {
        adapter: usize,
        seq: u64,
        window_ms: i64,
        services: Vec<(u16, Option<TransponderKey>, u64, u64)>,
    },
    Stop,
}

/// Reports from the worker to the control thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    /// `dvbca_init_dev` succeeded: the CA handles this adapter.
    Usable(usize),
    /// CA_INFO: the CAM descrambles these CAIDs (`add_caid_mask`).
    Caids(usize, Vec<u16>),
    /// `ca_release_pmts`: these `(PMT id, generation)` must be offered again.
    Released(Vec<(i32, u64)>),
    /// The device's `ca_sym` fields changed (status page).
    State(usize, DeviceView),
    /// `send_cw_to_all_pmts`: a CI+ key for these `(PMT id, generation)`.
    /// The bytes are never printed.
    CcKey {
        pmt_ids: Vec<(usize, u64)>,
        parity: u8,
        key: CcKeyBytes,
        iv: CcKeyBytes,
        /// CW lifetime: 3720 s for a new key, 3600 s for a replay on join.
        expiry_s: i64,
    },
    /// `disable_cws_for_all_pmts`: stop descrambling these `(PMT id, generation)`.
    DisableCws(Vec<(usize, u64)>),
    /// MMI menu or enquiry from the CAM in CA `id` (`--ddci-capture` trigger).
    MmiTrigger { id: usize, kind: &'static str },
}

/// `ca_devices[i]` fields read by `ca_sym` (ca.cpp:4234-4242).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeviceView {
    pub enabled: bool,
    /// `CA_STATE_*`: 0 inactive, 1 active, 2 initialized.
    pub state: i32,
    pub caids: Vec<u16>,
    pub ci_name: String,
    /// `--cam-menu` readiness for the web routes.
    pub ai_session: bool,
    pub mmi_session: bool,
    pub menu_open: bool,
}

/// Control-thread handle of the worker.
pub struct Dvbca {
    requests: Sender<Request>,
    notices: Receiver<Notice>,
    thread: Option<JoinHandle<()>>,
    /// Identifies this handle's entry in [`CAM_MENU`].
    serial: usize,
}

static SERIAL: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);

thread_local! {
    /// The control thread's worker for `--cam-menu` (the web routes run on this thread).
    static CAM_MENU: std::cell::RefCell<Option<(usize, Sender<Request>)>> =
        const { std::cell::RefCell::new(None) };
}

/// [`ms_net::web::CamMenuSend`]: queues `op` for CA `ca` on this thread's worker.
pub fn cam_menu_command(ca: usize, op: CamMenuOp) -> bool {
    CAM_MENU.with(|slot| {
        slot.borrow()
            .as_ref()
            .is_some_and(|(_, tx)| tx.send(Request::Cam { ca, op }).is_ok())
    })
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
        let serial = SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        CAM_MENU.with(|slot| *slot.borrow_mut() = Some((serial, requests.clone())));
        Ok(Self {
            requests,
            notices,
            thread: Some(thread),
            serial,
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
            serial: 0,
        }
    }

    /// A handle whose scrambled windows can be read back with the recorder.
    #[cfg(test)]
    pub(crate) fn recording() -> (Self, WindowRecorder) {
        let (requests, rx) = mpsc::channel();
        let (_, notices) = mpsc::channel();
        let dvbca = Self {
            requests,
            notices,
            thread: None,
            serial: 0,
        };
        (dvbca, WindowRecorder::new(rx))
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

    /// Reports one window of an adapter's output to its CAM (`--cam-recovery`).
    pub fn report_scrambled(
        &self,
        adapter: usize,
        seq: u64,
        scrambled: u64,
        clear: u64,
        window_ms: i64,
    ) {
        let _ = self.requests.send(Request::ScrambledWindow {
            adapter,
            seq,
            scrambled,
            clear,
            window_ms,
        });
    }

    /// Reports one window per service of an adapter (`--cam-recovery`).
    pub fn report_service_windows(
        &self,
        adapter: usize,
        seq: u64,
        window_ms: i64,
        services: Vec<(u16, Option<TransponderKey>, u64, u64)>,
    ) {
        let _ = self.requests.send(Request::ServiceWindows {
            adapter,
            seq,
            window_ms,
            services,
        });
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
        CAM_MENU.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot
                .as_ref()
                .is_some_and(|(serial, _)| *serial == self.serial)
            {
                *slot = None;
            }
        });
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
        generation: service.generation,
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
    // pmt.cpp:2216 keeps the independent PCR as a type-0 stream; create_capmt
    // filters it, ddci_create_pmt forwards it.
    program.streams = service
        .streams
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let av = service.av_pids.contains(&s.pid);
            EsStream {
                stream_type: s.stream_type,
                pid: s.pid,
                is_audio: av && !ms_psi::is_video_stream_type(s.stream_type),
                is_video: av && ms_psi::is_video_stream_type(s.stream_type),
                descriptors: service
                    .stream_descriptors
                    .get(i)
                    .cloned()
                    .unwrap_or_default(),
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

/// The PMT ids of a CAPMT, sorted: the composition the zap guard compares.
fn capmt_ids(capmt: &ms_ca::CaDevice) -> Vec<usize> {
    let mut ids = capmt_pmt_ids(capmt);
    ids.sort_unstable();
    ids
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

/// A notice's `(PMT id, generation)`: an unknown program cannot match a slot.
fn pmt_ref(programs: &ProgramTable, pmt_id: usize) -> (usize, u64) {
    (
        pmt_id,
        programs.get(pmt_id as i32).map_or(0, |p| p.generation),
    )
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
    /// The reconnect poller's 1 s timer (ca.cpp:4093).
    next_timer: i64,
    /// Each open CA socket's `ca_timeout` deadline: 1 s idle (`sockets_timeout(.., 1000)`,
    /// ca.cpp:3829), counted from link-up, the last read with data or the last timeout.
    next_timeout: BTreeMap<usize, i64>,
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
        registry.set_ci_plus_recovery_ms(settings.ci_plus_recovery_ms);
        registry.set_unsolicited_mmi(settings.unsolicited_mmi);
        registry.set_menu_snapshot(settings.menu_snapshot.clone());
        registry.set_apdu_sink(settings.apdu_sink.as_ref().map(|hook| hook.0.clone()));
        registry.set_cam_recovery_ms(settings.cam_recovery_ms);
        registry.set_cam_recovery_zap_guard(settings.cam_recovery_zap_guard);
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
            next_timeout: BTreeMap::new(),
        }
    }

    fn run(mut self, requests: &Receiver<Request>) {
        loop {
            let poll = if self
                .adapters
                .keys()
                .any(|&id| self.registry.get(id as i32).is_some_and(CiDevice::has_link))
            {
                POLL
            } else {
                Duration::from_millis(250)
            };
            match requests.recv_timeout(poll) {
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
                    let before = capmt_ids(capmt);
                    capmt.del_pmt(pmt_id, &self.programs, &mut SlotDriver(&mut *dev));
                    if capmt_ids(capmt) != before {
                        dev.cam_recovery_pmts_changed();
                    }
                }
                self.programs.del(pmt_id);
            }
            Request::RequestedPids(pids) => self.programs.set_requested_pids(pids),
            Request::Cam { ca, op } => self.cam_menu(ca, op),
            Request::ScrambledWindow {
                adapter,
                seq,
                scrambled,
                clear,
                window_ms,
            } => {
                // A window counts only while a CAPMT is in place on an initialised CAM.
                let active = self
                    .registry
                    .get(adapter as i32)
                    .is_some_and(|dev| dev.state == CaState::Initialized)
                    && self
                        .capmt
                        .get(&adapter)
                        .is_some_and(|capmt| !capmt_pmt_ids(capmt).is_empty());
                let Some(dev) = self.registry.get_mut(adapter as i32) else {
                    return;
                };
                dev.cam_recovery_window_seq(seq, window_ms, scrambled, clear, active);
                // Re-sent here, where `active` was just checked: the CAM has a CA session.
                if dev.take_cam_recovery()
                    && let Some(capmt) = self.capmt.get_mut(&adapter)
                {
                    capmt.resend_capmts(&self.programs, &mut SlotDriver(dev));
                }
            }
            Request::ServiceWindows {
                adapter,
                seq,
                window_ms,
                services,
            } => {
                // Only a real CAM re-init or an empty CAPMT makes a service window inactive.
                let active = self
                    .registry
                    .get(adapter as i32)
                    .is_some_and(|dev| dev.state == CaState::Initialized)
                    && self
                        .capmt
                        .get(&adapter)
                        .is_some_and(|capmt| !capmt_pmt_ids(capmt).is_empty());
                let mut pairs = Vec::new();
                if let Some(capmt) = self.capmt.get(&adapter) {
                    for pmt_id in capmt_pmt_ids(capmt) {
                        if let Some(program) = i32::try_from(pmt_id)
                            .ok()
                            .and_then(|id| self.programs.get(id))
                        {
                            pairs.push((pmt_id, program.sid, program.generation));
                        }
                    }
                }
                let Some(dev) = self.registry.get_mut(adapter as i32) else {
                    return;
                };
                dev.cam_recovery_sync_services(&pairs, active);
                if active {
                    for &(sid, tp_key, scrambled, clear) in &services {
                        dev.cam_recovery_service_window_seq(
                            seq, sid, tp_key, window_ms, scrambled, clear,
                        );
                    }
                }
                // Re-joined here, where `active` was just checked: an initialised CAM with a CAPMT.
                let due = dev.take_silent_recovery();
                if !due.is_empty()
                    && let Some(capmt) = self.capmt.get_mut(&adapter)
                {
                    for pmt_id in due {
                        capmt.rejoin_pmt(pmt_id, &self.programs, &mut SlotDriver(dev));
                    }
                }
                if active && let Some(dev) = self.registry.get_mut(adapter as i32) {
                    dev.cam_recovery_check_stuck();
                }
            }
            Request::Stop => {}
        }
    }

    /// Runs a `--cam-menu` command; the route checked a snapshot that may be stale.
    fn cam_menu(&mut self, ca: usize, op: CamMenuOp) {
        let Some(dev) = self.registry.get_mut(ca as i32) else {
            ms_log::log!("CA {}: menu command {:?} for a missing CA", ca, op);
            return;
        };
        let result = match op {
            CamMenuOp::Enter => dev.enter_menu(),
            CamMenuOp::Answer(choice) => dev.menu_answer(choice),
            CamMenuOp::Close => dev.close_mmi(),
        };
        if let Err(error) = result {
            ms_log::log!("CA {}: menu command {:?} not sent: {:?}", ca, op, error);
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
        let generation = program.generation;
        self.programs.insert(program);
        let before = capmt_ids(capmt);
        let result = capmt.process_pmt(pmt_id, update, &self.programs, &mut SlotDriver(&mut *dev));
        if capmt_ids(capmt) != before {
            dev.cam_recovery_pmts_changed();
        }
        // ca.cpp:1330-1335: a PMT that got its CAPMT also gets the stored CI+ keys.
        if result == TablesResult::Ok
            && let Ok(id) = usize::try_from(pmt_id)
        {
            for parity in 0..2u8 {
                let slot = usize::from(parity);
                if dev.key[slot] != [0; 16] {
                    let _ = self.notices.send(Notice::CcKey {
                        pmt_ids: vec![(id, generation)],
                        parity,
                        key: CcKeyBytes(dev.key[slot]),
                        iv: CcKeyBytes(dev.iv[slot]),
                        expiry_s: 3600,
                    });
                }
            }
        }
        // ca.cpp:1265,1276: a PMT that found no place puts the CI back.
        if result != TablesResult::Ok && capmt.slot_of(pmt_id).is_none() {
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
                dev.report_state("removed");
                dev.teardown();
            }
            if dev.take_close_request() {
                dev.teardown();
            }
        }
        for id in &ids {
            let Some(dev) = self.registry.get_mut(*id as i32) else {
                continue;
            };
            if !dev.has_link() {
                self.next_timeout.remove(id);
                continue;
            }
            let due = self.next_timeout.entry(*id).or_insert(now + TIMER_MS);
            // socketworks.cpp:896, 988: a read with data re-arms the idle timer.
            if dev.last_rx_ms >= 0 {
                *due = (*due).max(dev.last_rx_ms + TIMER_MS);
            }
            if now >= *due {
                *due = now + TIMER_MS;
                dev.timeout();
            }
        }
        if now >= self.next_timer {
            self.next_timer = now + TIMER_MS;
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
            ai_session: dev.has_ai_session(),
            mmi_session: dev.has_mmi_session(),
            menu_open: dev.menu_open(),
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
                    dev.cam_recovery_pmts_changed();
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
                    dev.cam_recovery_pmts_changed();
                    if !released.is_empty() {
                        let released = released
                            .into_iter()
                            .map(|id| (id, self.programs.get(id).map_or(0, |p| p.generation)))
                            .collect();
                        let _ = self.notices.send(Notice::Released(released));
                    }
                }
                HostEvent::CaSessionClosed => capmt.state = ms_ca::CaState::Inactive,
                // The C++ MMI logs live in APP_MMI_handler (en50221/apps.rs).
                // A requested page is no CAM error: no capture.
                HostEvent::MmiMenu {
                    solicited: false, ..
                } => {
                    let _ = self.notices.send(Notice::MmiTrigger { id, kind: "menu" });
                }
                HostEvent::MmiEnquiry {
                    solicited: false, ..
                } => {
                    let _ = self.notices.send(Notice::MmiTrigger {
                        id,
                        kind: "enquiry",
                    });
                }
                HostEvent::MmiMenu { .. } | HostEvent::MmiEnquiry { .. } => {}
                HostEvent::CcKey { parity, key, iv } => {
                    let pmt_ids = capmt_pmt_ids(capmt);
                    let pmt_refs: Vec<(usize, u64)> = pmt_ids
                        .iter()
                        .map(|id| pmt_ref(&self.programs, *id))
                        .collect();
                    ms_log::diag::events::ciplus_key(
                        i64::try_from(id).unwrap_or(0),
                        i64::from(parity),
                        || {
                            pmt_ids
                                .iter()
                                .filter_map(|pmt_id| self.programs.get(*pmt_id as i32))
                                .map(|program| u64::from(program.adapter))
                                .collect()
                        },
                    );
                    let _ = self.notices.send(Notice::CcKey {
                        pmt_ids: pmt_refs,
                        parity,
                        key,
                        iv,
                        expiry_s: 3720,
                    });
                }
                HostEvent::CcDisableCws => {
                    let pmt_ids = capmt_pmt_ids(capmt)
                        .into_iter()
                        .map(|id| pmt_ref(&self.programs, id))
                        .collect();
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

    // pmt.cpp:2216 keeps the independent PCR as a type-0 stream; ddci.cpp:944
    // forwards it while create_capmt filters it.
    #[test]
    fn independent_pcr_stream_survives_program_from_service() {
        let mut svc = service(0x0B00);
        svc.streams.push(StreamPid {
            stream_type: 0,
            pid: 0x1F0,
        });
        svc.pcr_pid = 0x1F0;
        let program = program_from_service(&svc);
        assert!(
            program
                .streams
                .iter()
                .any(|s| { s.stream_type == 0 && s.pid == 0x1F0 && !s.is_audio && !s.is_video })
        );
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

    /// Host-to-CAM APDUs with `tag` the fake CAM has seen.
    fn sent(cam: &Arc<Mutex<FakeCam>>, tag: u32) -> Vec<Vec<u8>> {
        use ms_fakecam::record::{Direction, Event};
        cam.lock()
            .unwrap()
            .recorder()
            .apdus()
            .into_iter()
            .filter_map(|r| match (r.dir, r.event) {
                (Direction::HostToCam, Event::Apdu { tag: t, body, .. }) if t == tag => Some(body),
                _ => None,
            })
            .collect()
    }

    // --cam-menu: the web callback reaches this thread's worker; a requested page is no capture trigger.
    #[test]
    fn cam_menu_commands_reach_the_worker_and_skip_the_capture() {
        use ms_ca_drivers::en50221::consts::{
            EN50221_APP_MMI_RESOURCEID, TAG_CLOSE_MMI, TAG_ENTER_MENU, TAG_MENU_ANSWER,
            TAG_MENU_LAST,
        };
        let cam = Arc::new(Mutex::new(FakeCam::new(
            CamConfig::default(),
            Arc::new(ms_fakecam::ManualClock::new(0)),
        )));
        let dvbca = Dvbca::spawn(
            DvbcaSettings::from_options(&Options::default()),
            Box::new(CamOpener(Arc::clone(&cam))),
            Arc::new(ms_ca_drivers::en50221::SystemClock::default()),
        )
        .unwrap();
        dvbca.init_dev(AdapterInfo::dvb(0, 0, 0));
        let triggers = std::cell::Cell::new(0);
        let until = |want: &dyn Fn(&Notice) -> bool| {
            wait_notice(&dvbca, |n| {
                triggers.set(triggers.get() + usize::from(matches!(n, Notice::MmiTrigger { .. })));
                want(n)
            })
        };
        assert!(until(&|n| matches!(
            n,
            Notice::State(0, v) if v.ai_session && v.mmi_session && !v.menu_open
        )));
        assert!(cam_menu_command(0, CamMenuOp::Enter));
        assert!(until(&|n| matches!(n, Notice::State(0, v) if v.menu_open)));
        assert_eq!(sent(&cam, TAG_ENTER_MENU), [Vec::<u8>::new()]);

        let mut page = vec![0x00];
        for text in ["Main", "", "", "Status"] {
            page.extend_from_slice(&[0x9F, 0x88, 0x03, text.len() as u8]);
            page.extend_from_slice(text.as_bytes());
        }
        assert!(cam.lock().unwrap().send_to_resource(
            EN50221_APP_MMI_RESOURCEID,
            TAG_MENU_LAST,
            &page
        ));
        // The page arrives on the next 1 s poll; until then the device refuses the answer.
        let deadline = Instant::now() + Duration::from_secs(5);
        while sent(&cam, TAG_MENU_ANSWER).is_empty() {
            assert!(
                Instant::now() < deadline,
                "the answer never reached the CAM"
            );
            assert!(cam_menu_command(0, CamMenuOp::Answer(1)));
            thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(sent(&cam, TAG_MENU_ANSWER), [vec![1]]);
        assert!(cam_menu_command(0, CamMenuOp::Close));
        assert!(until(&|n| matches!(n, Notice::State(0, v) if !v.menu_open)));
        assert_eq!(sent(&cam, TAG_CLOSE_MMI), [vec![0]]);

        assert!(cam.lock().unwrap().send_to_resource(
            EN50221_APP_MMI_RESOURCEID,
            TAG_MENU_LAST,
            &page
        ));
        assert!(until(&|n| matches!(
            n,
            Notice::MmiTrigger {
                id: 0,
                kind: "menu"
            }
        )));
        assert_eq!(
            triggers.get(),
            1,
            "only the unsolicited page triggers a capture"
        );
        drop(dvbca);
        assert!(
            !cam_menu_command(0, CamMenuOp::Close),
            "cleared with its worker"
        );
    }

    // DEV-151: a join, an unchanged update and a leave each emit one event.
    #[cfg(feature = "diag-log")]
    #[test]
    fn capmt_events_report_join_skip_and_leave() {
        let lines = super::diag_capture::lines();
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
        let caids = Rc::new(RefCell::new(BTreeMap::from([(0, vec![0x0B00])])));
        let mut sink = DvbcaSink::new(Rc::clone(&dvbca), caids, Rc::default());

        // PMT 21 is not used by the other unit tests in this binary, so their
        // CAPMT events cannot be mistaken for these.
        let mut program = service(0x0B00);
        program.pmt_id = 21;
        let ours = |line: &String| {
            line.contains("\"event\":\"capmt\"") && line.contains("\"pmt_ids\":[21]")
        };

        let start = lines
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len();
        assert_eq!(sink.add_pmt(&program, false), TablesResult::Ok);
        assert_eq!(sink.add_pmt(&program, true), TablesResult::Ok);
        assert_eq!(sink.del_pmt(&program), TablesResult::Ok);

        // `del_pmt` is asynchronous: wait for the worker to emit its event.
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let captured = lines
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if captured[start..].iter().filter(|line| ours(line)).count() == 3 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "not all capmt events: {captured:?}"
            );
            drop(captured);
            thread::sleep(Duration::from_millis(5));
        }

        let captured = lines
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let events: Vec<&String> = captured[start..].iter().filter(|line| ours(line)).collect();
        assert_eq!(events.len(), 3, "join, skipped update, leave: {events:?}");
        assert!(events[0].contains("\"reason\":\"join\""), "{}", events[0]);
        assert!(events[0].contains("\"skipped\":false"), "{}", events[0]);
        assert!(events[1].contains("\"reason\":\"update\""), "{}", events[1]);
        assert!(events[1].contains("\"skipped\":true"), "{}", events[1]);
        assert!(events[2].contains("\"reason\":\"leave\""), "{}", events[2]);
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

    /// `--cam-recovery`: a window reaches the device through the worker, and a CAM
    /// with no CAPMT in place is not judged, so an armed watchdog disarms.
    #[test]
    fn scrambled_window_without_a_capmt_disarms() {
        let (mut worker, _writes, _rx) = enigma_worker();
        ready_ci(&mut worker, 1, 0x0B00);
        let dev = worker.registry.device(1);
        dev.cam_recovery_timeout_ms = 10_000;
        dev.cam_recovery_since = 0;
        worker.handle(Request::ScrambledWindow {
            adapter: 1,
            seq: 1,
            scrambled: 50_000,
            clear: 0,
            window_ms: 10_000,
        });
        assert_eq!(
            worker.registry.get(1).unwrap().cam_recovery_since,
            -1,
            "no CAPMT: the window disarms"
        );
    }

    /// A worker on a manual clock, with `--cam-recovery` set to `timeout_ms`.
    fn cam_recovery_worker(timeout_ms: i64) -> (Worker, Arc<ManualClock>) {
        let clock = Arc::new(ManualClock::new(0, 0));
        let (tx, _rx) = mpsc::channel();
        let settings = DvbcaSettings {
            cam_recovery_ms: timeout_ms,
            ..DvbcaSettings::default()
        };
        let worker = Worker::new(
            settings,
            Box::new(ProcLog(Writes::default())),
            clock.clone(),
            tx,
        );
        (worker, clock)
    }

    /// `--cam-recovery`: a stuck window judged after the anchor re-sends the CAPMT as a
    /// forced CLM_UPDATE with a new version; the CAM is not reset.
    #[test]
    fn scrambled_window_with_a_capmt_resends_it() {
        use ms_ca_drivers::en50221::consts::TAG_CA_PMT;
        let (mut worker, clock, cam) = resend_worker();
        worker.add_pmt(zap_program(4, 0x64), false);
        let before = sent(&cam, TAG_CA_PMT);
        assert_eq!(before.len(), 1, "the join");
        arm_slot_0(&mut worker);
        clock.advance(20_000);
        worker.handle(Request::ScrambledWindow {
            adapter: 0,
            seq: 1,
            scrambled: 50_000,
            clear: 0,
            window_ms: 10_000,
        });
        let after = sent(&cam, TAG_CA_PMT);
        assert_eq!(after.len(), 2, "one forced re-send");
        assert_eq!(
            after[1][0],
            ms_ca::capmt::CLM_UPDATE,
            "list management update"
        );
        assert_ne!(after[1][3], before[0][3], "a new CAPMT version");
        assert_eq!(&after[1][4..], &before[0][4..], "the same programs");
        assert!(
            !worker.registry.get_mut(0).unwrap().take_close_request(),
            "the CAM is not reset"
        );
    }

    /// `zap_worker(true)` that also hands back the fake CAM.
    fn resend_worker() -> (Worker, Arc<ManualClock>, Arc<Mutex<FakeCam>>) {
        let cam = Arc::new(Mutex::new(FakeCam::new(
            CamConfig::default(),
            Arc::new(ms_fakecam::ManualClock::new(0)),
        )));
        let clock = Arc::new(ManualClock::new(0, 0));
        let (tx, _rx) = mpsc::channel();
        let settings = DvbcaSettings {
            cam_recovery_ms: 10_000,
            ..DvbcaSettings::default()
        };
        let mut worker = Worker::new(
            settings,
            Box::new(CamOpener(Arc::clone(&cam))),
            clock.clone(),
            tx,
        );
        worker.handle(Request::Init(AdapterInfo::dvb(0, 0, 0)));
        for _ in 0..500 {
            let up = worker.registry.get(0).is_some_and(|dev| {
                dev.state == CaState::Initialized && CaPmtSink::has_ca_session(dev)
            });
            if up {
                break;
            }
            clock.advance(10);
            worker.service();
        }
        (worker, clock, cam)
    }

    /// `--cam-recovery`: a CAM that is not initialised is not judged, even with a CAPMT.
    #[test]
    fn scrambled_window_on_an_uninitialised_cam_disarms() {
        let (mut worker, clock) = cam_recovery_worker(10_000);
        ready_ci(&mut worker, 1, 0x0B00);
        worker.capmt.get_mut(&1).unwrap().capmt[0].pmt_id = 5;
        let dev = worker.registry.device(1);
        dev.cam_recovery_since = 0;
        worker.registry.get_mut(1).unwrap().state = CaState::Active;
        clock.advance(20_000);
        worker.handle(Request::ScrambledWindow {
            adapter: 1,
            seq: 1,
            scrambled: 50_000,
            clear: 0,
            window_ms: 10_000,
        });
        assert_eq!(
            worker.registry.get(1).unwrap().cam_recovery_since,
            -1,
            "not initialised: the window disarms"
        );
        assert!(!worker.registry.get_mut(1).unwrap().take_cam_recovery());
    }

    /// A worker whose slot 0 holds one CAPMT with room for two programs.
    fn service_worker() -> (Worker, Arc<ManualClock>, Arc<Mutex<FakeCam>>) {
        let cam = Arc::new(Mutex::new(FakeCam::new(
            CamConfig::default(),
            Arc::new(ms_fakecam::ManualClock::new(0)),
        )));
        let clock = Arc::new(ManualClock::new(0, 0));
        let (tx, _rx) = mpsc::channel();
        let mut settings = DvbcaSettings {
            cam_recovery_ms: 10_000,
            ..DvbcaSettings::default()
        };
        settings.channels.insert(0, (1, 1, Vec::new()));
        let mut worker = Worker::new(
            settings,
            Box::new(CamOpener(Arc::clone(&cam))),
            clock.clone(),
            tx,
        );
        worker.handle(Request::Init(AdapterInfo::dvb(0, 0, 0)));
        for _ in 0..500 {
            let up = worker.registry.get(0).is_some_and(|dev| {
                dev.state == CaState::Initialized && CaPmtSink::has_ca_session(dev)
            });
            if up {
                break;
            }
            clock.advance(10);
            worker.service();
        }
        (worker, clock, cam)
    }

    /// `--cam-recovery`: one stuck service of a CAPMT is re-joined alone; a clean
    /// one is left alone, and further stuck windows send nothing more.
    #[test]
    fn service_windows_rejoin_only_the_stuck_service() {
        use ms_ca_drivers::en50221::consts::TAG_CA_PMT;
        #[cfg(feature = "diag-log")]
        let lines = super::diag_capture::lines();
        let (mut worker, clock, cam) = service_worker();
        worker.add_pmt(zap_program(4, 0x64), false);
        worker.add_pmt(zap_program(5, 0x65), false);
        {
            let capmt = worker.capmt.get_mut(&0).unwrap();
            assert_eq!(capmt.slot_of(4), Some(0), "both services share the CAPMT");
            assert_eq!(capmt.slot_of(5), Some(0));
            capmt.capmt_reason_log = Some(Vec::new());
        }
        let before = sent(&cam, TAG_CA_PMT).len();
        let window = |worker: &mut Worker, a: u64, b: u64| {
            worker.handle(Request::ServiceWindows {
                adapter: 0,
                seq: 1,
                window_ms: 10_000,
                services: vec![(0x64, None, a, 0), (0x65, None, 0, b)],
            });
        };
        window(&mut worker, 0, 0);
        clock.advance(12_000);
        window(&mut worker, 2_000, 2_000);
        clock.advance(15_000);
        window(&mut worker, 2_000, 2_000);
        assert_eq!(sent(&cam, TAG_CA_PMT).len(), before, "no fire yet");
        clock.advance(10_000);
        window(&mut worker, 2_000, 2_000);
        assert_eq!(
            sent(&cam, TAG_CA_PMT).len(),
            before + 1,
            "the stuck service is re-added alone"
        );
        assert_eq!(
            worker.capmt[&0].capmt_reason_log.as_deref(),
            Some(&["recovery"][..]),
            "the re-join carries the recovery reason"
        );
        let rejoined = sent(&cam, TAG_CA_PMT);
        let pmt =
            ms_fakecam::CaPmt::parse(rejoined.last().expect("the re-join")).expect("a CA_PMT body");
        assert_eq!(
            pmt.program_number, 0x64,
            "the stuck service's program number"
        );
        #[cfg(feature = "diag-log")]
        {
            let thread = std::thread::current().name().unwrap_or_default().to_owned();
            let captured = lines
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let fires: Vec<&String> = captured
                .iter()
                .filter(|line| {
                    line.contains(&thread) && line.contains("re-sending its CAPMT (CAM recovery)")
                })
                .collect();
            assert_eq!(fires.len(), 1, "{fires:?}");
            assert!(
                fires[0].contains("PMT 4 "),
                "the stuck service: {}",
                fires[0]
            );
            assert!(
                !fires[0].contains("PMT 5 "),
                "not the clean one: {}",
                fires[0]
            );
        }
        clock.advance(30_000);
        window(&mut worker, 2_000, 2_000);
        assert_eq!(
            sent(&cam, TAG_CA_PMT).len(),
            before + 1,
            "one re-send per join"
        );
    }

    /// A PMT update (version change) of a service that already fired keeps its
    /// identity: no second re-send, the same generation, one "already re-sent" line.
    #[test]
    fn service_windows_pmt_update_keeps_the_service_identity() {
        use ms_ca_drivers::en50221::consts::TAG_CA_PMT;
        #[cfg(feature = "diag-log")]
        let lines = super::diag_capture::lines();
        let (mut worker, clock, cam) = service_worker();
        worker.add_pmt(zap_program(4, 0x64), false);
        {
            let capmt = worker.capmt.get_mut(&0).unwrap();
            capmt.capmt_reason_log = Some(Vec::new());
        }
        let window = |worker: &mut Worker| {
            worker.handle(Request::ServiceWindows {
                adapter: 0,
                seq: 1,
                window_ms: 10_000,
                services: vec![(0x64, None, 2_000, 0)],
            });
        };
        window(&mut worker);
        clock.advance(12_000);
        window(&mut worker);
        clock.advance(15_000);
        window(&mut worker);
        clock.advance(10_000);
        window(&mut worker);
        let fired = sent(&cam, TAG_CA_PMT).len();
        assert_eq!(fired, 3, "the join and its one leave plus re-join");

        let generation = worker.programs.get(4).unwrap().generation;
        worker.add_pmt(zap_program(4, 0x64), true);
        assert_eq!(
            worker.programs.get(4).unwrap().generation,
            generation,
            "an update keeps the PMT generation"
        );
        let after_update = sent(&cam, TAG_CA_PMT).len();
        for _ in 0..5 {
            clock.advance(10_000);
            window(&mut worker);
        }
        assert_eq!(
            sent(&cam, TAG_CA_PMT).len(),
            after_update,
            "a PMT update does not allow a second re-send"
        );
        #[cfg(feature = "diag-log")]
        {
            let thread = std::thread::current().name().unwrap_or_default().to_owned();
            let captured = lines
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let warnings: Vec<&String> = captured
                .iter()
                .filter(|line| line.contains(&thread) && line.contains("was already re-sent once"))
                .collect();
            assert_eq!(warnings.len(), 1, "{warnings:?}");
            assert!(warnings[0].contains("PMT 4 "), "{}", warnings[0]);
        }
    }

    /// The ca.rs service path and the worker match the source PMT to the CAPMT
    /// program by SID, so the re-join uses the CAPMT's own pmt id.
    #[test]
    fn service_windows_rejoin_the_capmt_pmt_with_the_same_sid() {
        use crate::ca::{CaRuntime, ddci_services_for_test};
        use ms_ca_drivers::ddci::{DdciDevice, Mapping};
        use ms_ca_drivers::en50221::consts::TAG_CA_PMT;
        use ms_sched::{AdapterConfig, AdapterTuner, Scheduler, TuneError};
        use ms_tuner::{DeliverySystem, TuneParams};

        struct NoTune;
        impl AdapterTuner for NoTune {
            fn tune(&mut self, _params: &TuneParams) -> Result<(), TuneError> {
                Ok(())
            }
        }
        let hub = crate::ddci::DdciHub::new();
        let mut device = DdciDevice::new(0, 1);
        device.clear_slots();
        let source_pmt = 0;
        device.pmt[0].id = source_pmt;
        for (pid, ddci_pid) in [(601, 1301), (602, 1302)] {
            device.mapping.insert(
                (4, pid),
                Mapping {
                    ad: 4,
                    pid,
                    ddci_pid,
                    ddci: 0,
                    rewrite: false,
                    pmt: BTreeSet::from([source_pmt]),
                    filter_id: None,
                    pid_added: false,
                },
            );
        }
        hub.lock().ddci.devices.insert(0, device);

        let mut sched = Scheduler::new();
        let mut config = AdapterConfig::new(DeliverySystem::DvbS2);
        config.adapter_type = ms_sched::AdapterType::Dvb;
        sched.add_adapter(4, config, Box::new(NoTune));
        let id = sched.pmt_add(4, 0x1234, 0x100);
        assert_eq!(id, source_pmt);
        sched.pmt_add_stream_pid(id, 601, 2, false, true);
        sched.pmt_add_stream_pid(id, 602, 6, true, false);

        let services = ddci_services_for_test(&hub, &sched, 0);
        assert_eq!(services, [(0x1234, None, vec![1301, 1302])]);
        let mut base = BTreeMap::new();
        assert_eq!(
            CaRuntime::service_window_deltas(
                &mut base,
                0,
                &[(1301, 2_000, 0), (1302, 50, 0)],
                &services
            ),
            [(0x1234, None, 0, 0)],
            "the first report seeds"
        );
        let deltas = CaRuntime::service_window_deltas(
            &mut base,
            0,
            &[(1301, 4_000, 0), (1302, 100, 0)],
            &services,
        );
        assert_eq!(deltas, [(0x1234, None, 2_050, 0)]);

        let (mut worker, clock, cam) = resend_worker();
        worker.add_pmt(zap_program(3, 0x1234), false);
        worker.capmt.get_mut(&0).unwrap().capmt_reason_log = Some(Vec::new());
        let before = sent(&cam, TAG_CA_PMT).len();
        for seq in 1..=3 {
            clock.advance(30_000);
            worker.handle(Request::ServiceWindows {
                adapter: 0,
                seq,
                window_ms: 10_000,
                services: deltas.clone(),
            });
        }
        assert_eq!(
            sent(&cam, TAG_CA_PMT).len(),
            before + 2,
            "the CAPMT pmt 3 is left and re-joined"
        );
        assert_eq!(
            worker.capmt[&0].capmt_reason_log.as_deref(),
            Some(&["recovery", "recovery"][..]),
            "nothing was aimed at the source pmt"
        );
    }

    /// A worker whose slot 0 is an initialised CAM with a CA session; the zap
    /// guard is `zap_guard`.
    fn zap_worker(zap_guard: bool) -> (Worker, Arc<ManualClock>) {
        let cam = Arc::new(Mutex::new(FakeCam::new(
            CamConfig::default(),
            Arc::new(ms_fakecam::ManualClock::new(0)),
        )));
        let clock = Arc::new(ManualClock::new(0, 0));
        let (tx, _rx) = mpsc::channel();
        let settings = DvbcaSettings {
            cam_recovery_ms: 10_000,
            cam_recovery_zap_guard: zap_guard,
            ..DvbcaSettings::default()
        };
        let mut worker = Worker::new(settings, Box::new(CamOpener(cam)), clock.clone(), tx);
        worker.handle(Request::Init(AdapterInfo::dvb(0, 0, 0)));
        for _ in 0..500 {
            let up = worker.registry.get(0).is_some_and(|dev| {
                dev.state == CaState::Initialized && CaPmtSink::has_ca_session(dev)
            });
            if up {
                break;
            }
            clock.advance(10);
            worker.service();
        }
        (worker, clock)
    }

    fn zap_program(pmt_id: usize, sid: u16) -> Program {
        program_from_service(&CaService {
            pmt_id,
            adapter: 0,
            sid,
            pmt_pid: 0x100,
            ..CaService::default()
        })
    }

    /// Arms the watchdog as if slot 0 had been clean and an MMI had anchored it.
    fn arm_slot_0(worker: &mut Worker) {
        let dev = worker.registry.get_mut(0).unwrap();
        dev.cam_recovery_ok = true;
        dev.cam_recovery_since = 0;
    }

    /// `--cam-recovery` zap guard: a service joining the CAPMT clears the arming
    /// state, a service leaving it does too, and a re-add of the same service does not.
    #[test]
    fn zap_guard_clears_arming_on_a_join_and_a_leave() {
        let (mut worker, _clock) = zap_worker(true);
        arm_slot_0(&mut worker);
        worker.add_pmt(zap_program(4, 0x64), false);
        assert_eq!(worker.capmt[&0].slot_of(4), Some(0), "the service joined");
        let dev = worker.registry.get(0).unwrap();
        assert!(!dev.cam_recovery_ok, "a join clears the guard");
        assert_eq!(dev.cam_recovery_since, -1, "a join drops the anchor");

        arm_slot_0(&mut worker);
        worker.add_pmt(zap_program(4, 0x64), true);
        assert_eq!(worker.capmt[&0].slot_of(4), Some(0), "still joined");
        assert!(
            worker.registry.get(0).unwrap().cam_recovery_ok,
            "a re-add of the same service is no change"
        );

        worker.handle(Request::DelPmt {
            adapter: 0,
            pmt_id: 4,
        });
        assert!(
            !worker.registry.get(0).unwrap().cam_recovery_ok,
            "a leave clears the guard"
        );
    }

    /// With `--cam-recovery-no-zap-guard` a join leaves the arming state alone.
    #[test]
    fn zap_guard_off_keeps_arming_on_a_join() {
        let (mut worker, _clock) = zap_worker(false);
        arm_slot_0(&mut worker);
        worker.add_pmt(zap_program(4, 0x64), false);
        assert_eq!(worker.capmt[&0].slot_of(4), Some(0), "the service joined");
        assert!(worker.registry.get(0).unwrap().cam_recovery_ok);
    }

    /// A worker with slot 0 initialised by `Init`, its `Opened` event not yet forwarded.
    fn opened_worker() -> Worker {
        let cam = Arc::new(Mutex::new(FakeCam::new(
            CamConfig::default(),
            Arc::new(ms_fakecam::ManualClock::new(0)),
        )));
        let settings = DvbcaSettings {
            cam_recovery_ms: 10_000,
            ..DvbcaSettings::default()
        };
        let (tx, _rx) = mpsc::channel();
        let mut worker = Worker::new(
            settings,
            Box::new(CamOpener(cam)),
            Arc::new(ManualClock::new(0, 0)),
            tx,
        );
        worker.handle(Request::Init(AdapterInfo::dvb(0, 0, 0)));
        worker
    }

    /// The `Opened` event of a (re)opened CAM clears the zap guard's arming state.
    #[test]
    fn opened_event_clears_the_arming_state() {
        let mut worker = opened_worker();
        arm_slot_0(&mut worker);
        worker.forward_events(0);
        let dev = worker.registry.get(0).unwrap();
        assert!(!dev.cam_recovery_ok, "an opened CAM clears the guard");
        assert_eq!(dev.cam_recovery_since, -1, "an opened CAM drops the anchor");
    }

    /// The `ReleasePmts` event (a teardown) clears the zap guard's arming state.
    #[test]
    fn release_pmts_event_clears_the_arming_state() {
        let (mut worker, _clock) = zap_worker(true);
        arm_slot_0(&mut worker);
        worker.registry.get_mut(0).unwrap().teardown();
        worker.forward_events(0);
        assert!(
            !worker.registry.get(0).unwrap().cam_recovery_ok,
            "released PMTs clear the guard"
        );
    }

    /// `--cam-recovery-no-zap-guard` reaches the settings inverted, so the guard is on by default.
    #[test]
    fn settings_map_the_zap_guard_flag() {
        assert!(DvbcaSettings::from_options(&Options::default()).cam_recovery_zap_guard);
        let opts = Options {
            cam_recovery: 10,
            cam_recovery_no_zap_guard: true,
            ..Options::default()
        };
        let settings = DvbcaSettings::from_options(&opts);
        assert!(!settings.cam_recovery_zap_guard);
        assert_eq!(settings.cam_recovery_ms, 10_000);
    }

    #[test]
    fn report_scrambled_sends_the_window_to_the_worker() {
        let (requests, rx) = mpsc::channel();
        let (_tx, notices) = mpsc::channel();
        let dvbca = Dvbca {
            requests,
            notices,
            thread: None,
            serial: 0,
        };
        dvbca.report_scrambled(3, 7, 9, 1, 10_000);
        match rx.try_recv() {
            Ok(Request::ScrambledWindow {
                adapter,
                seq,
                scrambled,
                clear,
                window_ms,
            }) => assert_eq!(
                (adapter, seq, scrambled, clear, window_ms),
                (3, 7, 9, 1, 10_000)
            ),
            _ => panic!("no scrambled window request"),
        }
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

        // A free slot but no CA session: the failed send is retried and restores.
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

    // ca.cpp:3829: the first ca_timeout comes 1 s after the init, so a CAM that
    // answers T_CREATE_TC within that second never sees a second one.
    #[test]
    fn first_keepalive_waits_a_second_after_init() {
        struct Silent(Arc<Mutex<Vec<Vec<u8>>>>);
        impl CaLink for Silent {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::ErrorKind::WouldBlock.into())
            }
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                self.0.lock().unwrap().push(buf.to_vec());
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
        struct SilentOpener(Arc<Mutex<Vec<Vec<u8>>>>);
        impl CaOpener for SilentOpener {
            fn open(&mut self, _: &str) -> io::Result<Box<dyn CaLink>> {
                Ok(Box::new(Silent(Arc::clone(&self.0))))
            }
            fn has_ciplus_root_cert(&self) -> bool {
                false
            }
        }
        let writes = Arc::new(Mutex::new(Vec::new()));
        let clock = Arc::new(ManualClock::new(5_000, 0));
        let (tx, _rx) = mpsc::channel();
        let mut worker = Worker::new(
            DvbcaSettings::from_options(&Options::default()),
            Box::new(SilentOpener(Arc::clone(&writes))),
            clock.clone(),
            tx,
        );
        worker.handle(Request::Init(AdapterInfo::dvb(0, 0, 0)));
        let create_tc = || {
            writes
                .lock()
                .unwrap()
                .iter()
                .filter(|tpdu| tpdu.get(2) == Some(&0x82))
                .count()
        };
        worker.service();
        assert_eq!(create_tc(), 1, "no keepalive right after the init");
        clock.advance(999);
        worker.service();
        assert_eq!(create_tc(), 1, "still inside the first second");
        clock.advance(1);
        worker.service();
        assert_eq!(
            create_tc(),
            2,
            "the inactive link is polled with T_CREATE_TC after 1 s"
        );
        // socketworks.cpp:896: data read 500 ms later moves the next poll to 1 s after it.
        clock.advance(500);
        worker.registry.get_mut(0).expect("device").last_rx_ms = 6_500;
        clock.advance(999);
        worker.service();
        assert_eq!(create_tc(), 2, "a read re-arms the idle timer");
        clock.advance(1);
        worker.service();
        assert_eq!(create_tc(), 3, "1 s after the last read");
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

    #[test]
    fn ddci_pmt_forwards_es_descriptors_and_remaps_ecm() {
        let ca_desc = ms_ca::RawDescriptor {
            tag: 0x09,
            data: vec![0x0B, 0x00, 0x05, 0x73],
        };
        let lang_desc = ms_ca::RawDescriptor {
            tag: 0x0A,
            data: b"eng\0".to_vec(),
        };
        let svc = CaService {
            pmt_id: 0,
            adapter: 0,
            sid: 0x66,
            pmt_pid: 0x3FF,
            streams: vec![StreamPid {
                stream_type: 2,
                pid: 0x3FF,
            }],
            stream_descriptors: vec![vec![ca_desc.clone(), lang_desc.clone()]],
            ..CaService::default()
        };
        let program = program_from_service(&svc);
        assert_eq!(
            program.streams[0].descriptors,
            vec![ca_desc, lang_desc],
            "stream descriptors must reach the CAPMT program"
        );

        use ms_ca_drivers::ddci::{Ddci, DdciDevice, DdciPmt};
        let mut ddci = Ddci::new();
        ddci.devices.insert(0, DdciDevice::new(0, 0));
        // Occupy 0x0573 on adapter 1 so the ES-level ECM pid must be remapped.
        ddci.add_pid_mapping_table(1, 0x0573, 99, 0, false);
        let mapped = ddci.add_pid_mapping_table(0, 0x0573, program.id, 0, false);
        assert!(mapped >= 0 && mapped != 0x0573, "ECM pid must be remapped");
        let mut dp = DdciPmt {
            ver: 0,
            pcr_pid: 8191,
            ..DdciPmt::default()
        };
        let psi = ddci.create_pmt(0, &program, 512, &mut dp);
        let parsed =
            ms_psi::Pmt::parse(&psi[1..], ms_psi::CrcCheck::Verify).expect("generated PMT parses");
        let descriptors = &parsed.streams[0].descriptors;
        assert!(
            descriptors
                .iter()
                .any(|d| d.tag == 0x0A && d.data == b"eng\0"),
            "the language descriptor must be forwarded"
        );
        let ca = descriptors
            .iter()
            .find(|d| d.is_ca())
            .expect("ES-level CA descriptor");
        assert_eq!(ca.ca_pid(), Some(mapped as u16));
    }

    struct CamStuckRig {
        worker: Worker,
        clock: Arc<ManualClock>,
        cam: Arc<Mutex<FakeCam>>,
        hub: crate::ddci::DdciHub,
        sched: ms_sched::Scheduler,
        ca: crate::ca::CaRuntime,
        recorder: WindowRecorder,
        cam_cum: (u64, u64),
        cum: BTreeMap<u16, (u64, u64)>,
        expected_seq: u64,
    }

    impl CamStuckRig {
        fn new(max_programs: usize) -> Self {
            use ms_ca_drivers::ddci::DdciDevice;

            struct NoTune;
            impl ms_sched::AdapterTuner for NoTune {
                fn tune(&mut self, _: &ms_tuner::TuneParams) -> Result<(), ms_sched::TuneError> {
                    Ok(())
                }
            }

            let cam = Arc::new(Mutex::new(FakeCam::new(
                CamConfig::default(),
                Arc::new(ms_fakecam::ManualClock::new(0)),
            )));
            let clock = Arc::new(ManualClock::new(0, 0));
            let (tx, _rx) = mpsc::channel();
            let mut settings = DvbcaSettings {
                cam_recovery_ms: 10_000,
                ..DvbcaSettings::default()
            };
            settings.channels.insert(0, (max_programs, 1, Vec::new()));
            let mut worker = Worker::new(
                settings,
                Box::new(CamOpener(Arc::clone(&cam))),
                clock.clone(),
                tx,
            );
            worker.handle(Request::Init(AdapterInfo::dvb(0, 0, 0)));
            for _ in 0..500 {
                let up = worker.registry.get(0).is_some_and(|dev| {
                    dev.state == CaState::Initialized && CaPmtSink::has_ca_session(dev)
                });
                if up {
                    break;
                }
                clock.advance(10);
                worker.service();
            }

            let hub = crate::ddci::DdciHub::new();
            let mut ddci_dev = DdciDevice::new(0, max_programs);
            ddci_dev.clear_slots();
            hub.lock().ddci.devices.insert(0, ddci_dev);

            let mut sched = ms_sched::Scheduler::new();
            let mut ci_cfg = ms_sched::AdapterConfig::new(ms_tuner::DeliverySystem::DvbS2);
            ci_cfg.adapter_type = ms_sched::AdapterType::Ci;
            sched.add_adapter(0, ci_cfg, Box::new(NoTune));

            let opts = ms_config::Options {
                cam_recovery: 10,
                ..ms_config::Options::default()
            };
            let mut ca = crate::ca::CaRuntime::new(&opts);
            ca.attach_ddci(&mut sched, hub.clone());
            let (dvbca, recorder) = Dvbca::recording();
            ca.attach_dvbca(&mut sched, dvbca);
            ca.set_ci_window_totals_for_test(0, 0, 0, Vec::new());

            let mut rig = Self {
                worker,
                clock,
                cam,
                hub,
                sched,
                ca,
                recorder,
                cam_cum: (0, 0),
                cum: BTreeMap::new(),
                expected_seq: 0,
            };
            // Advance past the 5-minute readiness floor unless a test checks under-5-min behavior.
            rig.advance_idle(360_000);
            rig
        }

        /// Fast-forwards idle time while keeping the CI window baseline anchored at `now`.
        fn advance_idle(&mut self, ms: i64) {
            self.clock.advance(ms);
            self.ca.reanchor_ci_window_for_test(0, self.clock.tick_ms());
        }

        fn add_host_adapter(
            &mut self,
            aid: usize,
            params: Option<ms_tuner::TuneParams>,
            tsid: u16,
        ) {
            struct NoTune;
            impl ms_sched::AdapterTuner for NoTune {
                fn tune(&mut self, _: &ms_tuner::TuneParams) -> Result<(), ms_sched::TuneError> {
                    Ok(())
                }
            }
            let mut cfg = ms_sched::AdapterConfig::new(ms_tuner::DeliverySystem::DvbS2);
            cfg.adapter_type = ms_sched::AdapterType::Dvb;
            self.sched.add_adapter(aid, cfg, Box::new(NoTune));
            if let Some(tp) = params {
                self.sched.adapter_mut(aid).unwrap().params = tp;
            }
            if tsid != 0 {
                let fid = self
                    .sched
                    .add_filter(aid as i32, 0, ms_sched::pmt::FilterTarget::Pat, 0);
                let mut pat = vec![
                    0x00,
                    0xB0,
                    0x09,
                    (tsid >> 8) as u8,
                    tsid as u8,
                    0xC1,
                    0x00,
                    0x00,
                ];
                let crc = ms_ts::crc32_mpeg2(&pat);
                pat.extend_from_slice(&crc.to_be_bytes());
                self.sched.process_pat(fid, &pat);
                assert_eq!(self.sched.adapter_transponder_id(aid), tsid);
            }
        }

        /// Maps a host PMT into DDCI device 0 and adds the corresponding CI CAPMT program.
        fn add_service(
            &mut self,
            slot: usize,
            host_aid: usize,
            capmt_pmt_id: i32,
            sid: u16,
            es_pid: u16,
            ddci_pid: u16,
        ) {
            use ms_ca_drivers::ddci::Mapping;

            let src_pmt = self
                .sched
                .pmt_add(host_aid as i32, i32::from(sid), 0x100 + slot as i32);
            self.sched
                .pmt_add_stream_pid(src_pmt, i32::from(es_pid), 2, false, true);
            {
                let mut state = self.hub.lock();
                let dev = state.ddci.devices.get_mut(&0).unwrap();
                dev.pmt[slot].id = src_pmt;
                dev.mapping.insert(
                    (host_aid as u16, es_pid),
                    Mapping {
                        ad: host_aid as u16,
                        pid: es_pid,
                        ddci_pid: i32::from(ddci_pid),
                        ddci: 0,
                        rewrite: false,
                        pmt: BTreeSet::from([src_pmt]),
                        filter_id: None,
                        pid_added: false,
                    },
                );
            }
            self.worker
                .add_pmt(zap_program(capmt_pmt_id as usize, sid), false);
            let cur = *self.cum.entry(ddci_pid).or_insert((0, 0));
            self.ca.seed_service_pid_for_test(0, ddci_pid, cur);
        }

        /// Runs `CaRuntime::decrypt_windows` at the current clock tick and forwards the recorded pair.
        fn step_window_at_current_time(&mut self, pid_deltas: &[(u16, u64, u64)]) {
            for &(ddci_pid, scr, clr) in pid_deltas {
                self.cam_cum.0 += scr;
                self.cam_cum.1 += clr;
                let entry = self.cum.entry(ddci_pid).or_insert((0, 0));
                entry.0 += scr;
                entry.1 += clr;
            }
            let totals: Vec<(u16, u64, u64)> =
                self.cum.iter().map(|(&p, &(s, c))| (p, s, c)).collect();
            self.ca
                .set_ci_window_totals_for_test(0, self.cam_cum.0, self.cam_cum.1, totals);
            self.ca
                .run_decrypt_windows_for_test(&self.sched, self.clock.tick_ms());
            let scr_reqs = self.recorder.windows_with_seq();
            let svc_reqs = self.recorder.service_windows_with_seq();
            assert_eq!(scr_reqs.len(), 1);
            assert_eq!(svc_reqs.len(), 1);
            self.expected_seq += 1;
            let (ad_a, seq_a, scrambled, clear, window_ms) = scr_reqs[0];
            let (ad_b, seq_b, svc_window_ms, services) =
                svc_reqs.into_iter().next().expect("service window");
            assert_eq!(seq_a, self.expected_seq, "decrypt_windows seq increments");
            assert_eq!(
                seq_a, seq_b,
                "decrypt_windows shares seq across both requests"
            );
            assert_eq!(
                (ad_a, ad_b, window_ms, svc_window_ms),
                (0, 0, 10_000, 10_000)
            );
            self.worker.handle(Request::ScrambledWindow {
                adapter: ad_a,
                seq: seq_a,
                scrambled,
                clear,
                window_ms,
            });
            self.worker.handle(Request::ServiceWindows {
                adapter: ad_b,
                seq: seq_b,
                window_ms: svc_window_ms,
                services,
            });
        }

        /// Advances 10 s and feeds one window through `CaRuntime::decrypt_windows` -> `Worker`.
        fn step_window(&mut self, pid_deltas: &[(u16, u64, u64)]) {
            self.clock.advance(10_000);
            self.step_window_at_current_time(pid_deltas);
        }
    }

    fn sat_tp(src: i32, freq: i32, pol: ms_tuner::Polarization) -> ms_tuner::TuneParams {
        ms_tuner::TuneParams {
            src,
            src_specified: true,
            freq,
            pol,
            pol_specified: true,
            msys: ms_tuner::DeliverySystem::DvbS2,
            sr: 27_500_000,
            ..ms_tuner::TuneParams::default()
        }
    }

    // 1) 11-07 sequence alerts once in the window ending +100 s (:30:41), never resets, and holds 30 min.
    #[test]
    fn cam_stuck_11_07_sequence_alerts_once_and_holds_for_30_minutes() {
        #[cfg(feature = "diag-log")]
        let lines = super::diag_capture::lines();
        let mut rig = CamStuckRig::new(2);
        rig.add_host_adapter(
            4,
            Some(sat_tp(1, 11_000_000, ms_tuner::Polarization::Horizontal)),
            101,
        );
        rig.add_host_adapter(
            5,
            Some(sat_tp(1, 11_500_000, ms_tuner::Polarization::Vertical)),
            202,
        );
        rig.add_service(0, 4, 10, 0x0101, 601, 1301);
        rig.add_service(1, 5, 11, 0x0202, 701, 1401);

        // Anchor clock so :29:11 is +10 s from t0: set base at t0 (`ready_since` > 5 min earlier).
        let t_29_01 = rig.clock.tick_ms();
        // :29:11 (seq 1) and :29:21 (seq 2): both services clean (`ever_clean` set on seq 2 once `start >= joined`).
        rig.step_window(&[(1301, 0, 20_000), (1401, 0, 20_000)]);
        rig.step_window(&[(1301, 0, 20_000), (1401, 0, 20_000)]);
        assert_eq!(rig.clock.tick_ms() - t_29_01, 20_000);

        // Unsolicited MMI arrives right after :29:21 (arms MMI recovery).
        rig.clock.advance(4_000);
        rig.worker
            .registry
            .get_mut(0)
            .unwrap()
            .note_cam_recovery_mmi();
        rig.clock.advance(6_000);
        // :29:31 (seq 3): service A regresses (stuck), service B still clean through :30.
        rig.step_window_at_current_time(&[(1301, 25_000, 0), (1401, 0, 20_000)]);

        // :29:41 (seq 4): A stuck, B neutral (absent :31-:35, <1000 pkts); CAM-wide MMI re-send fires at :29:41.
        rig.step_window(&[(1301, 25_000, 0), (1401, 500, 0)]);
        assert_eq!(
            rig.worker.registry.get(0).unwrap().cam_recovery_last,
            t_29_01 + 40_000
        );
        assert_eq!(
            rig.worker.registry.get(0).unwrap().cam_stuck_last_alert_ms,
            -1
        );

        // :29:51 (seq 5): A silent re-join fires; B first full stuck window.
        rig.step_window(&[(1301, 25_000, 0), (1401, 15_000, 0)]);
        assert_eq!(
            rig.worker.registry.get(0).unwrap().cam_stuck_last_alert_ms,
            -1
        );

        // :30:01 (seq 6), :30:11 (seq 7: B silent re-join fires), :30:21 (seq 8), :30:31 (seq 9): no alert yet.
        for expected_offset in [60_000, 70_000, 80_000, 90_000] {
            rig.step_window(&[(1301, 25_000, 0), (1401, 15_000, 0)]);
            assert_eq!(rig.clock.tick_ms() - t_29_01, expected_offset);
            assert_eq!(
                rig.worker.registry.get(0).unwrap().cam_stuck_last_alert_ms,
                -1,
                "must not alert before :30:41 (offset {expected_offset})"
            );
        }

        // :30:41 (seq 10, 3 full windows after B's :30:11 re-join): alerts once, never requests close.
        rig.step_window(&[(1301, 25_000, 0), (1401, 15_000, 0)]);
        let t_30_41 = t_29_01 + 100_000;
        assert_eq!(rig.clock.tick_ms(), t_30_41);
        assert_eq!(
            rig.worker.registry.get(0).unwrap().cam_stuck_last_alert_ms,
            t_30_41
        );
        assert!(!rig.worker.registry.get_mut(0).unwrap().take_close_request());

        // :30:51 (seq 11): same episode does not re-alert.
        rig.step_window(&[(1301, 25_000, 0), (1401, 15_000, 0)]);
        assert_eq!(
            rig.worker.registry.get(0).unwrap().cam_stuck_last_alert_ms,
            t_30_41
        );

        // Teardown ends the episode, but the 30-minute hold survives teardown.
        rig.worker.registry.get_mut(0).unwrap().teardown();
        rig.worker.forward_events(0);
        rig.worker.handle(Request::Init(AdapterInfo::dvb(0, 0, 0)));
        for _ in 0..500 {
            let up = rig.worker.registry.get(0).is_some_and(|dev| {
                dev.state == CaState::Initialized && CaPmtSink::has_ca_session(dev)
            });
            if up {
                break;
            }
            rig.clock.advance(10);
            rig.worker.service();
        }
        rig.worker.add_pmt(zap_program(10, 0x0101), false);
        rig.worker.add_pmt(zap_program(11, 0x0202), false);
        rig.advance_idle(360_000);
        rig.step_window(&[(1301, 0, 20_000), (1401, 0, 20_000)]);
        rig.step_window(&[(1301, 0, 20_000), (1401, 0, 20_000)]);
        for _ in 0..6 {
            rig.step_window(&[(1301, 25_000, 0), (1401, 15_000, 0)]);
        }
        assert!(
            rig.worker
                .registry
                .get(0)
                .unwrap()
                .cam_stuck_episode_alerted
        );
        assert_eq!(
            rig.worker.registry.get(0).unwrap().cam_stuck_last_alert_ms,
            t_30_41,
            "second episode within 30 minutes is held across teardown"
        );

        // End held episode via CAM-wide clean window, advance past 30 min since t_30_41, and trigger again.
        rig.step_window(&[(1301, 0, 20_000), (1401, 0, 20_000)]);
        rig.advance_idle(1_800_000);
        for _ in 0..4 {
            rig.step_window(&[(1301, 25_000, 0), (1401, 15_000, 0)]);
        }
        assert!(
            rig.worker.registry.get(0).unwrap().cam_stuck_last_alert_ms >= t_30_41 + 1_800_000,
            "alert fires again once 30-minute hold has elapsed"
        );

        #[cfg(feature = "diag-log")]
        {
            let cam_states: Vec<String> = super::diag_capture::events("cam_state")
                .into_iter()
                .filter(|e| e.contains(r#""state":"cam_stuck""#))
                .collect();
            assert_eq!(
                cam_states.len(),
                2,
                "one initial + one post-30-min cam_stuck: {cam_states:?}"
            );
            let thread = std::thread::current().name().unwrap_or_default().to_owned();
            let captured = lines
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let alerts: Vec<&String> = captured
                .iter()
                .filter(|l| l.contains(&thread) && l.contains("manual CAM reset may be needed"))
                .collect();
            assert_eq!(alerts.len(), 2, "{alerts:?}");
            assert!(alerts[0].contains("PMT 10 sid 0x0101"), "{}", alerts[0]);
            assert!(alerts[0].contains("PMT 11 sid 0x0202"), "{}", alerts[0]);
            assert!(alerts[0].contains("freq=11000000"), "{}", alerts[0]);
            assert!(
                alerts[0].contains("regressed=80s_ago tier1=rejoin(50s_ago,seq=5)"),
                "{}",
                alerts[0]
            );
            assert!(
                alerts[0].contains("regressed=60s_ago tier1=rejoin(30s_ago,seq=7)"),
                "{}",
                alerts[0]
            );
            let held: Vec<&String> = captured
                .iter()
                .filter(|l| l.contains(&thread) && l.contains("cam_stuck held"))
                .collect();
            assert_eq!(held.len(), 1, "{held:?}");
        }
    }

    // 2) Pre-join clear window (`start < joined`), never-clear, single service, HD+SD (10:1), low-rate clear (100-999), and recovered third service never alert.
    #[test]
    fn cam_stuck_never_alerts_for_never_clear_single_or_alongside_clean_service() {
        // 2a: First window starting before `joined` (`start < run.joined`) does not mark `ever_clean`, so 8 stuck windows after it never alert.
        let mut rig = CamStuckRig::new(3);
        rig.add_host_adapter(
            4,
            Some(sat_tp(1, 11_000_000, ms_tuner::Polarization::Horizontal)),
            101,
        );
        rig.add_host_adapter(
            5,
            Some(sat_tp(1, 11_500_000, ms_tuner::Polarization::Vertical)),
            202,
        );
        rig.add_host_adapter(
            6,
            Some(sat_tp(1, 12_000_000, ms_tuner::Polarization::Horizontal)),
            303,
        );
        rig.add_service(0, 4, 10, 0x0101, 601, 1301);
        rig.add_service(1, 5, 11, 0x0202, 701, 1401);
        rig.step_window(&[(1301, 0, 20_000), (1401, 0, 20_000)]);
        for _ in 0..8 {
            rig.step_window(&[(1301, 25_000, 0), (1401, 25_000, 0)]);
        }
        assert_eq!(
            rig.worker.registry.get(0).unwrap().cam_stuck_last_alert_ms,
            -1,
            "window starting before joined must not set ever_clean"
        );

        // 2b: One regressed service alone (other never clear in this join) never alerts.
        rig.step_window(&[(1301, 0, 20_000), (1401, 25_000, 0)]);
        for _ in 0..8 {
            rig.step_window(&[(1301, 25_000, 0), (1401, 25_000, 0)]);
        }
        assert_eq!(
            rig.worker.registry.get(0).unwrap().cam_stuck_last_alert_ms,
            -1
        );

        // 2c: Stuck HD services (50_000 scr each) next to a clear SD service (5_000 clr, ~10:1) or low-rate clear (150 clr) never alert.
        rig.add_service(2, 6, 12, 0x0303, 801, 1501);
        rig.step_window(&[(1301, 0, 50_000), (1401, 0, 50_000), (1501, 0, 5_000)]);
        rig.step_window(&[(1301, 0, 50_000), (1401, 0, 50_000), (1501, 0, 5_000)]);
        for _ in 0..7 {
            rig.step_window(&[(1301, 50_000, 0), (1401, 50_000, 0), (1501, 0, 5_000)]);
        }
        assert_eq!(
            rig.worker.registry.get(0).unwrap().cam_stuck_last_alert_ms,
            -1
        );
        rig.step_window(&[(1301, 50_000, 0), (1401, 50_000, 0), (1501, 0, 150)]);
        assert_eq!(
            rig.worker.registry.get(0).unwrap().cam_stuck_last_alert_ms,
            -1
        );

        // 2d: Third service regresses first, then is clear again (150 clear pkts) while the other two stay stuck: vetoes alert.
        for _ in 0..4 {
            rig.step_window(&[(1301, 0, 50_000), (1401, 0, 50_000), (1501, 5_000, 0)]);
        }
        for _ in 0..6 {
            rig.step_window(&[(1301, 50_000, 0), (1401, 50_000, 0), (1501, 0, 150)]);
        }
        assert_eq!(
            rig.worker.registry.get(0).unwrap().cam_stuck_last_alert_ms,
            -1
        );
    }

    // 3) Same transponder (one adapter, two adapters equal params, or slight freq offset + same PAT tsid) and missing key never alert.
    #[test]
    fn cam_stuck_same_transponder_and_missing_key_never_alert() {
        // 3a: One adapter carrying two services.
        let mut rig = CamStuckRig::new(2);
        rig.add_host_adapter(
            4,
            Some(sat_tp(1, 12_000_000, ms_tuner::Polarization::Horizontal)),
            0,
        );
        rig.add_service(0, 4, 10, 0x0101, 601, 1301);
        rig.add_service(1, 4, 11, 0x0202, 602, 1302);
        rig.step_window(&[(1301, 0, 20_000), (1302, 0, 20_000)]);
        rig.step_window(&[(1301, 0, 20_000), (1302, 0, 20_000)]);
        for _ in 0..8 {
            rig.step_window(&[(1301, 25_000, 0), (1302, 25_000, 0)]);
        }
        assert_eq!(
            rig.worker.registry.get(0).unwrap().cam_stuck_last_alert_ms,
            -1
        );

        // 3b: Two adapters with identical tuned params and unknown tsid (0).
        let mut rig = CamStuckRig::new(2);
        rig.add_host_adapter(
            4,
            Some(sat_tp(1, 12_000_000, ms_tuner::Polarization::Horizontal)),
            0,
        );
        rig.add_host_adapter(
            5,
            Some(sat_tp(1, 12_000_000, ms_tuner::Polarization::Horizontal)),
            0,
        );
        rig.add_service(0, 4, 10, 0x0101, 601, 1301);
        rig.add_service(1, 5, 11, 0x0202, 701, 1401);
        rig.step_window(&[(1301, 0, 20_000), (1401, 0, 20_000)]);
        rig.step_window(&[(1301, 0, 20_000), (1401, 0, 20_000)]);
        for _ in 0..8 {
            rig.step_window(&[(1301, 25_000, 0), (1401, 25_000, 0)]);
        }
        assert_eq!(
            rig.worker.registry.get(0).unwrap().cam_stuck_last_alert_ms,
            -1
        );

        // 3c: Two adapters at slightly different frequencies (12000 vs 12001 MHz) with same PAT tsid.
        let mut rig = CamStuckRig::new(2);
        rig.add_host_adapter(
            4,
            Some(sat_tp(1, 12_000_000, ms_tuner::Polarization::Horizontal)),
            0x0401,
        );
        rig.add_host_adapter(
            5,
            Some(sat_tp(1, 12_001_000, ms_tuner::Polarization::Horizontal)),
            0x0401,
        );
        rig.add_service(0, 4, 10, 0x0101, 601, 1301);
        rig.add_service(1, 5, 11, 0x0202, 701, 1401);
        rig.step_window(&[(1301, 0, 20_000), (1401, 0, 20_000)]);
        rig.step_window(&[(1301, 0, 20_000), (1401, 0, 20_000)]);
        for _ in 0..8 {
            rig.step_window(&[(1301, 25_000, 0), (1401, 25_000, 0)]);
        }
        assert_eq!(
            rig.worker.registry.get(0).unwrap().cam_stuck_last_alert_ms,
            -1
        );

        // 3d: Default/unknown TuneParams -> no transponder key -> never counts.
        let mut rig = CamStuckRig::new(2);
        rig.add_host_adapter(
            4,
            Some(sat_tp(1, 11_000_000, ms_tuner::Polarization::Horizontal)),
            101,
        );
        rig.add_host_adapter(5, None, 202);
        rig.add_service(0, 4, 10, 0x0101, 601, 1301);
        rig.add_service(1, 5, 11, 0x0202, 701, 1401);
        rig.step_window(&[(1301, 0, 20_000), (1401, 0, 20_000)]);
        rig.step_window(&[(1301, 0, 20_000), (1401, 0, 20_000)]);
        for _ in 0..8 {
            rig.step_window(&[(1301, 25_000, 0), (1401, 25_000, 0)]);
        }
        assert_eq!(
            rig.worker.registry.get(0).unwrap().cam_stuck_last_alert_ms,
            -1
        );
    }

    // 4) Clean window restarts the 3-window count; neutral windows advance it, and the latest window must be stuck.
    #[test]
    fn cam_stuck_neutral_counts_toward_three_clean_restarts_and_latest_must_be_stuck() {
        let mut rig = CamStuckRig::new(2);
        rig.add_host_adapter(
            4,
            Some(sat_tp(1, 11_000_000, ms_tuner::Polarization::Horizontal)),
            101,
        );
        rig.add_host_adapter(
            5,
            Some(sat_tp(1, 11_500_000, ms_tuner::Polarization::Vertical)),
            202,
        );
        rig.add_service(0, 4, 10, 0x0101, 601, 1301);
        rig.add_service(1, 5, 11, 0x0202, 701, 1401);

        rig.step_window(&[(1301, 0, 20_000), (1401, 0, 20_000)]);
        rig.step_window(&[(1301, 0, 20_000), (1401, 0, 20_000)]);
        // 3 stuck windows after join lag trigger silent re-join on both services (used = true).
        for _ in 0..3 {
            rig.step_window(&[(1301, 25_000, 0), (1401, 25_000, 0)]);
        }
        // 2 stuck windows after Tier 1, then 1 clean window restarts the regression and 3-window count.
        for _ in 0..2 {
            rig.step_window(&[(1301, 25_000, 0), (1401, 25_000, 0)]);
        }
        rig.step_window(&[(1301, 0, 1_500), (1401, 0, 1_500)]);

        // 3 stuck windows (re-regress + 2 post windows): no alert yet (without the reset, count would be 5).
        for _ in 0..3 {
            rig.step_window(&[(1301, 25_000, 0), (1401, 25_000, 0)]);
        }
        assert_eq!(
            rig.worker.registry.get(0).unwrap().cam_stuck_last_alert_ms,
            -1
        );

        // 3rd post window is neutral: count reaches 3, but latest is not stuck -> no alert.
        rig.step_window(&[(1301, 0, 0), (1401, 0, 0)]);
        assert_eq!(
            rig.worker.registry.get(0).unwrap().cam_stuck_last_alert_ms,
            -1
        );

        // 4th post window is stuck: neutral window did not restart the count, so it alerts now.
        rig.step_window(&[(1301, 25_000, 0), (1401, 25_000, 0)]);
        assert_eq!(
            rig.worker.registry.get(0).unwrap().cam_stuck_last_alert_ms,
            rig.clock.tick_ms()
        );
    }

    // 5) Check runs on a batch with no re-join due; no alert while inactive or within 5 min of ready (repeat CA_INFO keeps ready_since).
    #[test]
    fn cam_stuck_waits_five_minutes_from_initial_ready_and_skips_inactive_cam() {
        use ms_ca_drivers::en50221::consts::{EN50221_APP_CA_RESOURCEID, TAG_CA_INFO};

        let mut rig = CamStuckRig::new(2);
        // Re-anchor `ready_since` right at `now` via a fresh teardown + init.
        rig.worker.registry.get_mut(0).unwrap().teardown();
        rig.worker.forward_events(0);
        rig.worker.handle(Request::Init(AdapterInfo::dvb(0, 0, 0)));
        for _ in 0..500 {
            let up = rig.worker.registry.get(0).is_some_and(|dev| {
                dev.state == CaState::Initialized && CaPmtSink::has_ca_session(dev)
            });
            if up {
                break;
            }
            rig.clock.advance(10);
            rig.worker.service();
        }
        let ready_at = rig.worker.registry.get(0).unwrap().ready_since;
        assert!(ready_at >= 0);

        rig.add_host_adapter(
            4,
            Some(sat_tp(1, 11_000_000, ms_tuner::Polarization::Horizontal)),
            101,
        );
        rig.add_host_adapter(
            5,
            Some(sat_tp(1, 11_500_000, ms_tuner::Polarization::Vertical)),
            202,
        );
        rig.add_service(0, 4, 10, 0x0101, 601, 1301);
        rig.add_service(1, 5, 11, 0x0202, 701, 1401);

        // Advance to 210 s after ready; 2 clean (230 s) + 6 stuck reach 3 post-rejoin windows at 290 s (< 300 s): no alert yet.
        rig.advance_idle(210_000);
        rig.step_window(&[(1301, 0, 20_000), (1401, 0, 20_000)]);
        rig.step_window(&[(1301, 0, 20_000), (1401, 0, 20_000)]);
        for _ in 0..6 {
            rig.step_window(&[(1301, 25_000, 0), (1401, 25_000, 0)]);
        }
        assert_eq!(rig.clock.tick_ms() - ready_at, 290_000);
        assert_eq!(
            rig.worker.registry.get(0).unwrap().cam_stuck_last_alert_ms,
            -1
        );

        // At 300 s (>= 5 min ready), mark CAM `CaState::Active` (inactive CA session): neither a direct
        // check before `cam_recovery_sync_services(..., false)` nor an inactive `ServiceWindows` batch alerts.
        rig.advance_idle(10_000);
        assert_eq!(rig.clock.tick_ms() - ready_at, 300_000);
        rig.worker.registry.get_mut(0).unwrap().state = CaState::Active;
        rig.worker
            .registry
            .get_mut(0)
            .unwrap()
            .cam_recovery_check_stuck();
        assert_eq!(
            rig.worker.registry.get(0).unwrap().cam_stuck_last_alert_ms,
            -1
        );
        rig.step_window(&[(1301, 25_000, 0), (1401, 25_000, 0)]);
        assert_eq!(
            rig.worker.registry.get(0).unwrap().cam_stuck_last_alert_ms,
            -1
        );

        // Restore `Initialized` and send repeated CA_INFO at 312 s: `ready_since` stays `ready_at`, and one
        // stuck window at 322 s still does not alert because the inactive batch cleared the regression.
        rig.worker.registry.get_mut(0).unwrap().state = CaState::Initialized;
        assert!(rig.cam.lock().unwrap().send_to_resource(
            EN50221_APP_CA_RESOURCEID,
            TAG_CA_INFO,
            &[0x7E, 0x01, 0x7E, 0x02],
        ));
        rig.advance_idle(2_000);
        for _ in 0..5 {
            rig.worker.service();
        }
        assert_eq!(
            rig.worker.registry.get(0).unwrap().caids(),
            &[0x7E01u16, 0x7E02][..]
        );
        assert_eq!(
            rig.worker.registry.get(0).unwrap().ready_since,
            ready_at,
            "repeated CA_INFO must not move ready_since"
        );
        rig.step_window(&[(1301, 25_000, 0), (1401, 25_000, 0)]);
        assert_eq!(
            rig.worker.registry.get(0).unwrap().cam_stuck_last_alert_ms,
            -1,
            "inactive batch disarmed the prior regression"
        );

        // 2 clean + 6 stuck alert at 402 s (< 5 min after the 312 s repeat CA_INFO).
        rig.step_window(&[(1301, 0, 20_000), (1401, 0, 20_000)]);
        rig.step_window(&[(1301, 0, 20_000), (1401, 0, 20_000)]);
        for _ in 0..6 {
            rig.step_window(&[(1301, 25_000, 0), (1401, 25_000, 0)]);
        }
        let alert_at = rig.clock.tick_ms();
        assert_eq!(
            rig.worker.registry.get(0).unwrap().cam_stuck_last_alert_ms,
            alert_at
        );

        // Empty CAPMT makes `ServiceWindows` inactive even while `Initialized`, so `cam_recovery_check_stuck` is skipped.
        rig.worker.handle(Request::DelPmt {
            adapter: 0,
            pmt_id: 10,
        });
        rig.worker.handle(Request::DelPmt {
            adapter: 0,
            pmt_id: 11,
        });
        rig.advance_idle(900_000);
        rig.worker.handle(Request::ServiceWindows {
            adapter: 0,
            seq: 99,
            window_ms: 10_000,
            services: Vec::new(),
        });
        assert_eq!(
            rig.worker.registry.get(0).unwrap().cam_stuck_episode_since,
            alert_at,
            "inactive ServiceWindows must not run cam_recovery_check_stuck"
        );
    }

    // 6) 9-13% scrambled with < 1000 clear packets (11-03 16:32 case) is not clean.
    #[test]
    fn cam_stuck_9_to_13_percent_scrambled_under_1000_clear_is_not_clean() {
        let mut rig = CamStuckRig::new(2);
        rig.add_host_adapter(
            4,
            Some(sat_tp(1, 11_000_000, ms_tuner::Polarization::Horizontal)),
            101,
        );
        rig.add_host_adapter(
            5,
            Some(sat_tp(1, 11_500_000, ms_tuner::Polarization::Vertical)),
            202,
        );
        rig.add_service(0, 4, 10, 0x0101, 601, 1301);
        rig.add_service(1, 5, 11, 0x0202, 701, 1401);

        // Warm-up seeds `joined`; then A sees 9.1% (33/363) and 12.0% (45/375) with 330 clear (< 1000) in-join.
        rig.step_window(&[(1301, 33, 330), (1401, 0, 20_000)]);
        rig.step_window(&[(1301, 33, 330), (1401, 0, 20_000)]);
        rig.step_window(&[(1301, 45, 330), (1401, 0, 20_000)]);
        // Both go 100% stuck: A was never clean, so no alert may fire.
        for _ in 0..7 {
            rig.step_window(&[(1301, 25_000, 0), (1401, 25_000, 0)]);
        }
        assert_eq!(
            rig.worker.registry.get(0).unwrap().cam_stuck_last_alert_ms,
            -1
        );

        // Once A gets a real >= 1000-clear window, regresses, and sees (33 scr, 330 clr) mid-confirmation, it is not reset as clean.
        rig.step_window(&[(1301, 0, 2_000), (1401, 0, 2_000)]);
        rig.step_window(&[(1301, 25_000, 0), (1401, 25_000, 0)]);
        rig.step_window(&[(1301, 25_000, 0), (1401, 25_000, 0)]);
        // 2nd confirmation window for A is (33 scr, 330 clr = 9.1% scr, < 1000 clr): counts as neutral, not clean!
        rig.step_window(&[(1301, 33, 330), (1401, 25_000, 0)]);
        assert_eq!(
            rig.worker.registry.get(0).unwrap().cam_stuck_last_alert_ms,
            -1
        );
        // 3rd confirmation window stuck: alerts immediately!
        rig.step_window(&[(1301, 25_000, 0), (1401, 25_000, 0)]);
        assert_eq!(
            rig.worker.registry.get(0).unwrap().cam_stuck_last_alert_ms,
            rig.clock.tick_ms()
        );
    }

    // 7) ~11-13% scrambled with >= 1000 clear packets neither marks a service clean in its join nor ends a regression.
    #[test]
    fn cam_stuck_11_to_13_percent_scrambled_with_1000_plus_clear_is_not_clean() {
        let mut rig = CamStuckRig::new(2);
        rig.add_host_adapter(
            4,
            Some(sat_tp(1, 11_000_000, ms_tuner::Polarization::Horizontal)),
            101,
        );
        rig.add_host_adapter(
            5,
            Some(sat_tp(1, 11_500_000, ms_tuner::Polarization::Vertical)),
            202,
        );
        rig.add_service(0, 4, 10, 0x0101, 601, 1301);
        rig.add_service(1, 5, 11, 0x0202, 701, 1401);

        // Service A sees 11.5% (2600/22600) and 13.0% (3000/23000) scrambled with 20_000 clear (>= 1000), while B is clean.
        rig.step_window(&[(1301, 2_600, 20_000), (1401, 0, 20_000)]);
        rig.step_window(&[(1301, 3_000, 20_000), (1401, 0, 20_000)]);
        // Both go 100% stuck: A was never < 10% scrambled in its join, so no alert may fire.
        for _ in 0..7 {
            rig.step_window(&[(1301, 25_000, 0), (1401, 25_000, 0)]);
        }
        assert_eq!(
            rig.worker.registry.get(0).unwrap().cam_stuck_last_alert_ms,
            -1
        );

        // Once A gets a clean (< 10% scrambled, >= 1000 clear) window and regresses, (2600 scr, 20000 clr) does not end the regression.
        rig.step_window(&[(1301, 0, 20_000), (1401, 0, 20_000)]);
        rig.step_window(&[(1301, 25_000, 0), (1401, 25_000, 0)]);
        rig.step_window(&[(1301, 25_000, 0), (1401, 25_000, 0)]);
        rig.step_window(&[(1301, 2_600, 20_000), (1401, 25_000, 0)]);
        assert_eq!(
            rig.worker.registry.get(0).unwrap().cam_stuck_last_alert_ms,
            -1
        );
        rig.step_window(&[(1301, 25_000, 0), (1401, 25_000, 0)]);
        assert_eq!(
            rig.worker.registry.get(0).unwrap().cam_stuck_last_alert_ms,
            rig.clock.tick_ms()
        );
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
        opts.ci_plus_recovery = 7;
        let settings = DvbcaSettings::from_options(&opts);
        assert_eq!(settings.pins.get(&1).map(String::as_str), Some("0000"));
        assert!(settings.force_ci.contains(&2));
        assert_eq!(settings.limits(1), (3, 2));
        assert_eq!(settings.limits(0), (DEFAULT_CA_PMT, 0));
        assert_eq!(settings.channels[&1].2, vec![0x0B00, 0x1702]);
        assert_eq!(settings.ci_plus_recovery_ms, 7000, "seconds become ms");
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
            serial: 0,
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
            // Nonzero, so the notices prove the generation is passed through.
            generation: 7,
            ..CaService::default()
        }
    }

    fn drain(rx: &Receiver<Notice>) -> Vec<Notice> {
        std::iter::from_fn(|| rx.try_recv().ok()).collect()
    }

    #[test]
    fn ciplus_keys_are_forwarded_for_offered_pmts() {
        // Initialise the capture before the worker emits anything.
        #[cfg(feature = "diag-log")]
        let _ = super::diag_capture::lines();
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
        let joined = drain(&rx);

        // The handshake already delivered a key in register 0; the update must
        // prove it is a new one and not a replay of the init key.
        let init = cam.lock().unwrap().ciplus_status().expect("CI+ CAM");
        let init_key = init.expected_key.expect("the handshake sent a key");
        let init_iv = init.expected_iv.expect("the handshake sent an IV");
        assert_eq!(init.expected_parity, Some(0), "the handshake key register");

        // ca.cpp:1330: the joining PMT gets the stored key at once, not at the next rotation.
        let replay = joined
            .iter()
            .find_map(|notice| match notice {
                Notice::CcKey {
                    pmt_ids,
                    parity: 0,
                    key,
                    iv,
                    expiry_s,
                } if pmt_ids == &[(program.id as usize, program.generation)] => {
                    Some((key.0, iv.0, *expiry_s))
                }
                _ => None,
            })
            .expect("the stored key is replayed to the joining PMT");
        assert!(replay.0 == init_key, "the replayed key is the stored one");
        assert!(replay.1 == init_iv, "the replayed IV is the stored one");
        assert_eq!(replay.2, 3600, "a replayed key lives 3600 s");
        assert!(
            !joined
                .iter()
                .any(|notice| matches!(notice, Notice::CcKey { parity: 1, .. })),
            "register 1 is still empty, so nothing is replayed for it"
        );

        // ca.cpp:993-996: an unchanged update is not re-sent but still counts as sent,
        // and a stored key whose first byte is 0x00 is still replayed.
        worker.registry.get_mut(0).unwrap().key[0][0] = 0;
        let (reply, answers) = mpsc::channel();
        worker.handle(Request::AddPmt {
            program: Box::new(program.clone()),
            update: true,
            reply,
        });
        assert_eq!(answers.recv().ok(), Some(TablesResult::Ok));
        assert!(
            drain(&rx).iter().any(|notice| matches!(
                notice,
                Notice::CcKey { pmt_ids, parity: 0, expiry_s: 3600, .. }
                    if pmt_ids == &[(program.id as usize, program.generation)]
            )),
            "an unchanged update replays the stored key too"
        );

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
        let Notice::DisableCws(disabled) = &notices[disable] else {
            panic!("matched above")
        };
        assert!(
            disabled.contains(&(program.id as usize, program.generation)),
            "the disabled PMT keeps its generation: {disabled:?}"
        );
        let Notice::CcKey {
            pmt_ids,
            parity,
            key,
            iv,
            expiry_s,
        } = &notices[key_pos]
        else {
            panic!("matched above")
        };
        assert!(
            pmt_ids.contains(&(program.id as usize, program.generation)),
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
        assert_eq!(*expiry_s, 3720, "a new key lives 3720 s");

        #[cfg(feature = "diag-log")]
        {
            let captured = super::diag_capture::events("ciplus_key");
            let event = captured
                .last()
                .unwrap_or_else(|| panic!("no ciplus_key event: {captured:?}"));
            assert!(event.contains("\"ca\":0"), "{event}");
            assert!(event.contains("\"parity\":1"), "{event}");
            assert!(event.contains("\"target_adapters\":[0]"), "{event}");
        }
        pki.remove();
    }

    #[test]
    fn released_notice_keeps_the_generation() {
        let cam = Arc::new(Mutex::new(FakeCam::new(
            CamConfig::default(),
            Arc::new(ms_fakecam::ManualClock::new(0)),
        )));
        let clock = Arc::new(ManualClock::new(0, UNIX));
        let worker_clock: Arc<dyn Clock> = clock.clone();
        let (tx, rx) = mpsc::channel();
        let mut worker = Worker::new(
            DvbcaSettings::default(),
            Box::new(CiOpener {
                cam: Arc::clone(&cam),
                cert_dir: PathBuf::new(),
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

        worker.registry.get_mut(0).unwrap().teardown();
        worker.forward_events(0);
        let released = drain(&rx);
        assert!(
            released.iter().any(|notice| matches!(
                notice,
                Notice::Released(pmts)
                    if pmts == &[(program.id, program.generation)]
            )),
            "a released PMT keeps its generation: {released:?}"
        );
    }
}

/// Captures diagnostic events for the unit tests (one `init` per process).
#[cfg(all(test, feature = "diag-log"))]
mod diag_capture {
    use std::sync::{Arc, Mutex, OnceLock};

    use ms_log::Backend;

    /// Lines for `event` emitted by the current test thread, so parallel tests
    /// cannot hide this test's event.
    pub(crate) fn events(event: &str) -> Vec<String> {
        let thread = std::thread::current().name().unwrap_or_default().to_owned();
        let event = format!("\"event\":\"{event}\"");
        let thread = format!("\"thread\":\"{thread}\"");
        lines()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter(|line| line.contains(&event) && line.contains(&thread))
            .cloned()
            .collect()
    }

    pub(crate) fn lines() -> &'static Arc<Mutex<Vec<String>>> {
        static LINES: OnceLock<Arc<Mutex<Vec<String>>>> = OnceLock::new();
        LINES.get_or_init(|| {
            let lines = Arc::new(Mutex::new(Vec::new()));
            ms_log::init(ms_log::Config {
                log: ms_log::ALL_LOG_MODULES | ms_log::LOG_GENERAL,
                debug: 0,
                slog: 0,
                file_line: false,
                backend: Box::new(Capture(Arc::clone(&lines))),
            });
            lines
        })
    }

    struct Capture(Arc<Mutex<Vec<String>>>);

    impl Backend for Capture {
        fn timestamp(&self) -> ms_log::Timestamp {
            ms_log::Timestamp::new(1, 1, 0, 0, 0, 0)
        }

        fn syslog(&self, _line: &str) {}

        fn stdout(&self, line: &str) {
            self.0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(line.to_owned());
        }
    }
}

/// One `ServiceWindows` request as a test reads it back: adapter, window, services.
#[cfg(test)]
type ServiceWindowReport = (usize, i64, Vec<(u16, u64, u64)>);
