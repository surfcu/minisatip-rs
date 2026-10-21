//! Sans-IO DVBAPI client: key table, CAPMT sending and server message handling.
//!
//! Mirrors the state of `dvbapi.cpp` without globals; time is passed in as
//! milliseconds and socket writes are queued for the caller.

use std::collections::{HashMap, VecDeque};

use ms_ca::{CaCaps, CaDriver, CaError, CaEvent, CaService, CapmtDiff, CapmtOp, CwAlgo, FilterId};

use super::proto::{
    self, CLM_ADD, CLM_ONLY, CLM_UPDATE, CMD_ID_NOT_SELECTED, CMD_ID_OK_DESCRAMBLING,
    CW_ALGO_AES128, CW_ALGO_CSA, CW_ALGO_CSA_ALT, CW_ALGO_DES, CW_ALGO_MODE_ECB, Capmt, EcmInfo,
    Parsed, ServerMsg,
};

/// Key slots (`MAX_KEYS`).
pub const MAX_KEYS: usize = 255;
/// Filters per key (`MAX_KEY_FILTERS`).
pub const MAX_KEY_FILTERS: usize = 20;
/// Oldest OSCam build that is registered (`< 11533` is refused).
pub const MIN_OSCAM_BUILD: i32 = 11533;
/// Scrambled packets without a decrypt for this long count as a stall.
pub const STALL_MS: i64 = 3000;
/// A new key waits this long for its CWs (doubled per restart, up to 6 times).
pub const KEY_GRACE_MS: i64 = 5000;
/// A scrambled packet this recent means the service is still scrambled.
pub const SCRAMBLED_RECENT_MS: i64 = 2000;
/// A key without filters this long after its last DMX_STOP is closed.
pub const NO_FILTER_CLOSE_MS: i64 = 3000;
/// The ECM parity filter resets after this long without ECM or CW.
pub const ECM_PARITY_RESET_MS: i64 = 1000;
/// Receive buffer size; a larger unframed backlog is dropped like C++.
pub const READ_BUFFER: usize = 8192;

/// DVBAPI settings taken from the `-o` and `--send-all-ecm` options.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DvbapiConfig {
    /// Host name, IP address or (starting with `/`) unix socket path.
    pub host: String,
    /// TCP port; 0 disables DVBAPI.
    pub port: i32,
    /// Offset added to key ids on the wire (several clients on one server).
    pub offset: i32,
    /// Forward every ECM, not only parity changes.
    pub send_all_ecm: bool,
    /// `-o ~`: keep scrambled packets for `pids=all` (used by the data plane).
    pub pids_all_no_dec: bool,
    /// Name sent in CLIENT_INFO (C++ `app_name/version`).
    pub client_name: String,
}

impl Default for DvbapiConfig {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: 0,
            offset: 0,
            send_all_ecm: false,
            pids_all_no_dec: false,
            client_name: format!("minisatip/{}", env!("CARGO_PKG_VERSION")),
        }
    }
}

impl DvbapiConfig {
    /// Settings from parsed command-line options.
    pub fn from_options(opts: &ms_config::Options) -> Self {
        Self {
            host: opts.dvbapi_host.clone(),
            port: opts.dvbapi_port,
            offset: opts.dvbapi_offset,
            send_all_ecm: opts.send_all_ecm != 0,
            pids_all_no_dec: opts.pids_all_no_dec != 0,
            ..Self::default()
        }
    }

    /// Whether a server is configured at all.
    pub fn is_configured(&self) -> bool {
        self.port != 0 && !self.host.is_empty()
    }

    /// TCP mode; a `/` path selects the local socket mode (`network_mode = 0`).
    pub fn network_mode(&self) -> bool {
        !self.host.starts_with('/')
    }
}

/// One section filter requested by the server for a key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyFilter {
    /// Filter handle, `None` for a free slot.
    pub id: Option<FilterId>,
    /// Server filter number.
    pub filter: i32,
    /// Server demux index.
    pub demux: i32,
    /// PID, -1 after DMX_STOP.
    pub pid: i32,
    /// Parity of the last forwarded ECM, -1 for none.
    pub ecm_parity: i32,
}

