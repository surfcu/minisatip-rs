//! Web UI and generated HTTP endpoints, mirroring the C++ `read_http`.
//!
//! Static files are served from `document_root`; HTML/XML files get the same
//! `$var$` substitution as C++ `process_file` (utils.cpp:379).

use std::fs;
use std::path::PathBuf;

use ms_sched::MAX_ADAPTERS;
use ms_tuner::DeliverySystem;

use crate::bandwidth::BandwidthSnapshot;
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
        }
    }
}

/// Runtime values shown in the status page.
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
}

/// One adapter as exposed by the `ad_*` symbols.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterState {
    /// Whether the adapter slot exists (`ad_type`).
    pub present: bool,
    /// Whether the adapter is enabled.
    pub enabled: bool,
    /// Disabled through the configuration.
    pub disabled: bool,
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
    /// Bit error rate.
    pub ber: i32,
    /// PLP/ISI or stream id.
    pub stream: i32,
    /// Frontend number.
    pub fe: i32,
    /// Stream owning the adapter, or -1.
    pub master: i32,
    /// Number of streams using the adapter.
    pub sid_count: i32,
    /// Delivery system of the current tune (`fe_delivery_system_t`).
    pub sys: i32,
    /// Modulation.
    pub mtype: u32,
    /// DVB-T2 transponder id.
    pub t2id: i32,
    /// DVB-C2 tuner frequency.
    pub c2tft: i32,
    /// PLP/ISI.
    pub plp: i32,
    /// PLS code.
    pub plsc: i32,
    /// Stream/input-stream id.
    pub sm: i32,
    /// Guard interval.
    pub gi: i32,
    /// Active PIDs (`-1` is never used; empty means none).
    pub pids: Vec<i32>,
    /// Consecutive counter errors.
    pub ccerr: i32,
    /// Decoder errors.
    pub decerr: i32,
    /// Adapter name.
    pub name: String,
}

impl Default for AdapterState {
    fn default() -> Self {
        Self {
            present: false,
            enabled: false,
            disabled: false,
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
            sys: 0,
            // adapter.cpp:2399-2410: QAM_AUTO and GUARD_INTERVAL_AUTO.
            mtype: 6,
            t2id: 0,
            c2tft: 0,
            plp: -1,
            plsc: -1,
            sm: 0,
            gi: 4,
            pids: Vec::new(),
            ccerr: 0,
            decerr: 0,
            name: String::new(),
        }
    }
}

/// One stream as exposed by the `st_*` symbols.
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
    /// PIDs selected by this stream.
    pub pids: Vec<i32>,
    /// Socket overflow counter.
    pub overflow: i32,
    /// Bytes waiting in the write queue.
    pub buffered: i32,
}

