//! Web UI and generated HTTP endpoints, mirroring the C++ `read_http`.
//!
//! Static files are served from `document_root`; HTML/XML files go through
//! the symbol table like C++ `process_file` (utils.cpp:379).

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use ms_sched::MAX_ADAPTERS;
use ms_tuner::DeliverySystem;

use crate::api::{
    CBuf, Raw, Symbol, SymbolProvider, SymbolTable, VAR_AARRAY_INT, VAR_AARRAY_INT8,
    VAR_AARRAY_STRING, VAR_AARRAY_UINT8, VAR_AARRAY_UINT16, VAR_ARRAY_INT8, VAR_FUNCTION_INT,
    VAR_FUNCTION_STRING, VAR_INT, VAR_PSTRING, VAR_STRING, strncpy_c,
};
use crate::handler::{HttpRoute, MAX_STREAMS};
use crate::rtsp::Request;
use crate::ssdp::{DeviceIdentity, SharedIdentity};

/// UPnP device description path served by C++ (`DESC_XML`).
pub const DESC_XML_PATH: &str = "/desc.xml";
/// Live state endpoint.
pub const STATE_JSON_PATH: &str = "/state.json";
/// Bandwidth counters endpoint.
pub const BANDWIDTH_JSON_PATH: &str = "/bandwidth.json";
/// Bytes C++ keeps for `/desc.xml` (`char buf[2000]`, minisatip.cpp:1462).
const DESC_XML_MAX: usize = 1999;
/// DVBAPI key slots (`MAX_KEYS`, dvbapi.h:56).
pub const MAX_KEYS: i32 = 255;
/// `QAM_AUTO`, the `ad_mtype` default (adapter.cpp:2397).
pub const QAM_AUTO: i32 = 6;
/// `GUARD_INTERVAL_AUTO`, the `ad_gi` default (adapter.cpp:2403).
pub const GUARD_INTERVAL_AUTO: i32 = 4;
/// `adapter::type` of a Linux DVB adapter (adapter.h:22).
pub const ADAPTER_DVB: i32 = 1;

/// Optional C++ subsystems whose symbol tables are compiled in
/// (symbols.cpp:27-41); every one is part of the default build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApiFeatures {
    /// `ca_sym` (no `DISABLE_DVBCA`).
    pub dvbca: bool,
    /// `dvbapi_sym` (no `DISABLE_DVBAPI`).
    pub dvbapi: bool,
    /// `satipc_sym` (no `DISABLE_SATIPCLIENT`).
    pub satipc: bool,
    /// `pmt_sym` and `has_pmt = 1` (no `DISABLE_PMT`).
    pub pmt: bool,
}

impl Default for ApiFeatures {
    fn default() -> Self {
        Self {
            dvbca: true,
            dvbapi: true,
            satipc: true,
            pmt: true,
        }
    }
}

/// Configuration of the HTTP file server and generated pages.
#[derive(Debug, Clone)]
pub struct WebConfig {
    /// Directory containing the web UI.
    pub document_root: PathBuf,
    /// Service name (`--name`).
    pub name_app: String,
    /// Version reported in generated pages and templates.
    pub version: String,
    /// UUID without the `uuid:` prefix.
    pub uuid: String,
    /// `host:port` used in URLs and SSDP headers.
    pub http_host: String,
    /// RTSP host reported in `state.json`.
    pub rtsp_host: String,
    /// HTTP port reported in `state.json`.
    pub http_port: i32,
    /// RTSP port advertised by `desc.xml`.
    pub rtsp_port: i32,
    /// Device description file name.
    pub xml_path: String,
    /// Optional playlist appended to `desc.xml`.
    pub playlist: Option<String>,
    /// UPnP boot id.
    pub bootid: i32,
    /// SAT>IP device id.
    pub device_id: i32,
    /// `-a` server count; nonzero pins the DVBS2 tuner count
    /// (adapter.cpp:478-480).
    pub force_sadapter: i32,
    /// `-a` slave count; nonzero pins the DVBT tuner count.
    pub force_tadapter: i32,
    /// `-a` ca count; nonzero pins the DVBC tuner count.
    pub force_cadapter: i32,
    /// Symbol tables included in the API.
    pub features: ApiFeatures,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            document_root: PathBuf::from("html"),
            name_app: "minisatip".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            uuid: String::new(),
            http_host: "127.0.0.1:8080".to_owned(),
            rtsp_host: "127.0.0.1".to_owned(),
            http_port: 8080,
            rtsp_port: 554,
            xml_path: "desc.xml".to_owned(),
            playlist: None,
            bootid: 0,
            device_id: 0,
            force_sadapter: 0,
            force_tadapter: 0,
            force_cadapter: 0,
            features: ApiFeatures::default(),
        }
    }
}

/// Runtime values shown in the status page (`minisatip_sym`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeState {
    /// Build date/time.
    pub datetime_compile: String,
    /// Process start date/time.
    pub datetime_start: String,
    /// Current date/time.
    pub datetime_current: String,
    /// Human-readable uptime.
    pub time_running: String,
    /// Numeric user id.
    pub run_user: i32,
    /// Process id.
    pub run_pid: i32,
    /// Original command line.
    pub command_line: String,
    /// Start time; when set, every HTTP request refreshes the current time
    /// and uptime like `read_http` (minisatip.cpp:1536-1553).
    pub start_time: Option<SystemTime>,
}

impl RuntimeState {
    /// Values as `read_http` leaves them for a request at `now`.
    pub fn at(&self, now: SystemTime) -> Self {
        let mut state = self.clone();
        if let Some(start) = self.start_time {
            state.datetime_current = format_asctime(now);
            state.time_running = format_running(start, now);
        }
        state
    }
}