impl Default for KeyFilter {
    fn default() -> Self {
        Self {
            id: None,
            filter: -1,
            demux: -1,
            pid: -1,
            ecm_parity: -1,
        }
    }
}

/// One DVBAPI key slot (`SKey`); slots and their stale fields are reused.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Key {
    /// Slot index.
    pub id: usize,
    /// Whether the key is live.
    pub enabled: bool,
    /// PMT slot id of the service.
    pub pmt_id: usize,
    /// Logical adapter, -1 when deleted.
    pub adapter: i32,
    /// Algorithm for the next CW; like C++ it survives slot reuse.
    pub algo: CwAlgo,
    /// Last CWs per parity.
    pub cw: [[u8; 16]; 2],
    /// Length of the last CW.
    pub key_len: usize,
    /// Service id sent in the CAPMT.
    pub sid: u16,
    /// PMT PID.
    pub pmt_pid: u16,
    /// Last ECM forwarded (ms).
    pub last_ecm: i64,
    /// Last DMX_STOP or (re)send (ms), 0 for never.
    pub last_dmx_stop: i64,
    /// ECM_INFO hops.
    pub hops: u8,
    /// ECM_INFO CAID.
    pub caid: u16,
    /// ECM_INFO PID.
    pub info_pid: u16,
    /// ECM_INFO provider id.
    pub prid: u32,
    /// ECM_INFO ECM time (ms).
    pub ecm_time: i32,
    /// Demux index of the last CAPMT, -1 before any.
    pub demux_index: i32,
    /// Adapter index of the last CAPMT, -1 before any.
    pub adapter_index: i32,
    /// Version marker, -1 for a fresh key.
    pub ver: i32,
    /// Active filters.
    pub ecms: i32,
    /// ECM_INFO card system.
    pub cardsystem: Vec<u8>,
    /// ECM_INFO reader.
    pub reader: Vec<u8>,
    /// ECM_INFO source.
    pub from: Vec<u8>,
    /// ECM_INFO protocol.
    pub protocol: Vec<u8>,
    /// CW parity (unused by the protocol, reset to -1).
    pub parity: i32,
    /// Transport stream id.
    pub tsid: u16,
    /// Original network id.
    pub onid: u16,
    /// Filter slots.
    pub filters: [KeyFilter; MAX_KEY_FILTERS],
    /// iCAM mode of the last ECM.
    pub icam_ecm: u8,
    /// Whether the last ECM was an iCAM ECM.
    pub is_icam: bool,
    /// Last CAPMT sent, with list management zeroed (C++ keeps a CRC).
    pub capmt: Option<Vec<u8>>,
    /// Creation time (ms).
    pub created: i64,
    /// Start of the current stall (ms), 0 for none.
    pub stall_start: i64,
}

/// Per-service descrambling state the watchdog reads (C++ `SPMT` stamps).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PmtStatus {
    /// PMT slot id.
    pub pmt_id: usize,
    /// Adapter of the PMT.
    pub adapter: usize,
    /// Whether the PMT slot is in use.
    pub enabled: bool,
    /// Whether the PMT is in `PMT_RUNNING`.
    pub running: bool,
    /// Last successful decrypt (ms), 0 for never.
    pub last_decrypt_ok: i64,
    /// Last scrambled packet (ms).
    pub last_scrambled_pkt: i64,
}

#[derive(Clone, Copy, Debug, Default)]
struct Watchdog {
    decrypt_ok: i64,
    restarts: u32,
}

/// What the IO layer should do after [`DvbapiClient::poll`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PollAction {
    /// Nothing.
    None,
    /// Open a connection to the server.
    Connect,
}