/// Adapter, stream and runtime values captured for one HTTP request.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WebSnapshot {
    /// Adapter slots; missing trailing slots are padded with defaults.
    pub adapters: Vec<AdapterState>,
    /// Stream slots; missing trailing slots are padded with defaults.
    pub streams: Vec<StreamState>,
    /// Values from `minisatip_sym`.
    pub runtime: RuntimeState,
    /// Folded counters printed by `/bandwidth.json` (`c_*`).
    pub bandwidth: BandwidthSnapshot,
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
            STATE_JSON_PATH => json_route(self.state_json(state)),
            BANDWIDTH_JSON_PATH => json_route(self.bandwidth_json(&state.bandwidth)),
            uri => self.static_route(uri, request.is_method("HEAD"), state),
        };
        Some(route)
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

    /// Builds `/state.json` in C++ symbol order (`adapters_sym`, `stream_sym`,
    /// `minisatip_sym`).
    pub fn state_json(&self, state: &WebSnapshot) -> String {
        let identity = self.identity();
        let none_adapter = AdapterState::default();
        let none_stream = StreamState::default();
        let adapters = |index: usize| {
            state
                .adapters
                .get(index)
                .filter(|adapter| adapter.present)
                .unwrap_or(&none_adapter)
        };
        let streams = |index: usize| streams_or_default(state, index, &none_stream);

        let mut json = Json::new();
        json.int_array(
            "ad_enabled",
            (0..MAX_ADAPTERS).map(|i| i32::from(adapters(i).enabled)),
        );
        json.int_array(
            "ad_type",
            (0..MAX_ADAPTERS).map(|i| i32::from(adapters(i).present)),
        );
        json.int_array("ad_pos", (0..MAX_ADAPTERS).map(|i| adapters(i).pos));
        json.int_array(
            "ad_freq",
            (0..MAX_ADAPTERS).map(|i| symbol_scaled_1000(adapters(i).freq)),
        );
        json.int_array(
            "ad_strength",
            (0..MAX_ADAPTERS).map(|i| i32::from(adapters(i).strength)),
        );
        json.int_array(
            "ad_snr",
            (0..MAX_ADAPTERS).map(|i| i32::from(adapters(i).snr)),
        );
        json.int_array(
            "ad_db",
            (0..MAX_ADAPTERS).map(|i| i32::from(adapters(i).db)),
        );
        json.int_array("ad_ber", (0..MAX_ADAPTERS).map(|i| adapters(i).ber));
        json.int_array("ad_pol", (0..MAX_ADAPTERS).map(|i| adapters(i).pol));
        json.int_array(
            "ad_sr",
            (0..MAX_ADAPTERS).map(|i| symbol_scaled_1000_u(adapters(i).sr)),
        );
        json.int_array(
            "ad_bw",
            (0..MAX_ADAPTERS).map(|i| symbol_scaled_1000_u(adapters(i).bw)),
        );
        json.int_array("ad_stream", (0..MAX_ADAPTERS).map(|i| adapters(i).stream));
        json.int_array("ad_fe", (0..MAX_ADAPTERS).map(|i| adapters(i).fe));
        json.int_array("ad_master", (0..MAX_ADAPTERS).map(|i| adapters(i).master));
        json.int_array(
            "ad_sidcount",
            (0..MAX_ADAPTERS).map(|i| adapters(i).sid_count),
        );
        json.int_array("ad_phyad", (0..MAX_ADAPTERS).map(|_| 0));
        json.int_array("ad_phyfd", (0..MAX_ADAPTERS).map(|_| 0));
        json.int_array("ad_sys", (0..MAX_ADAPTERS).map(|i| adapters(i).sys));
        json.int_array(
            "ad_mtype",
            (0..MAX_ADAPTERS).map(|i| adapters(i).mtype as i32),
        );
        json.int_array("ad_t2id", (0..MAX_ADAPTERS).map(|i| adapters(i).t2id));
        json.int_array("ad_c2tft", (0..MAX_ADAPTERS).map(|i| adapters(i).c2tft));
        json.int_array("ad_plp", (0..MAX_ADAPTERS).map(|i| adapters(i).plp));
        json.int_array("ad_plsc", (0..MAX_ADAPTERS).map(|i| adapters(i).plsc));
        json.int_array("ad_sm", (0..MAX_ADAPTERS).map(|i| adapters(i).sm));
        json.int_array("ad_gi", (0..MAX_ADAPTERS).map(|i| adapters(i).gi));
        json.string_array("ad_allsys", (0..MAX_ADAPTERS).map(|i| allsys(adapters(i))));
        json.string_array(
            "ad_pids",
            (0..MAX_ADAPTERS).map(|i| pid_string(&adapters(i).pids, adapters(i).present)),
        );
        json.int_array("ad_ccerr", (0..MAX_ADAPTERS).map(|i| adapters(i).ccerr));
        json.int_array("ad_decerr", (0..MAX_ADAPTERS).map(|i| adapters(i).decerr));

        let counts = self.tuner_counts(state);
        json.int("tuner_s2", counts.s2 as i32);
        json.int("tuner_t2", counts.t2 as i32);
        json.int("tuner_c2", counts.c2 as i32);
        json.int("tuner_t", counts.t as i32);
        json.int("tuner_c", counts.c as i32);
        json.int("tuner_ac", counts.ac as i32);
        json.int("tuner_at", counts.at as i32);
        json.int_array(
            "ad_disabled",
            (0..MAX_ADAPTERS).map(|i| i32::from(adapters(i).disabled)),
        );
        json.string_array(
            "ad_adapter_names",
            (0..MAX_ADAPTERS).map(|i| adapters(i).name.clone()),
        );

        json.int_array(
            "st_enabled",
            (0..MAX_STREAMS as usize).map(|i| i32::from(streams(i).enabled)),
        );
        json.int_array(
            "st_play",
            (0..MAX_STREAMS as usize).map(|i| i32::from(streams(i).play)),
        );
        json.int_array(
            "st_adapter",
            (0..MAX_STREAMS as usize).map(|i| streams(i).adapter),
        );
        json.string_array(
            "st_useragent",
            (0..MAX_STREAMS as usize).map(|i| streams(i).user_agent.clone()),
        );
        json.string_array(
            "st_rhost",
            (0..MAX_STREAMS as usize).map(|i| streams(i).rhost.clone()),
        );
        json.int_array(
            "st_rport",
            (0..MAX_STREAMS as usize).map(|i| i32::from(streams(i).rport)),
        );
        json.string_array(
            "st_proto",
            (0..MAX_STREAMS as usize).map(|i| streams(i).protocol.clone()),
        );
        json.string_array(
            "st_pids",
            (0..MAX_STREAMS as usize).map(|i| pid_string(&streams(i).pids, streams(i).enabled)),
        );
        json.int_array(
            "st_overflow",
            (0..MAX_STREAMS as usize).map(|i| streams(i).overflow),
        );
        json.int_array(
            "st_buffered",
            (0..MAX_STREAMS as usize).map(|i| streams(i).buffered),
        );

        // minisatip.cpp:2181: a normal (PMT-enabled) build exports has_pmt=1.
        json.int("has_pmt", 1);
        json.string("name_app", &self.config.name_app);
        json.string("http_host", &self.config.http_host);
        json.string("uuid", &identity.uuid);
        json.int("bootid", identity.bootid);
        json.int("deviceid", identity.device_id);
        json.int("http_port", self.config.http_port);
        json.string("rtsp_host", &self.config.rtsp_host);
        json.string("datetime_compile", &state.runtime.datetime_compile);
        json.string("datetime_start", &state.runtime.datetime_start);
        json.string("datetime_current", &state.runtime.datetime_current);
        json.string("time_running", &state.runtime.time_running);
        json.int("run_user", state.runtime.run_user);
        json.int("run_pid", state.runtime.run_pid);
        json.string("version", &self.config.version);
        json.string("command_line", &state.runtime.command_line);
        json.finish()
    }

    /// Builds `/bandwidth.json` from the folded counters
    /// (`get_json_bandwidth`, api/variables.cpp:305-324).
    pub fn bandwidth_json(&self, counters: &BandwidthSnapshot) -> String {
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
            counters.bw,
            counters.dmx,
            counters.tbw,
            counters.reads,
            counters.writes,
            counters.fwrites,
            counters.ns_read,
            counters.tt,
            counters.buffered,
            counters.dropped
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
        let templated = headers
            .iter()
            .any(|line| line.contains("html") || line.contains("xml"));
        if templated {
            let counts = self.tuner_counts(state);
            data = substitute(&data, |name| self.template_value(name, counts));
        }
        if is_head {
            HttpRoute::new(200, Vec::new()).with_headers(headers)
        } else {
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

    /// Resolves a template variable by the C++ prefix-match rule
    /// (variables.cpp:113). Generated state values are supplied by
    /// [`Self::state_json`]; templates only use service-wide values.
    fn template_value(&self, name: &str, counts: TunerCounts) -> Option<String> {
        let identity = self.identity();
        let values = [
            ("version", self.config.version.clone()),
            ("uuid", identity.uuid.clone()),
            ("http_host", self.config.http_host.clone()),
            ("rtsp_host", self.config.rtsp_host.clone()),
            ("name_app", self.config.name_app.clone()),
            ("http_port", self.config.http_port.to_string()),
            ("bootid", identity.bootid.to_string()),
            ("deviceid", identity.device_id.to_string()),
            ("tuner_s2", counts.s2.to_string()),
            ("tuner_t2", counts.t2.to_string()),
            ("tuner_c2", counts.c2.to_string()),
            ("tuner_t", counts.t.to_string()),
            ("tuner_c", counts.c.to_string()),
            ("tuner_ac", counts.ac.to_string()),
            ("tuner_at", counts.at.to_string()),
        ];
        values
            .iter()
            .find(|(symbol, _)| name.starts_with(symbol))
            .map(|(_, value)| value.clone())
    }
}

/// Applies `$var$` replacement; unknown variables expand to nothing (utils.cpp:379).
pub fn substitute(data: &[u8], lookup: impl Fn(&str) -> Option<String>) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut index = 0;
    while index < data.len() {
        if data[index] == b'$'
            && let Some(close) =
                (index + 1..data.len().min(index + 21)).find(|position| data[*position] == b'$')
            && let Ok(name) = std::str::from_utf8(&data[index + 1..close])
        {
            if let Some(value) = lookup(name) {
                out.extend_from_slice(value.as_bytes());
            }
            index = close + 1;
            continue;
        }
        out.push(data[index]);
        index += 1;
    }
    out
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

fn json_route(body: String) -> HttpRoute {
    HttpRoute::new(200, body.into_bytes())
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

/// Reproduces the C++ symbol table's `value * (1.0f / 1000.0f)` cast to `int`.
///
/// `snprintf_pointer` multiplies the integer getter result by a `float`
/// multiplier before truncating, which can round to the next MHz/kSym/s for
/// values close to a boundary.
fn symbol_scaled_1000(value: i32) -> i32 {
    ((value as f32) * (1.0_f32 / 1000.0_f32)) as i32
}

/// [`symbol_scaled_1000`] for raw `u32` symbol values.
fn symbol_scaled_1000_u(value: u32) -> i32 {
    ((value as f32) * (1.0_f32 / 1000.0_f32)) as i32
}

fn streams_or_default<'a>(
    state: &'a WebSnapshot,
    index: usize,
    none: &'a StreamState,
) -> &'a StreamState {
    state.streams.get(index).unwrap_or(none)
}