/// `asctime(localtime())` with the newline replaced by a space, as minisatip
/// stores it (minisatip.cpp:1155-1158).
pub fn format_asctime(time: SystemTime) -> String {
    #[cfg(target_os = "linux")]
    if let Some(fields) = crate::sys::local_time_fields(time) {
        return format_asctime_fields(fields);
    }
    let seconds = unix_seconds(time);
    let days = seconds.div_euclid(86_400);
    let rest = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format_asctime_fields((
        (days + 4).rem_euclid(7) as i32,
        month as i32 - 1,
        day as i32,
        (rest / 3600) as i32,
        ((rest % 3600) / 60) as i32,
        (rest % 60) as i32,
        year,
    ))
}

/// `asctime` layout `%s %3d %02d:%02d:%02d %d ` with the trailing newline
/// replaced by a space.
fn format_asctime_fields(fields: (i32, i32, i32, i32, i32, i32, i64)) -> String {
    const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let (wday, month, day, hour, minute, second, year) = fields;
    let weekday = WEEKDAYS
        .get(wday.rem_euclid(7) as usize)
        .copied()
        .unwrap_or("");
    let month = MONTHS
        .get(month.rem_euclid(12) as usize)
        .copied()
        .unwrap_or("");
    format!("{weekday} {month}{day:3} {hour:02}:{minute:02}:{second:02} {year} ")
}

/// `time_running` (minisatip.cpp:1544-1553): `%.0d` prints nothing for 0 days.
pub fn format_running(start: SystemTime, now: SystemTime) -> String {
    let seconds = (unix_seconds(now) - unix_seconds(start)).max(0);
    let days = seconds / 86_400;
    let rest = seconds % 86_400;
    let (day_text, suffix) = if days > 0 {
        (days.to_string(), "d ")
    } else {
        (String::new(), "")
    };
    format!(
        "{day_text}{suffix}{:02}:{:02}:{:02}",
        rest / 3600,
        (rest % 3600) / 60,
        rest % 60
    )
}

fn unix_seconds(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_secs() as i64,
        Err(error) => -(error.duration().as_secs() as i64),
    }
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    if month <= 2 {
        year += 1;
    }
    (year, month as u32, day as u32)
}

/// One running PMT of an adapter (`ad->active_pmt`, pmt.cpp:2449-2486).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PmtState {
    /// `pmt->state == PMT_RUNNING`.
    pub running: bool,
    /// Channel name from the SDT (empty when unknown).
    pub name: String,
    /// PMT PID.
    pub pid: i32,
}

/// One adapter for the `ad_*` symbols: `present` is `a[i] != NULL`; the
/// `get_adapter_nw` getters also need `enabled`, else report C++ defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterState {
    /// Whether the adapter slot exists.
    pub present: bool,
    /// Whether the adapter is enabled.
    pub enabled: bool,
    /// Disabled through the configuration (`disabled[]`, any slot).
    pub disabled: bool,
    /// `adapter::type` (`ad_type`).
    pub kind: i32,
    /// Delivery systems supported by the adapter.
    pub systems: Vec<DeliverySystem>,
    /// diSEqC position (`ad_pos`).
    pub pos: i32,
    /// Frequency in kHz (`ad_freq` is MHz).
    pub freq: i32,
    /// `ad_pol` value used by the web UI.
    pub pol: i32,
    /// Symbol rate in sym/s (`ad_sr` is kSym/s).
    pub sr: u32,
    /// Bandwidth in Hz.
    pub bw: u32,
    /// Signal strength (0..65535).
    pub strength: u16,
    /// Signal-to-noise ratio (0..65535).
    pub snr: u16,
    /// SNR in dB.
    pub db: u16,
    /// Bit error rate (`uint32_t`, printed as `int`).
    pub ber: u32,
    /// PLP/ISI (`ad_stream`).
    pub stream: i32,
    /// Frontend number as `get_adapter_fe` reports it.
    pub fe: i32,
    /// `master_sid`; `ad_master` prints its low byte.
    pub master: i32,
    /// `sid_cnt`; `ad_sidcount` prints its low byte.
    pub sid_count: i32,
    /// Physical adapter number (`pa`).
    pub phy_adapter: i32,
    /// Physical frontend number (`fn`).
    pub phy_frontend: i32,
    /// Delivery system of the current tune (`fe_delivery_system_t`).
    pub sys: i32,
    /// Modulation.
    pub mtype: u32,
    /// DVB-T2 transponder id.
    pub t2id: i32,
    /// DVB-C2 tuner frequency.
    pub c2tft: i32,
    /// DVB-C2 data slice (`ad_plp`).
    pub plp: i32,
    /// PLS code.
    pub plsc: i32,
    /// Stream/input-stream id.
    pub sm: i32,
    /// Guard interval.
    pub gi: i32,
    /// Active and new PIDs in adapter slot order.
    pub pids: Vec<i32>,
    /// Continuity errors (`get_adapter_ccerrs`).
    pub ccerr: i32,
    /// Decoder errors.
    pub decerr: i32,
    /// Adapter name.
    pub name: String,
    /// Active PMTs in `ad->active_pmt` order.
    pub pmts: Vec<PmtState>,
}

impl Default for AdapterState {
    fn default() -> Self {
        Self {
            present: false,
            enabled: false,
            disabled: false,
            kind: 0,
            systems: Vec::new(),
            pos: 0,
            freq: 0,
            pol: 0,
            sr: 0,
            bw: 8_000_000,
            strength: 0,
            snr: 0,
            db: 0,
            ber: 0,
            stream: -1,
            fe: 0,
            master: 0,
            sid_count: 0,
            phy_adapter: 0,
            phy_frontend: 0,
            sys: 0,
            mtype: QAM_AUTO as u32,
            t2id: 0,
            c2tft: 0,
            plp: -1,
            plsc: -1,
            sm: 0,
            gi: GUARD_INTERVAL_AUTO,
            pids: Vec::new(),
            ccerr: 0,
            decerr: 0,
            name: String::new(),
            pmts: Vec::new(),
        }
    }
}