/// The DVBAPI protocol state machine.
#[derive(Debug)]
pub struct DvbapiClient {
    cfg: DvbapiConfig,
    keys: Vec<Key>,
    services: HashMap<usize, CaService>,
    // pmt_id -> key slot, the C++ `pmt->opaque`.
    opaque: HashMap<usize, usize>,
    watchdog: HashMap<usize, Watchdog>,
    connected: bool,
    enabled: bool,
    registered: bool,
    close_requested: bool,
    protocol_version: u16,
    oscam_version: i32,
    next_filter: u32,
    rx: Vec<u8>,
    out: VecDeque<Vec<u8>>,
    events: VecDeque<CaEvent>,
    // Clock for the `CaDriver` methods; sans-IO callers pass time explicitly.
    now: i64,
}

impl DvbapiClient {
    /// A disconnected client.
    pub fn new(cfg: DvbapiConfig) -> Self {
        Self {
            cfg,
            keys: Vec::new(),
            services: HashMap::new(),
            opaque: HashMap::new(),
            watchdog: HashMap::new(),
            connected: false,
            enabled: false,
            registered: false,
            close_requested: false,
            protocol_version: proto::DVBAPI_PROTOCOL_VERSION,
            oscam_version: 0,
            next_filter: 1,
            rx: Vec::new(),
            out: VecDeque::new(),
            events: VecDeque::new(),
            now: 0,
        }
    }

    /// Time used by the [`CaDriver`] methods (milliseconds).
    pub fn set_time(&mut self, now: i64) {
        self.now = now;
    }

    /// Settings in use.
    pub fn config(&self) -> &DvbapiConfig {
        &self.cfg
    }

    /// Whether a connection is open (`sock > 0`).
    pub fn is_connected(&self) -> bool {
        self.connected
    }

    /// `dvbapi_is_enabled`: handshake started and no close pending.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Whether DVBAPI is registered as a CA (`dvbapi_ca >= 0`).
    pub fn is_registered(&self) -> bool {
        self.registered
    }

    /// Whether the IO layer must close the connection.
    pub fn close_requested(&self) -> bool {
        self.close_requested
    }

    /// Negotiated protocol version.
    pub fn protocol_version(&self) -> u16 {
        self.protocol_version
    }

    /// Server build number from SERVER_INFO, 0 if unknown.
    pub fn oscam_version(&self) -> i32 {
        self.oscam_version
    }

    /// All key slots, live or not (status API `key_*`).
    pub fn keys(&self) -> &[Key] {
        &self.keys
    }

    /// A live key.
    pub fn key(&self, id: usize) -> Option<&Key> {
        self.keys.get(id).filter(|k| k.enabled)
    }

    /// Live key of a service.
    pub fn key_for_pmt(&self, pmt_id: usize) -> Option<&Key> {
        self.opaque.get(&pmt_id).and_then(|&id| self.key(id))
    }

    /// Next queued write, one C++ `write()` each.
    pub fn take_output(&mut self) -> Option<Vec<u8>> {
        self.out.pop_front()
    }

    /// Next event.
    pub fn next_event(&mut self) -> Option<CaEvent> {
        self.events.pop_front()
    }

    fn write(&mut self, bytes: Vec<u8>) {
        if self.connected {
            self.out.push_back(bytes);
        }
    }

    fn get_key_index(&self, raw: i32) -> Option<usize> {
        let id = usize::try_from(raw).ok()?;
        (id < MAX_KEYS && self.keys.get(id)?.enabled).then_some(id)
    }

    fn wire_key(&self, raw: u8) -> Option<usize> {
        self.get_key_index(raw as i32 - self.cfg.offset)
    }

    /// Connection established: sends CLIENT_INFO (`send_client_info`).
    pub fn on_connected(&mut self, _now: i64) {
        self.connected = true;
        self.close_requested = false;
        self.rx.clear();
        let info = proto::client_info(self.protocol_version, &self.cfg.client_name);
        self.enabled = true;
        self.write(info);
    }

    /// `dvbapi_close_socket`: disable and let the IO layer close later.
    pub fn mark_close(&mut self) {
        self.enabled = false;
        if self.connected {
            self.close_requested = true;
        }
    }