fn allsys(adapter: &AdapterState) -> String {
    adapter
        .systems
        .iter()
        .filter_map(|sys| match sys {
            DeliverySystem::DvbS => Some("dvbs"),
            DeliverySystem::DvbS2 => Some("dvbs2"),
            DeliverySystem::DvbT => Some("dvbt"),
            DeliverySystem::DvbT2 => Some("dvbt2"),
            DeliverySystem::DvbC => Some("dvbc"),
            DeliverySystem::DvbC2 => Some("dvbc2"),
            DeliverySystem::Undefined | DeliverySystem::Other(_) => None,
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn pid_string(pids: &[i32], present: bool) -> String {
    if !present {
        return String::new();
    }
    if pids.contains(&8192) {
        return "all".to_owned();
    }
    if pids.is_empty() {
        return "none".to_owned();
    }
    pids.iter()
        .map(i32::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

struct Json {
    out: String,
    first: bool,
}

impl Json {
    fn new() -> Self {
        Self {
            out: "{\n".to_owned(),
            first: true,
        }
    }

    fn key(&mut self, name: &str) {
        if !self.first {
            self.out.push_str(",\n");
        }
        self.first = false;
        self.out.push('"');
        self.out.push_str(name);
        self.out.push_str("\":");
    }

    fn int(&mut self, name: &str, value: i32) {
        self.key(name);
        self.out.push_str(&value.to_string());
    }

    fn string(&mut self, name: &str, value: &str) {
        self.key(name);
        self.out.push_str(&json_string(value));
    }

    fn int_array(&mut self, name: &str, values: impl IntoIterator<Item = i32>) {
        self.key(name);
        self.out.push('[');
        for (index, value) in values.into_iter().enumerate() {
            if index > 0 {
                self.out.push(',');
            }
            self.out.push_str(&value.to_string());
        }
        self.out.push(']');
    }

    fn string_array<S: AsRef<str>>(&mut self, name: &str, values: impl IntoIterator<Item = S>) {
        self.key(name);
        self.out.push('[');
        for (index, value) in values.into_iter().enumerate() {
            if index > 0 {
                self.out.push(',');
            }
            self.out.push_str(&json_string(value.as_ref()));
        }
        self.out.push(']');
    }

    fn finish(mut self) -> String {
        self.out.push_str("\n}\n");
        self.out
    }
}

/// Escapes a string like C++ `escape_json_string` (variables.cpp:171): control
/// characters become `%xNN`; quotes and backslashes are left untouched.
pub fn json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        if ch >= ' ' {
            out.push(ch);
        } else {
            out.push_str(&format!("%x{:02X}", ch as u32));
        }
    }
    out.push('"');
    out
}