/// One stream as exposed by the `st_*` symbols; a default value is an
/// unallocated slot.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StreamState {
    /// Whether the stream is live.
    pub enabled: bool,
    /// Whether playback started.
    pub play: bool,
    /// Assigned adapter, or -1.
    pub adapter: i32,
    /// Client user agent.
    pub user_agent: String,
    /// Remote host.
    pub rhost: String,
    /// Remote port.
    pub rport: u16,
    /// Protocol label, already in C++ form (`RTP/UDP`, `SRT`, ...).
    pub protocol: String,
    /// PIDs of the adapter that carry this stream, in adapter slot order.
    pub pids: Vec<i32>,
    /// Socket overflow counter (-1 without a socket).
    pub overflow: i32,
    /// Bytes waiting in the write queue.
    pub buffered: i32,
}

/// One CA device (`ca_devices[i]`, ca.h:195-218).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CaDeviceState {
    /// `enabled`.
    pub enabled: i32,
    /// `state` (`CA_STATE_*`).
    pub state: i32,
    /// `caid[0..caids]`.
    pub caids: Vec<u16>,
    /// CAM name.
    pub ci_name: String,
}

/// Channel resolved for a DVBAPI key (`get_channel_for_key`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeyChannel {
    /// PMT name (empty without SDT).
    pub name: String,
    /// Service id.
    pub sid: i32,
}

/// One DVBAPI key slot (`keys[i]`, dvbapi.cpp:1122-1183).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeyState {
    /// `enabled`.
    pub enabled: i32,
    /// `hops`.
    pub hops: i32,
    /// `ecmtime`.
    pub ecmtime: i32,
    /// `pmt_pid`.
    pub pmt_pid: i32,
    /// `adapter`.
    pub adapter: i32,
    /// Card system name.
    pub cardsystem: String,
    /// Reader name.
    pub reader: String,
    /// Source.
    pub from: String,
    /// Protocol.
    pub protocol: String,
    /// Channel of the key's PMT when the key and PMT are enabled.
    pub channel: Option<KeyChannel>,
}

/// One SAT>IP client adapter (`satip[i]`, satipc.cpp:2315-2319).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SatipcState {
    /// Remote server address (`sip`).
    pub sip: String,
    /// `transport_type`.
    pub transport_type: i32,
}

/// `/bandwidth.json` counters (`c_*`, stream.cpp:1299-1309).
///
/// Same values as the folded [`crate::bandwidth::BandwidthSnapshot`] that the
/// data plane produces, so the two names refer to one type.
pub type BandwidthCounters = crate::bandwidth::BandwidthSnapshot;

/// Values captured for one HTTP request; missing slots are absent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WebSnapshot {
    /// Adapter slots.
    pub adapters: Vec<AdapterState>,
    /// Stream slots.
    pub streams: Vec<StreamState>,
    /// Values from `minisatip_sym`.
    pub runtime: RuntimeState,
    /// CA devices by adapter id.
    pub ca: Vec<Option<CaDeviceState>>,
    /// DVBAPI key slots.
    pub keys: Vec<Option<KeyState>>,
    /// SAT>IP client adapters by adapter id.
    pub satipc: Vec<Option<SatipcState>>,
    /// Bandwidth counters.
    pub bandwidth: BandwidthCounters,
}

impl WebSnapshot {
    /// Creates a snapshot with every slot empty.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adapter counts in `desc.xml` order.
    pub fn tuner_counts(&self) -> TunerCounts {
        TunerCounts::from_adapters(&self.adapters)
    }

    /// `a[i]`.
    fn adapter(&self, index: i32) -> Option<&AdapterState> {
        usize::try_from(index)
            .ok()
            .and_then(|index| self.adapters.get(index))
            .filter(|adapter| adapter.present)
    }

    /// `get_adapter_nw` (adapter.h:253-255).
    fn adapter_nw(&self, index: i32) -> Option<&AdapterState> {
        self.adapter(index).filter(|adapter| adapter.enabled)
    }

    /// `st[i]`; unallocated slots read as the default.
    fn stream(&self, index: i32) -> Option<&StreamState> {
        usize::try_from(index)
            .ok()
            .filter(|index| *index < MAX_STREAMS as usize)
            .and_then(|index| self.streams.get(index))
    }

    /// `get_sid_nw` (stream.h:108-113).
    fn stream_nw(&self, index: i32) -> Option<&StreamState> {
        self.stream(index).filter(|stream| stream.enabled)
    }
}

fn slot<T>(slots: &[Option<T>], index: i32) -> Option<&T> {
    usize::try_from(index)
        .ok()
        .and_then(|index| slots.get(index))
        .and_then(Option::as_ref)
}

/// Tuner counts reported by `desc.xml` and `state.json`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TunerCounts {
    /// Adapters supporting DVB-S or DVB-S2.
    pub s2: usize,
    /// DVB-T-only adapters.
    pub t: usize,
    /// DVB-C-only adapters.
    pub c: usize,
    /// DVB-T2 adapters.
    pub t2: usize,
    /// DVB-C2 adapters.
    pub c2: usize,
    /// ATSC terrestrial adapters (always zero in this port).
    pub at: usize,
    /// ATSC cable adapters (always zero in this port).
    pub ac: usize,
}

impl TunerCounts {
    /// Counts delivery systems like C++ `getAdaptersCount` (adapter.cpp:464).
    pub fn from_adapters(adapters: &[AdapterState]) -> Self {
        let mut counts = Self::default();
        for adapter in adapters.iter().filter(|adapter| adapter.present) {
            let has = |sys| adapter.systems.contains(&sys);
            if has(DeliverySystem::DvbS) || has(DeliverySystem::DvbS2) {
                counts.s2 += 1;
            }
            if has(DeliverySystem::DvbT) && !has(DeliverySystem::DvbT2) {
                counts.t += 1;
            }
            if has(DeliverySystem::DvbC) && !has(DeliverySystem::DvbC2) {
                counts.c += 1;
            }
            if has(DeliverySystem::DvbT2) {
                counts.t2 += 1;
            }
            if has(DeliverySystem::DvbC2) {
                counts.c2 += 1;
            }
        }
        counts
    }