    /// Connection gone (`dvbapi_close`): every key is deleted.
    pub fn on_closed(&mut self, now: i64) {
        self.connected = false;
        self.enabled = false;
        self.close_requested = false;
        self.out.clear();
        self.rx.clear();
        for i in 0..self.keys.len() {
            self.keys_del(i, now);
        }
        self.opaque.clear();
        self.registered = false;
        self.events.push_back(CaEvent::Closed);
    }

    /// Bytes read from the server; complete messages are handled in order.
    pub fn on_data(&mut self, data: &[u8], now: i64) {
        self.rx.extend_from_slice(data);
        if self.rx.len() > READ_BUFFER {
            self.rx.clear();
            return;
        }
        while let Parsed::Message(msg, used) = proto::parse_message(&self.rx) {
            self.rx.drain(..used);
            self.handle(msg, now);
        }
    }

    fn handle(&mut self, msg: ServerMsg, now: i64) {
        match msg {
            ServerMsg::ServerInfo { version, info } => self.server_info(version, &info),
            ServerMsg::SetFilter {
                key,
                demux,
                filter,
                pid,
                data,
                mask,
            } => self.set_filter(key, demux, filter, pid, data, mask),
            ServerMsg::DmxStop {
                key,
                demux,
                filter,
                pid,
            } => self.dmx_stop(key, demux, filter, pid, now),
            ServerMsg::CaSetPid { .. } | ServerMsg::Unknown(_) => {}
            ServerMsg::CaSetDescr {
                key, parity, cw, ..
            } => self.set_descr(key, parity, cw),
            ServerMsg::CaSetDescrAes => log_unknown_aes(),
            ServerMsg::CaSetDescrMode {
                key, algo, mode, ..
            } => {
                if let Some(i) = self.wire_key(key) {
                    set_algo(&mut self.keys[i], algo, mode);
                }
            }
            ServerMsg::EcmInfo(info) => self.ecm_info(info),
        }
    }

    fn server_info(&mut self, version: u16, info: &[u8]) {
        self.protocol_version = version.min(proto::DVBAPI_PROTOCOL_VERSION);
        if let Some(at) = find(info, b"build r") {
            self.oscam_version = parse_int(&info[at + 7..]);
        }
        self.enabled = true;
        if self.oscam_version > 0 && self.oscam_version < MIN_OSCAM_BUILD {
            self.events.push_back(CaEvent::Unsupported {
                build: self.oscam_version,
            });
        } else {
            self.registered = true;
            self.events.push_back(CaEvent::Ready);
        }
    }

    fn set_filter(
        &mut self,
        raw_key: u8,
        demux: u8,
        filter: u8,
        pid: u16,
        data: [u8; 16],
        mask: [u8; 16],
    ) {
        let Some(i) = self.wire_key(raw_key) else {
            return;
        };
        let emm = (0x82..=0x8F).contains(&(data[0] & mask[0]));
        let (demux, filter, pid) = (demux as i32, filter as i32, pid as i32);
        let k = &mut self.keys[i];
        // A repeated request keeps the existing filter (C++ only updates flags).
        if k.filters
            .iter()
            .any(|f| f.id.is_some() && f.pid == pid && f.demux == demux && f.filter == filter)
        {
            return;
        }
        let Some(slot) = k.filters.iter().position(|f| f.id.is_none()) else {
            return;
        };
        let id = FilterId(self.next_filter);
        self.next_filter = self.next_filter.wrapping_add(1).max(1);
        k.filters[slot] = KeyFilter {
            id: Some(id),
            filter,
            demux,
            pid,
            ecm_parity: -1,
        };
        k.ecms += 1;
        let event = CaEvent::SetFilter {
            id,
            pmt_id: k.pmt_id,
            adapter: k.adapter.max(0) as usize,
            pid: pid as u16,
            filter: data,
            mask,
            emm,
        };
        self.events.push_back(event);
    }