    /// Builds the `X_SATIPCAP` string used in `desc.xml`.
    pub fn satip_cap(&self) -> String {
        let mut out = String::new();
        for (name, count) in [
            ("DVBS2", self.s2),
            ("DVBT", self.t),
            ("DVBC", self.c),
            ("DVBT2", self.t2),
            ("DVBC2", self.c2),
            ("ATSCT", self.at),
            ("ATSCC", self.ac),
        ] {
            if count > 0 {
                out.push_str(&format!("{name}-{count},"));
            }
        }
        if out.is_empty() {
            out.push_str("DVBS2-0,");
        }
        out.pop();
        out
    }
}

/// HTTP endpoints backed by the web UI files and generated state pages.
#[derive(Debug, Clone)]
pub struct WebService {
    config: WebConfig,
    identity: Option<SharedIdentity>,
}

impl WebService {
    /// Creates a service rooted at `config.document_root`.
    pub fn new(config: WebConfig) -> Self {
        Self {
            config,
            identity: None,
        }
    }

    /// Serves DEVICEID/BOOTID/UUID from `identity` instead of the static config,
    /// so SSDP conflict handling is visible to HTTP clients.
    pub fn with_identity(mut self, identity: SharedIdentity) -> Self {
        self.identity = Some(identity);
        self
    }

    /// Current configuration.
    pub fn config(&self) -> &WebConfig {
        &self.config
    }

    /// Effective identity: the shared one when installed, else the config.
    fn identity(&self) -> DeviceIdentity {
        self.identity
            .as_ref()
            .map(SharedIdentity::snapshot)
            .unwrap_or_else(|| DeviceIdentity {
                uuid: self.config.uuid.clone(),
                device_id: self.config.device_id,
                bootid: self.config.bootid,
            })
    }

    /// Routes an HTTP request; unhandled paths become 404 like C++ `read_http`.
    pub fn route(&self, request: &Request, state: &WebSnapshot) -> Option<HttpRoute> {
        let route = match request.uri.as_str() {
            DESC_XML_PATH => self.desc_route(state),
            STATE_JSON_PATH => json_route(self.state_json_bytes(state)),
            BANDWIDTH_JSON_PATH => json_route(self.bandwidth_json(state).into_bytes()),
            uri => self.static_route(uri, request.is_method("HEAD"), state),
        };
        Some(route)
    }

    /// Effective tuner counts: `-a` nonzero entries pin the DVBS2/DVBT/DVBC
    /// counts after `getAdaptersCount` counted the adapters
    /// (adapter.cpp:478-487,523-534).
    fn tuner_counts(&self, state: &WebSnapshot) -> TunerCounts {
        let mut counts = state.tuner_counts();
        let forced = |value: i32, count: &mut usize| {
            if value != 0 {
                *count = usize::try_from(value).unwrap_or(0);
            }
        };
        forced(self.config.force_sadapter, &mut counts.s2);
        forced(self.config.force_tadapter, &mut counts.t);
        forced(self.config.force_cadapter, &mut counts.c);
        counts
    }

    /// Builds the UPnP device description (minisatip.cpp:1468-1494, 1575-1609).
    pub fn desc_xml(&self, state: &WebSnapshot) -> String {
        let adapters = self.tuner_counts(state).satip_cap();
        let identity = self.identity();
        let playlist = self.config.playlist.as_deref().unwrap_or("");
        format!(
            concat!(
                "<?xml version=\"1.0\"?>",
                "<root xmlns=\"urn:schemas-upnp-org:device-1-0\" configId=\"1\">",
                "<specVersion><major>1</major><minor>1</minor></specVersion>",
                "<device><deviceType>urn:ses-com:device:SatIPServer:1</deviceType>",
                "<friendlyName>{}</friendlyName><manufacturer>cata</manufacturer>",
                "<manufacturerURL>http://github.com/catalinii/minisatip</",
                "manufacturerURL>",
                "<modelDescription>{} for ",
                "Linux</modelDescription><modelName>{}</modelName>",
                "<modelNumber>{}</modelNumber><modelURL></modelURL><serialNumber>1</",
                "serialNumber><UDN>uuid:{}</UDN>",
                "<iconList>",
                "<icon><mimetype>image/png</mimetype><width>48</width><height>48</",
                "height><depth>24</depth><url>/sm.png</url></icon>",
                "<icon><mimetype>image/png</mimetype><width>120</width><height>120</",
                "height><depth>24</depth><url>/lr.png</url></icon>",
                "<icon><mimetype>image/jpeg</mimetype><width>48</width><height>48</",
                "height><depth>24</depth><url>/sm.jpg</url></icon>",
                "<icon><mimetype>image/jpeg</mimetype><width>120</width><height>120</",
                "height><depth>24</depth><url>/lr.jpg</url></icon>",
                "</iconList>",
                "<presentationURL>http://{}/</presentationURL>\r\n",
                "<satip:X_SATIPCAP ",
                "xmlns:satip=\"urn:ses-com:satip\">{}</satip:X_SATIPCAP>",
                "{}",
                "</device></root>"
            ),
            self.config.name_app,
            "minisatip",
            self.config.name_app,
            self.config.version,
            identity.uuid,
            self.config.http_host,
            adapters,
            playlist
        )
    }