    fn dmx_stop(&mut self, raw_key: u8, demux: u8, filter: u8, pid: u16, now: i64) {
        let Some(i) = self.wire_key(raw_key) else {
            return;
        };
        let (demux, filter, pid) = (demux as i32, filter as i32, pid as i32);
        let k = &mut self.keys[i];
        if let Some(f) = k
            .filters
            .iter_mut()
            .find(|f| f.filter == filter && f.demux == demux && f.pid == pid)
        {
            if let Some(id) = f.id.take() {
                self.events.push_back(CaEvent::StopFilter { id });
            }
            f.pid = -1;
        }
        if k.ecms > 0 {
            k.ecms -= 1;
        }
        k.last_dmx_stop = now;
    }

    fn set_descr(&mut self, raw_key: u8, parity: i32, cw: [u8; 8]) {
        let Some(i) = self.wire_key(raw_key) else {
            return;
        };
        if !(0..2).contains(&parity) {
            return;
        }
        let k = &mut self.keys[i];
        // C++ stores 8 bytes for every CA_SET_DESCR, even in AES mode.
        k.key_len = 8;
        k.cw[parity as usize][..8].copy_from_slice(&cw);
        let algo = if k.is_icam { CwAlgo::CsaIcam } else { k.algo };
        self.events.push_back(CaEvent::ControlWord {
            pmt_id: k.pmt_id,
            algo,
            parity: parity as u8,
            cw: cw.to_vec(),
            icam_ecm: k.icam_ecm,
        });
    }

    fn ecm_info(&mut self, info: EcmInfo) {
        let Some(i) = self.wire_key(info.key) else {
            return;
        };
        let k = &mut self.keys[i];
        k.caid = info.caid;
        k.info_pid = info.pid;
        k.prid = info.prid;
        k.ecm_time = info.ecm_time;
        k.cardsystem = info.cardsystem;
        k.reader = info.reader;
        k.from = info.from;
        k.protocol = info.protocol;
        k.hops = info.hops;
    }

    /// `keys_add`: claims the lowest free slot for a known service.
    pub(crate) fn keys_add(&mut self, adapter: usize, pmt_id: usize, now: i64) -> Option<usize> {
        let sid = self.services.get(&pmt_id)?.sid;
        let i = match self.keys.iter().position(|k| !k.enabled) {
            Some(i) => i,
            None if self.keys.len() < MAX_KEYS => {
                self.keys.push(Key {
                    id: self.keys.len(),
                    ..Key::default()
                });
                self.keys.len() - 1
            }
            None => return None,
        };
        let k = &mut self.keys[i];
        k.parity = -1;
        k.sid = sid;
        k.pmt_id = pmt_id;
        k.adapter = adapter as i32;
        k.adapter_index = -1;
        k.demux_index = -1;
        k.ver = -1;
        k.ecms = 0;
        k.last_dmx_stop = 0;
        k.onid = 0;
        k.tsid = 0;
        k.icam_ecm = 0;
        k.is_icam = false;
        k.capmt = None;
        k.created = now;
        k.stall_start = 0;
        k.cw = [[0; 16]; 2];
        for f in &mut k.filters {
            f.id = None;
            f.filter = -1;
            f.demux = -1;
        }
        k.enabled = true;
        Some(i)
    }

    /// `keys_del`: CA_STOP (or NOT_SELECTED on a shared demux) and filter teardown.
    pub(crate) fn keys_del(&mut self, i: usize, _now: i64) {
        if !self.keys.get(i).is_some_and(|k| k.enabled) {
            return;
        }
        self.keys[i].enabled = false;
        let demux = self.keys[i].demux_index;
        let others = self.keys.iter().filter(|o| o.enabled);
        let (ek, ed) = others.fold((0, 0), |(ek, ed), o| {
            (ek + 1, ed + usize::from(o.demux_index == demux))
        });
        if ek == 0 {
            self.write(proto::ca_stop(0xFF).to_vec());
        } else if ed == 0 {
            self.write(proto::ca_stop(demux as u8).to_vec());
        } else {
            self.send_pmt(i, CMD_ID_NOT_SELECTED, true);
        }
        let k = &mut self.keys[i];
        k.sid = 0;
        k.pmt_pid = 0;
        k.adapter = -1;
        k.last_dmx_stop = 0;
        k.demux_index = -1;
        k.adapter_index = -1;
        for f in &mut k.filters {
            if let Some(id) = f.id.take() {
                self.events.push_back(CaEvent::StopFilter { id });
            }
        }
        k.hops = 0;
        k.caid = 0;
        k.info_pid = 0;
        k.prid = 0;
        k.ecm_time = 0;
    }