    /// Calls `f` with the `sym[]` list for `state` (symbols.cpp:27-41).
    pub fn with_symbols<R>(&self, state: &WebSnapshot, f: impl FnOnce(&SymbolTable<'_>) -> R) -> R {
        let identity = self.identity();
        let features = self.config.features;
        let minisatip = MinisatipSymbols {
            config: &self.config,
            identity: &identity,
            runtime: &state.runtime,
        };
        let adapters = AdapterSymbols {
            state,
            counts: self.tuner_counts(state),
        };
        let streams = StreamSymbols(state);
        let (ca, dvbapi) = (CaSymbols(state), DvbapiSymbols(state));
        let (satipc, pmt) = (SatipcSymbols(state), PmtSymbols(state));
        let mut providers: Vec<&dyn SymbolProvider> = vec![&adapters, &streams, &minisatip];
        if features.dvbca {
            providers.push(&ca);
        }
        if features.dvbapi {
            providers.push(&dvbapi);
        }
        if features.satipc {
            providers.push(&satipc);
        }
        if features.pmt {
            providers.push(&pmt);
        }
        let table: SymbolTable<'_> = providers
            .iter()
            .map(|provider| provider.symbols())
            .collect();
        f(&table)
    }

    /// `/state.json` bytes from `get_json_state` (variables.cpp:189-296).
    pub fn state_json_bytes(&self, state: &WebSnapshot) -> Vec<u8> {
        self.with_symbols(state, |table| table.json_state())
    }

    /// [`Self::state_json_bytes`] as text (lossy for non-UTF-8 values).
    pub fn state_json(&self, state: &WebSnapshot) -> String {
        String::from_utf8_lossy(&self.state_json_bytes(state)).into_owned()
    }

    /// `$var$` expansion of one variable name, as `var_eval` with `max_len`.
    pub fn var_eval(&self, state: &WebSnapshot, var: &str, max_len: usize) -> Vec<u8> {
        self.with_symbols(state, |table| table.var_eval(var.as_bytes(), max_len))
    }

    /// `/bandwidth.json` from `get_json_bandwidth` (variables.cpp:305-324).
    pub fn bandwidth_json(&self, state: &WebSnapshot) -> String {
        let bw = &state.bandwidth;
        format!(
            concat!(
                "{{\n",
                "\"bw\":{},\n",
                "\"dmx\":{},\n",
                "\"tbw\":{},\n",
                "\"reads\":{},\n",
                "\"writes\":{},\n",
                "\"fwrites\":{},\n",
                "\"ns_read\":{},\n",
                "\"tt\":{},\n",
                "\"buffered\":{},\n",
                "\"dropped\":{}\n",
                "}}"
            ),
            bw.bw,
            bw.dmx,
            bw.tbw,
            bw.reads,
            bw.writes,
            bw.fwrites,
            bw.ns_read,
            bw.tt,
            bw.buffered,
            bw.dropped
        )
    }

    fn desc_route(&self, state: &WebSnapshot) -> HttpRoute {
        // minisatip.cpp:1462,1601: the description is formatted into
        // `char buf[2000]` with snprintf, so it is truncated to 1999 bytes.
        let mut body = self.desc_xml(state).into_bytes();
        body.truncate(DESC_XML_MAX);
        HttpRoute::new(200, body)
            .with_headers(vec![
                "Cache-Control: no-cache".to_owned(),
                "Content-type: text/xml".to_owned(),
                format!("X-SATIP-RTSP-Port: {}", self.config.rtsp_port),
                "Connection: close".to_owned(),
            ])
            .head_body()
    }

    fn static_route(&self, uri: &str, is_head: bool, state: &WebSnapshot) -> HttpRoute {
        let target = if uri == "/" { "/status.html" } else { uri };
        // minisatip.cpp:1647-1657 accepts a name without a leading '/'.
        let relative = target.strip_prefix('/').unwrap_or(target);
        if relative.contains('/') || relative.contains("..") || relative.is_empty() {
            return HttpRoute::new(404, Vec::new());
        }
        let Ok(mut data) = fs::read(self.config.document_root.join(relative)) else {
            return HttpRoute::new(404, Vec::new());
        };
        let headers = content_type_headers(relative);
        if is_head {
            return HttpRoute::new(200, Vec::new()).with_headers(headers);
        }
        let templated = headers
            .iter()
            .any(|line| line.contains("html") || line.contains("xml"));
        if templated {
            data = self.with_symbols(state, |table| table.process_file(&data).0);
        }
        let route = HttpRoute::new(200, data).with_headers(headers);
        // process_file returns the response without Content-Length once
        // `io > sizeof(outp) - 100` triggers its first flush
        // (utils.cpp:379-419).
        if templated && route.body.len() > 8200 {
            route.streaming()
        } else {
            route
        }
    }
}

/// Raw content-type header block used by C++ `readfile` (utils.cpp:423).
pub fn content_type(fn_name: &str) -> &'static str {
    if fn_name.ends_with("png") {
        "Cache-Control: max-age=3600\r\nContent-type: image/png\r\nConnection: close"
    } else if fn_name.ends_with("ico") {
        "Cache-Control: max-age=3600\r\nContent-type: image/x-icon\r\nConnection: close"
    } else if fn_name.ends_with("jpg") || fn_name.ends_with("jpeg") {
        "Cache-Control: max-age=3600\r\nContent-type: image/jpeg\r\nConnection: close"
    } else if fn_name.ends_with("css") {
        "Cache-Control: max-age=3600\r\nContent-type: text/css\r\nConnection: close"
    } else if fn_name.ends_with("js") {
        "Cache-Control: max-age=3600\r\nContent-type: text/javascript\r\nConnection: close"
    } else if fn_name.ends_with("htm") || fn_name.ends_with("html") {
        "Cache-Control: max-age=3600\r\nContent-type: text/html; charset=utf-8\r\nConnection: close"
    } else if fn_name.ends_with("xml") {
        "Cache-Control: no-cache\r\nContent-type: text/xml"
    } else if fn_name.ends_with("json") {
        "Cache-Control: no-cache\r\nContent-type: application/json; charset=utf-8"
    } else if fn_name.ends_with("m3u") {
        "Cache-Control: no-cache\r\nContent-type: video/x-mpegurl"
    } else {
        "Cache-Control: no-cache\r\nContent-type: application/octet-stream"
    }
}

fn content_type_headers(fn_name: &str) -> Vec<String> {
    content_type(fn_name)
        .split("\r\n")
        .map(str::to_owned)
        .collect()
}

fn json_route(body: Vec<u8>) -> HttpRoute {
    HttpRoute::new(200, body)
        .with_headers(vec![
            "Content-Type: application/json; charset=utf-8".to_owned(),
            "Connection: close".to_owned(),
            "Access-Control-Allow-Origin: *".to_owned(),
        ])
        .head_body()
}

trait HeadBody {
    fn head_body(self) -> Self;
}

impl HeadBody for HttpRoute {
    fn head_body(mut self) -> Self {
        self.body_on_head = true;
        self
    }
}

/// `1. / 1000` stored in the `float` multiplier (adapter.cpp:2417-2427).
fn per_thousand() -> f32 {
    (1.0_f64 / 1000.0) as f32
}

fn int(value: impl Into<i64>) -> Raw {
    Raw::Int(value.into())
}

/// `strlcatf` the items, then drop the trailing separator like the getters.
fn joined(items: impl IntoIterator<Item = Vec<u8>>, max_size: usize, trim: usize) -> Raw {
    let mut dest = CBuf::new(max_size);
    for item in items {
        dest.catf(&item);
    }
    let len = dest.len();
    if len >= trim && len > 0 {
        for index in len - trim..len {
            dest.set(index, 0);
        }
    }
    Raw::Str(dest.as_cstr().to_vec())
}

/// `adapters_sym` (adapter.cpp:2412-2477).
struct AdapterSymbols<'a> {
    state: &'a WebSnapshot,
    counts: TunerCounts,
}

impl SymbolProvider for AdapterSymbols<'_> {
    fn symbols(&self) -> Vec<Symbol<'_>> {
        let s = self.state;
        let counts = self.counts;
        let n = MAX_ADAPTERS as i32;
        let field = move |name, ty, get: fn(&AdapterState) -> i64| {
            Symbol::new(name, ty, 1.0, n, move |i, _| {
                Raw::Int(s.adapter(i).map_or(0, get))
            })
        };
        let tp = move |name, multiplier, default: i64, get: fn(&AdapterState) -> i64| {
            Symbol::new(name, VAR_FUNCTION_INT, multiplier, n, move |i, _| {
                Raw::Int(s.adapter_nw(i).map_or(default, get))
            })
        };
        let count = |name, value: usize| Symbol::scalar(name, VAR_INT, 1.0, int(value as i64));
        vec![
            field("ad_enabled", VAR_AARRAY_INT8, |a| i64::from(a.enabled)),
            field("ad_type", VAR_AARRAY_INT8, |a| i64::from(a.kind)),
            tp("ad_pos", 0.0, 0, |a| i64::from(a.pos)),
            tp("ad_freq", per_thousand(), 0, |a| i64::from(a.freq)),
            field("ad_strength", VAR_AARRAY_UINT16, |a| i64::from(a.strength)),
            field("ad_snr", VAR_AARRAY_UINT16, |a| i64::from(a.snr)),
            field("ad_db", VAR_AARRAY_UINT16, |a| i64::from(a.db)),
            field("ad_ber", VAR_AARRAY_INT, |a| i64::from(a.ber)),
            tp("ad_pol", 0.0, 0, |a| i64::from(a.pol)),
            tp("ad_sr", per_thousand(), 0, |a| i64::from(a.sr)),
            tp("ad_bw", per_thousand(), 8_000_000, |a| i64::from(a.bw)),
            tp("ad_stream", 0.0, -1, |a| i64::from(a.stream)),
            tp("ad_fe", 0.0, 0, |a| i64::from(a.fe)),
            field("ad_master", VAR_AARRAY_UINT8, |a| i64::from(a.master)),
            field("ad_sidcount", VAR_AARRAY_UINT8, |a| i64::from(a.sid_count)),
            field("ad_phyad", VAR_AARRAY_INT, |a| i64::from(a.phy_adapter)),
            field("ad_phyfd", VAR_AARRAY_INT, |a| i64::from(a.phy_frontend)),
            tp("ad_sys", 0.0, 0, |a| i64::from(a.sys)),
            tp("ad_mtype", 0.0, QAM_AUTO.into(), |a| i64::from(a.mtype)),
            tp("ad_t2id", 0.0, 0, |a| i64::from(a.t2id)),
            tp("ad_c2tft", 0.0, 0, |a| i64::from(a.c2tft)),
            tp("ad_plp", 0.0, -1, |a| i64::from(a.plp)),
            tp("ad_plsc", 0.0, -1, |a| i64::from(a.plsc)),
            tp("ad_sm", 0.0, 0, |a| i64::from(a.sm)),
            tp("ad_gi", 0.0, GUARD_INTERVAL_AUTO.into(), |a| {
                i64::from(a.gi)
            }),
            Symbol::new("ad_allsys", VAR_FUNCTION_STRING, 0.0, n, move |i, max| {
                all_delsys(s.adapter_nw(i), max)
            }),
            Symbol::new("ad_pids", VAR_FUNCTION_STRING, 0.0, n, move |i, max| {
                adapter_pids(s.adapter_nw(i), max)
            }),
            tp("ad_ccerr", 0.0, 0, |a| i64::from(a.ccerr)),
            tp("ad_decerr", 0.0, 0, |a| i64::from(a.decerr)),
            count("tuner_s2", counts.s2),
            count("tuner_t2", counts.t2),
            count("tuner_c2", counts.c2),
            count("tuner_t", counts.t),
            count("tuner_c", counts.c),
            count("tuner_ac", counts.ac),
            count("tuner_at", counts.at),
            Symbol::new("ad_disabled", VAR_ARRAY_INT8, 1.0, n, move |i, _| {
                let disabled = usize::try_from(i)
                    .ok()
                    .and_then(|i| s.adapters.get(i))
                    .is_some_and(|a| a.disabled);
                int(disabled)
            }),
            Symbol::new(
                "ad_adapter_names",
                VAR_FUNCTION_STRING,
                0.0,
                n,
                move |i, _| {
                    // get_configured_adapter_nw (adapter.cpp:1433-1437).
                    let name = s
                        .adapter(i)
                        .filter(|a| !a.disabled)
                        .map(|a| a.name.as_str());
                    Raw::text(name.unwrap_or(""))
                },
            ),
        ]
    }
}