    /// `dvbapi_send_pmt`: builds the CAPMT and skips an unchanged update.
    fn send_pmt(&mut self, i: usize, cmd_id: u8, force: bool) {
        let network_mode = self.cfg.network_mode();
        let offset = self.cfg.offset;
        let others_live = self.keys.iter().any(|o| o.id != i && o.enabled);
        let k = &self.keys[i];
        let Some(service) = self.services.get(&k.pmt_id) else {
            return;
        };
        let (demux, adapter) = if network_mode {
            let v = k.id as i32 + offset;
            (v, v)
        } else {
            (service.hw_demux as i32, service.hw_adapter as i32)
        };
        let mut listmgmt = CLM_UPDATE;
        if cmd_id == CMD_ID_OK_DESCRAMBLING {
            listmgmt = if others_live { CLM_ADD } else { CLM_ONLY };
        }
        // List management is framing, not content: compare with it zeroed.
        let mut bytes = proto::build_capmt(&Capmt {
            list_management: 0,
            cmd_id,
            sid: k.sid,
            adapter: adapter as u8,
            demux: demux as u8,
            pmt_pid: service.pmt_pid,
            ca: &service.ca,
            streams: &service.streams,
        });
        let k = &mut self.keys[i];
        k.demux_index = demux;
        k.adapter_index = adapter;
        if !force && k.capmt.as_deref() == Some(&bytes[..]) {
            return;
        }
        if self.connected {
            k.capmt = Some(bytes.clone());
            bytes[6] = listmgmt;
            self.out.push_back(bytes);
        }
    }

    /// `dvbapi_add_pmt`: re-sends on the live key or allocates a new one.
    pub fn add_service(
        &mut self,
        service: &CaService,
        update: bool,
        now: i64,
    ) -> Result<(), CaError> {
        if !self.connected {
            return Err(CaError::NotConnected);
        }
        let pmt_id = service.pmt_id;
        self.services.insert(pmt_id, service.clone());
        if let Some(&old) = self.opaque.get(&pmt_id) {
            let k = &mut self.keys[old];
            if k.enabled && k.pmt_id == pmt_id && k.adapter == service.adapter as i32 {
                k.sid = service.sid;
                k.pmt_pid = service.pmt_pid;
                k.tsid = service.tsid;
                k.last_dmx_stop = now;
                self.send_pmt(old, CMD_ID_OK_DESCRAMBLING, !update);
                return Ok(());
            }
        }
        let i = self
            .keys_add(service.adapter, pmt_id, now)
            .ok_or(CaError::NoSlot)?;
        self.opaque.insert(pmt_id, i);
        let k = &mut self.keys[i];
        k.sid = service.sid;
        k.adapter = service.adapter as i32;
        k.pmt_pid = service.pmt_pid;
        k.tsid = service.tsid;
        k.onid = 0;
        k.last_dmx_stop = now;
        self.send_pmt(i, CMD_ID_OK_DESCRAMBLING, true);
        Ok(())
    }

    /// `dvbapi_del_pmt`.
    pub fn remove_service(&mut self, pmt_id: usize, now: i64) {
        if let Some(i) = self.opaque.remove(&pmt_id) {
            self.keys_del(i, now);
        }
        self.services.remove(&pmt_id);
    }

    // close_pmt_for_ca(): drop the key and let the reconciler resend.
    fn restart_service(&mut self, pmt_id: usize, now: i64) {
        if let Some(i) = self.opaque.remove(&pmt_id) {
            self.keys_del(i, now);
        }
        self.events.push_back(CaEvent::Restart { pmt_id });
    }

    /// `send_ecm`: forwards a filtered section as FILTER_DATA.
    pub fn deliver_section(&mut self, id: FilterId, b: &[u8], cw_valid: bool, now: i64) {
        if !self.enabled || b.len() < 3 {
            return;
        }
        let Some((i, slot)) = self.keys.iter().enumerate().find_map(|(i, k)| {
            let slot = k.filters.iter().position(|f| f.id == Some(id))?;
            k.enabled.then_some((i, slot))
        }) else {
            return;
        };
        if !self.services.contains_key(&self.keys[i].pmt_id) {
            return;
        }
        let send_all = self.cfg.send_all_ecm;
        let k = &mut self.keys[i];
        let f = &mut k.filters[slot];
        if now - k.last_ecm > ECM_PARITY_RESET_MS && !cw_valid {
            f.ecm_parity = -1;
        }
        let is_ecm = b[0] == 0x80 || b[0] == 0x81;
        if !send_all && is_ecm && (b[0] & 1) as i32 == f.ecm_parity {
            return;
        }
        f.ecm_parity = (b[0] & 1) as i32;
        let (demux, filter) = (f.demux, f.filter);
        let len = ((((b[1] & 0x0F) as usize) << 8) | b[2] as usize) + 3;
        k.last_ecm = now;
        // Only ECMs set the iCAM mode: CAT and SDT share the key's filters.
        if is_ecm {
            if len > 0x15 && b.len() > 0x15 && b[2] as i32 - b[4] as i32 == 4 {
                k.icam_ecm = b[0x15];
                k.is_icam = k.icam_ecm != 0;
            } else {
                k.icam_ecm = 0;
                k.is_icam = false;
            }
        }
        if demux < 0 || len > b.len() {
            return;
        }
        if let Some(msg) = proto::filter_data(demux as u8, filter as u8, &b[..len]) {
            self.write(msg);
        }
    }

    /// The 1 s `connect_dvbapi` tick: closes keys without filters, asks to connect.
    pub fn poll(&mut self, now: i64, adapters_active: bool) -> PollAction {
        self.now = now;
        if self.connected && self.enabled {
            if self.cfg.network_mode() {
                for i in 0..self.keys.len() {
                    let k = &self.keys[i];
                    if k.enabled
                        && k.ecms == 0
                        && k.last_dmx_stop > 0
                        && now - k.last_dmx_stop > NO_FILTER_CLOSE_MS
                    {
                        let pmt_id = k.pmt_id;
                        self.keys_del(i, now);
                        self.restart_service(pmt_id, now);
                    }
                }
            }
            if !adapters_active {
                self.mark_close();
            }
            return PollAction::None;
        }
        self.enabled = false;
        if !self.cfg.is_configured() || !adapters_active || self.connected {
            return PollAction::None;
        }
        PollAction::Connect
    }

    /// `dvbapi_descramble_watchdog` for one adapter.
    pub fn watchdog(&mut self, adapter: usize, now: i64, pmts: &[PmtStatus]) {
        if !self.enabled || !self.registered {
            return;
        }
        for p in pmts {
            if p.adapter != adapter || !p.enabled || !p.running || p.last_decrypt_ok == 0 {
                continue;
            }
            let Some(&i) = self.opaque.get(&p.pmt_id) else {
                continue;
            };
            let k = &mut self.keys[i];
            if !k.enabled || k.pmt_id != p.pmt_id {
                continue;
            }
            if now - p.last_scrambled_pkt > SCRAMBLED_RECENT_MS
                || p.last_decrypt_ok >= p.last_scrambled_pkt
            {
                k.stall_start = 0;
                continue;
            }
            if k.stall_start == 0 {
                k.stall_start = now;
            }
            let w = self.watchdog.entry(p.pmt_id).or_default();
            if w.decrypt_ok != p.last_decrypt_ok {
                *w = Watchdog {
                    decrypt_ok: p.last_decrypt_ok,
                    restarts: 0,
                };
            }
            let grace = KEY_GRACE_MS << w.restarts.min(6);
            if now - k.stall_start < STALL_MS || now - k.created < grace {
                continue;
            }
            w.restarts += 1;
            self.restart_service(p.pmt_id, now);
        }
    }