/// `get_all_delsys` (adapter.cpp:2321-2338).
fn all_delsys(adapter: Option<&AdapterState>, max_size: usize) -> Raw {
    let Some(adapter) = adapter else {
        return Raw::text("");
    };
    let names = adapter.systems.iter().filter_map(|sys| match sys {
        DeliverySystem::DvbS => Some("dvbs"),
        DeliverySystem::DvbS2 => Some("dvbs2"),
        DeliverySystem::DvbT => Some("dvbt"),
        DeliverySystem::DvbT2 => Some("dvbt2"),
        DeliverySystem::DvbC => Some("dvbc"),
        DeliverySystem::DvbC2 => Some("dvbc2"),
        DeliverySystem::Undefined | DeliverySystem::Other(_) => None,
    });
    joined(
        names.map(|name| format!("{name},").into_bytes()),
        max_size,
        1,
    )
}

/// `get_adapter_pids` (adapter.cpp:2294-2319).
fn adapter_pids(adapter: Option<&AdapterState>, max_size: usize) -> Raw {
    let Some(adapter) = adapter else {
        return Raw::text("");
    };
    let mut items = Vec::new();
    for pid in &adapter.pids {
        if *pid == 8192 {
            items.push(b"all,".to_vec());
            break;
        }
        items.push(format!("{pid},").into_bytes());
    }
    if items.is_empty() {
        return joined([b"none".to_vec()], max_size, 0);
    }
    joined(items, max_size, 1)
}

/// `stream_sym` (stream.cpp:1589-1610).
struct StreamSymbols<'a>(&'a WebSnapshot);

impl SymbolProvider for StreamSymbols<'_> {
    fn symbols(&self) -> Vec<Symbol<'_>> {
        let s = self.0;
        let n = MAX_STREAMS;
        let slot_int = move |name, get: fn(&StreamState) -> i64| {
            Symbol::new(name, VAR_FUNCTION_INT, 0.0, n, move |i, _| {
                Raw::Int(s.stream(i).map_or(0, get))
            })
        };
        let live_int = move |name, get: fn(&StreamState) -> i64| {
            Symbol::new(name, VAR_FUNCTION_INT, 0.0, n, move |i, _| {
                Raw::Int(s.stream_nw(i).map_or(0, get))
            })
        };
        vec![
            slot_int("st_enabled", |st| i64::from(st.enabled)),
            slot_int("st_play", |st| i64::from(st.play)),
            slot_int("st_adapter", |st| i64::from(st.adapter)),
            Symbol::new(
                "st_useragent",
                VAR_FUNCTION_STRING,
                0.0,
                n,
                move |i, max| {
                    let agent = s.stream(i).map_or(&[][..], |st| st.user_agent.as_bytes());
                    Raw::Str(strncpy_c(agent, max.saturating_sub(1)))
                },
            ),
            Symbol::new("st_rhost", VAR_FUNCTION_STRING, 0.0, n, move |i, _| {
                Raw::text(s.stream_nw(i).map_or("", |st| st.rhost.as_str()))
            }),
            live_int("st_rport", |st| i64::from(st.rport)),
            Symbol::new("st_proto", VAR_FUNCTION_STRING, 0.0, n, move |i, max| {
                let proto = s.stream_nw(i).map(|st| st.protocol.as_bytes().to_vec());
                joined(proto, max, 0)
            }),
            Symbol::new("st_pids", VAR_FUNCTION_STRING, 0.0, n, move |i, max| {
                stream_pids(s, i, max)
            }),
            live_int("st_overflow", |st| i64::from(st.overflow)),
            live_int("st_buffered", |st| i64::from(st.buffered)),
        ]
    }
}

/// `get_stream_pids` (stream.cpp:1508-1537): no "none", and "all" repeats.
fn stream_pids(s: &WebSnapshot, index: i32, max_size: usize) -> Raw {
    let Some(stream) = s.stream_nw(index) else {
        return Raw::text("");
    };
    if s.adapter_nw(stream.adapter).is_none() {
        return Raw::text("");
    }
    let items = stream.pids.iter().map(|pid| {
        if *pid == 8192 {
            b"all,".to_vec()
        } else {
            format!("{pid},").into_bytes()
        }
    });
    joined(items, max_size, 1)
}

/// `minisatip_sym` (minisatip.cpp:2186-2203).
struct MinisatipSymbols<'a> {
    config: &'a WebConfig,
    identity: &'a DeviceIdentity,
    runtime: &'a RuntimeState,
}

impl SymbolProvider for MinisatipSymbols<'_> {
    fn symbols(&self) -> Vec<Symbol<'_>> {
        let text = |name, ty, multiplier, value: &str| {
            Symbol::scalar(name, ty, multiplier, Raw::text(value))
        };
        let number = |name, value: i32| Symbol::scalar(name, VAR_INT, 1.0, int(value));
        let config = self.config;
        let runtime = self.runtime;
        vec![
            number("has_pmt", i32::from(config.features.pmt)),
            text("name_app", VAR_PSTRING, 0.0, &config.name_app),
            text("http_host", VAR_PSTRING, 0.0, &config.http_host),
            text("uuid", VAR_STRING, 0.0, &self.identity.uuid),
            number("bootid", self.identity.bootid),
            number("deviceid", self.identity.device_id),
            number("http_port", config.http_port),
            text("rtsp_host", VAR_PSTRING, 0.0, &config.rtsp_host),
            text(
                "datetime_compile",
                VAR_PSTRING,
                0.0,
                &runtime.datetime_compile,
            ),
            text("datetime_start", VAR_PSTRING, 0.0, &runtime.datetime_start),
            text(
                "datetime_current",
                VAR_PSTRING,
                0.0,
                &runtime.datetime_current,
            ),
            text("time_running", VAR_PSTRING, 0.0, &runtime.time_running),
            number("run_user", runtime.run_user),
            number("run_pid", runtime.run_pid),
            text("version", VAR_STRING, 1.0, &config.version),
            text("command_line", VAR_PSTRING, 1.0, &runtime.command_line),
        ]
    }
}

/// `ca_sym` (ca.cpp:4234-4242).
struct CaSymbols<'a>(&'a WebSnapshot);