    /// `get_channel_for_key`: the PMT name, or `SID n` before any SDT.
    pub fn key_channel(&self, id: usize, pmt: impl Fn(usize) -> Option<(String, u16)>) -> String {
        let Some(k) = self.key(id) else {
            return String::new();
        };
        match pmt(k.pmt_id) {
            Some((name, _)) if !name.is_empty() => name,
            Some((_, sid)) => format!("SID {sid}"),
            None => String::new(),
        }
    }
}

impl CaDriver for DvbapiClient {
    fn capabilities(&self) -> CaCaps {
        CaCaps {
            max_services: Some(MAX_KEYS),
        }
    }

    fn apply(&mut self, diff: &CapmtDiff) -> Result<(), CaError> {
        let now = self.now;
        for op in &diff.ops {
            match op {
                CapmtOp::Add(service) | CapmtOp::Update(service) => {
                    let update = matches!(op, CapmtOp::Update(_));
                    self.add_service(service, update, now)?;
                }
                CapmtOp::Remove { pmt_id } => self.remove_service(*pmt_id, now),
            }
        }
        Ok(())
    }

    fn events(&mut self) -> Option<CaEvent> {
        self.next_event()
    }
}

/// `set_algo`: maps a CA_SET_DESCR_MODE algorithm; unknown values are ignored.
fn set_algo(k: &mut Key, algo: i32, mode: i32) {
    k.algo = match algo {
        CW_ALGO_CSA | CW_ALGO_CSA_ALT => CwAlgo::Csa,
        CW_ALGO_DES => CwAlgo::Des,
        CW_ALGO_AES128 if mode == CW_ALGO_MODE_ECB => CwAlgo::Aes128Ecb,
        CW_ALGO_AES128 => CwAlgo::Aes128Cbc,
        _ => return,
    };
}

/// `CA_SET_DESCR_AES` has no C++ `case`; C++ logs the unknown operation and
/// drops the rest of the read (plain `LOG`, level 0 and unconditional).
fn log_unknown_aes() {
    eprintln!(
        "dvbapi: unknown operation: {:08X}",
        proto::DVBAPI_CA_SET_DESCR_AES
    );
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// C++ `parse_int`: optional sign and leading digits, 0 on failure or overflow.
fn parse_int(b: &[u8]) -> i32 {
    let s = std::str::from_utf8(b).unwrap_or("").trim_start();
    let s = s.strip_prefix('+').unwrap_or(s);
    let (neg, digits) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s),
    };
    let end = digits
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(digits.len());
    let text = if neg {
        format!("-{}", &digits[..end])
    } else {
        digits[..end].to_string()
    };
    text.parse().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_int_like_cpp() {
        assert_eq!(parse_int(b"11720 (x86_64-linux)"), 11720);
        assert_eq!(parse_int(b" -5x"), -5);
        assert_eq!(parse_int(b"abc"), 0);
        assert_eq!(parse_int(b"99999999999"), 0);
    }

    #[test]
    fn set_algo_maps_modes() {
        let mut k = Key::default();
        set_algo(&mut k, CW_ALGO_AES128, CW_ALGO_MODE_ECB);
        assert_eq!(k.algo, CwAlgo::Aes128Ecb);
        set_algo(&mut k, CW_ALGO_AES128, 1);
        assert_eq!(k.algo, CwAlgo::Aes128Cbc);
        set_algo(&mut k, 9, 0);
        assert_eq!(k.algo, CwAlgo::Aes128Cbc);
        set_algo(&mut k, CW_ALGO_DES, 0);
        assert_eq!(k.algo, CwAlgo::Des);
        set_algo(&mut k, CW_ALGO_CSA_ALT, 0);
        assert_eq!(k.algo, CwAlgo::Csa);
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod cpp_tests;