impl SymbolProvider for CaSymbols<'_> {
    fn symbols(&self) -> Vec<Symbol<'_>> {
        let s = self.0;
        let n = MAX_ADAPTERS as i32;
        vec![
            Symbol::new("ca_enabled", VAR_AARRAY_INT8, 1.0, n, move |i, _| {
                int(slot(&s.ca, i).map_or(0, |ca| ca.enabled))
            }),
            Symbol::new("ca_state", VAR_AARRAY_INT8, 1.0, n, move |i, _| {
                int(slot(&s.ca, i).map_or(0, |ca| ca.state))
            }),
            Symbol::new("ca_caids", VAR_FUNCTION_STRING, 0.0, n, move |i, max| {
                // get_ca_caids_string (ca.cpp:4210-4232) allocates a missing
                // device, which has no CAIDs.
                let caids = slot(&s.ca, i).map_or(&[][..], |ca| ca.caids.as_slice());
                let items = caids
                    .iter()
                    .map(|caid| format!("{caid:04X}, ").into_bytes());
                joined(items, max, 2)
            }),
            Symbol::new("ca_ci_name", VAR_AARRAY_STRING, 1.0, n, move |i, _| {
                Raw::text(slot(&s.ca, i).map_or("", |ca| ca.ci_name.as_str()))
            }),
        ]
    }
}

/// `dvbapi_sym` (dvbapi.cpp:1185-1201).
struct DvbapiSymbols<'a>(&'a WebSnapshot);

impl SymbolProvider for DvbapiSymbols<'_> {
    fn symbols(&self) -> Vec<Symbol<'_>> {
        let s = self.0;
        let n = MAX_KEYS;
        let key = move |i: i32| slot(&s.keys, i).filter(|_| i < MAX_KEYS);
        let number = move |name, get: fn(&KeyState) -> i32| {
            Symbol::new(name, VAR_FUNCTION_INT, 0.0, n, move |i, _| {
                int(key(i).map_or(0, get))
            })
        };
        let text = move |name, get: fn(&KeyState) -> &str| {
            Symbol::new(name, VAR_FUNCTION_STRING, 0.0, n, move |i, max: usize| {
                let value = key(i).map_or("", get);
                Raw::Str(strncpy_c(value.as_bytes(), max.saturating_sub(1)))
            })
        };
        vec![
            number("key_enabled", |k| k.enabled),
            number("key_hops", |k| k.hops),
            number("key_ecmtime", |k| k.ecmtime),
            number("key_pmt", |k| k.pmt_pid),
            number("key_adapter", |k| k.adapter),
            text("key_cardsystem", |k| &k.cardsystem),
            text("key_reader", |k| &k.reader),
            text("key_from", |k| &k.from),
            text("key_protocol", |k| &k.protocol),
            Symbol::new("key_channel", VAR_FUNCTION_STRING, 0.0, n, move |i, max| {
                key_channel(key(i), max)
            }),
        ]
    }
}

/// `get_channel_for_key` (dvbapi.cpp:1079-1111).
fn key_channel(key: Option<&KeyState>, max_size: usize) -> Raw {
    let Some(channel) = key
        .filter(|key| key.enabled != 0)
        .and_then(|key| key.channel.as_ref())
    else {
        return Raw::text("");
    };
    if channel.name.is_empty() {
        let mut dest = CBuf::new(max_size);
        dest.catf(format!("SID {}", channel.sid).as_bytes());
        Raw::Str(dest.as_cstr().to_vec())
    } else {
        Raw::Str(strncpy_c(
            channel.name.as_bytes(),
            max_size.saturating_sub(1),
        ))
    }
}

/// `satipc_sym` (satipc.cpp:2315-2319).
struct SatipcSymbols<'a>(&'a WebSnapshot);

impl SymbolProvider for SatipcSymbols<'_> {
    fn symbols(&self) -> Vec<Symbol<'_>> {
        let s = self.0;
        let n = MAX_ADAPTERS as i32;
        vec![
            Symbol::new("ad_satip", VAR_AARRAY_STRING, 1.0, n, move |i, _| {
                Raw::text(slot(&s.satipc, i).map_or("", |sip| sip.sip.as_str()))
            }),
            Symbol::new(
                "ad_satip_transport_type",
                VAR_AARRAY_INT,
                1.0,
                n,
                move |i, _| int(slot(&s.satipc, i).map_or(0, |sip| sip.transport_type)),
            ),
        ]
    }
}

/// `pmt_sym` (pmt.cpp:2539-2543); `ad_pmt` spans `MAX_STREAMS` slots.
struct PmtSymbols<'a>(&'a WebSnapshot);

impl SymbolProvider for PmtSymbols<'_> {
    fn symbols(&self) -> Vec<Symbol<'_>> {
        let s = self.0;
        vec![
            Symbol::new(
                "ad_channel",
                VAR_FUNCTION_STRING,
                0.0,
                MAX_ADAPTERS as i32,
                move |i, max| {
                    let pmts = s.adapter_nw(i).map_or(&[][..], |a| a.pmts.as_slice());
                    let names = pmts
                        .iter()
                        .filter(|pmt| pmt.running && !pmt.name.is_empty())
                        .map(|pmt| format!("{},", pmt.name).into_bytes());
                    joined(names, max, 1)
                },
            ),
            Symbol::new(
                "ad_pmt",
                VAR_FUNCTION_STRING,
                0.0,
                MAX_STREAMS,
                move |i, max| {
                    let pmts = s.adapter_nw(i).map_or(&[][..], |a| a.pmts.as_slice());
                    let pids = pmts
                        .iter()
                        .filter(|pmt| pmt.running)
                        .map(|pmt| format!("{},", pmt.pid).into_bytes());
                    joined(pids, max, 1)
                },
            ),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asctime_layout_matches_cpp() {
        // asctime's "%.3s %.3s%3d %.2d:%.2d:%.2d %d\n" with a trailing space.
        assert_eq!(
            format_asctime_fields((3, 9, 21, 10, 20, 19, 2026)),
            "Wed Oct 21 10:20:19 2026 "
        );
        assert_eq!(
            format_asctime_fields((0, 0, 1, 0, 0, 5, 1970)),
            "Sun Jan  1 00:00:05 1970 "
        );
    }
}
