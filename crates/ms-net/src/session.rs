//! Stream table and RTSP session manager implementing [`StreamBackend`].
//!
//! The manager owns all mutable state; adapter decisions are delegated to
//! [`ms_sched::Scheduler`].

use ms_sched::{Scheduler, SetParamsError, StreamId, TuneError};
use ms_tuner::{DeliverySystem, PID_ALL, Polarization, TuneParams};

use crate::handler::{
    HttpResource, MAX_STREAMS, PlayError, StreamBackend, StreamInfo, StreamType, TransportError,
};
use crate::rtsp::{Protocol, TransportKind, parse_int_prefix, parse_transport};
use crate::satip::{merge_satip_url, parse_satip_url_new};

const STREAM_SLOTS: usize = MAX_STREAMS as usize;

/// One stream slot, mirroring the C++ `streams` structure.
#[derive(Debug, Clone)]
pub struct Stream {
    /// Slot index (`sid`).
    pub id: StreamId,
    /// Whether the slot is live.
    pub enabled: bool,
    /// Socket handle that created the stream.
    pub sock: i32,
    /// Assigned adapter, or `-1`.
    pub adapter: i32,
    /// Output transport.
    pub stream_type: StreamType,
    /// Whether playback has started.
    pub do_play: bool,
    /// Current tuning and PID parameters.
    pub params: TuneParams,
    /// Pending error text used as a 403/503 response body.
    pub rtsp_error: String,
    /// RTP session id (`ssrc`).
    pub session_id: i64,
    /// Client or multicast destination host.
    pub remote_host: String,
    /// Client or multicast destination RTP port.
    pub remote_port: u16,
    /// Local RTP server port.
    pub server_port: u16,
    /// Local RTCP server port.
    pub rtcp_server_port: u16,
    /// RTP sequence number.
    pub rtp_seq: u16,
    /// Idle timeout in milliseconds; zero disables it.
    pub timeout_ms: u32,
    /// Client user agent.
    pub user_agent: String,
    /// Whether the stream was touched since creation.
    pub touched: bool,
}

impl Stream {
    fn new(id: StreamId, sock: i32, session_id: i64, start_rtp: u16) -> Self {
        let server_port = start_rtp.wrapping_add((2 * id) as u16);
        Self {
            id,
            enabled: true,
            sock,
            adapter: -1,
            stream_type: StreamType::None,
            do_play: false,
            params: TuneParams::default(),
            rtsp_error: String::new(),
            session_id,
            remote_host: "0.0.0.0".to_owned(),
            remote_port: 0,
            server_port,
            rtcp_server_port: server_port.wrapping_add(1),
            rtp_seq: 0,
            timeout_ms: 30_000,
            user_agent: String::new(),
            touched: false,
        }
    }
}

/// Deterministic inputs for the SDP header.
#[derive(Debug, Clone)]
pub struct DescribeOptions {
    /// Local address printed in `o=` and used to pick `IP4` or `IP6`.
    pub local_host: String,
    /// Overrides the `o=` session id (default: the socket's stream id, else 0).
    pub session_id: Option<i64>,
    /// Overrides the `o=` session version (default: the session id).
    pub session_version: Option<i64>,
    /// Value printed in `t=`, e.g. `0 0`.
    pub time: String,
}

impl Default for DescribeOptions {
    fn default() -> Self {
        Self {
            local_host: "127.0.0.1".to_owned(),
            session_id: None,
            session_version: None,
            time: "0 0".to_owned(),
        }
    }
}

/// Stream table plus adapter scheduler.
pub struct SessionManager {
    streams: Vec<Option<Stream>>,
    sched: Scheduler,
    sock: i32,
    next_session_id: i64,
    start_rtp: u16,
    describe: DescribeOptions,
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionManager {
    /// Creates an empty manager with default ports.
    pub fn new() -> Self {
        Self::with_scheduler(Scheduler::new())
    }

    /// Creates an empty manager around an existing scheduler.
    pub fn with_scheduler(sched: Scheduler) -> Self {
        let mut streams = Vec::with_capacity(STREAM_SLOTS);
        streams.resize_with(STREAM_SLOTS, || None);
        Self {
            streams,
            sched,
            sock: -1,
            next_session_id: 1000,
            start_rtp: 5500,
            describe: DescribeOptions::default(),
        }
    }

    /// Scheduler used for adapter allocation.
    pub fn scheduler(&self) -> &Scheduler {
        &self.sched
    }

    /// Mutable scheduler.
    pub fn scheduler_mut(&mut self) -> &mut Scheduler {
        &mut self.sched
    }

    /// Sets the socket handle copied into streams created afterwards.
    pub fn set_socket(&mut self, sock: i32) {
        self.sock = sock;
    }

    /// Sets the first RTP server port.
    pub fn set_start_rtp(&mut self, start_rtp: u16) {
        self.start_rtp = start_rtp;
    }

    /// Replaces the SDP header inputs.
    pub fn set_describe_options(&mut self, options: DescribeOptions) {
        self.describe = options;
    }

    /// Mutable SDP header inputs.
    pub fn describe_options_mut(&mut self) -> &mut DescribeOptions {
        &mut self.describe
    }

    /// Live stream with `id`, or `None` when disabled.
    pub fn stream(&self, id: StreamId) -> Option<&Stream> {
        self.slot_index(id)
            .and_then(|index| self.streams[index].as_ref())
    }

    fn slot_index(&self, id: StreamId) -> Option<usize> {
        usize::try_from(id)
            .ok()
            .filter(|index| *index < STREAM_SLOTS)
            .filter(|index| {
                self.streams[*index]
                    .as_ref()
                    .is_some_and(|stream| stream.enabled)
            })
    }

    /// Allocates an empty or disabled slot, like the C++ `streams_add`.
    pub fn add_stream(&mut self) -> Option<StreamId> {
        let index = (0..STREAM_SLOTS).find(|index| {
            self.streams[*index]
                .as_ref()
                .is_none_or(|stream| !stream.enabled)
        })?;
        let id = index as StreamId;
        let session_id = self.next_session_id;
        self.next_session_id += 1;
        self.streams[index] = Some(Stream::new(id, self.sock, session_id, self.start_rtp));
        Some(id)
    }

    /// Whether every stream slot is live.
    pub fn streams_full(&self) -> bool {
        self.streams
            .iter()
            .all(|slot| slot.as_ref().is_some_and(|stream| stream.enabled))
    }

    /// Creates or updates a stream, applying SETUP adapter-release semantics.
    pub fn setup_stream_request(
        &mut self,
        url: &str,
        current: Option<StreamId>,
        method: &str,
    ) -> Option<StreamId> {
        let existing = current.filter(|id| self.slot_index(*id).is_some());
        let id = match existing {
            Some(id) => id,
            None => self.add_stream()?,
        };
        let index = self.slot_index(id)?;
        let params = match existing {
            Some(_) => {
                let mut params = self.streams[index].as_ref()?.params.clone();
                merge_satip_url(url, &mut params);
                params
            }
            None => parse_satip_url_new(url).0,
        };
        {
            let stream = self.streams[index].as_mut()?;
            stream.params = params;
            stream.do_play = false;
            stream.rtsp_error.clear();
        }
        let adapter = self.streams[index].as_ref()?.adapter;
        if adapter >= 0 && method.eq_ignore_ascii_case("SETUP") {
            let (same_mux, adds_pids) = {
                let stream = self.streams[index].as_ref()?;
                (
                    !self
                        .sched
                        .compare_tuning_parameters(adapter, &stream.params)
                        && self.sched.fe_pin_matches_adapter(adapter, &stream.params),
                    url.contains("addpids") || url.contains("delpids"),
                )
            };
            if !adds_pids && !same_mux {
                self.sched.close_adapter_for_stream(id, adapter);
                if let Some(stream) = self.streams[index].as_mut() {
                    stream.adapter = -1;
                }
            }
        }
        Some(id)
    }

    /// Starts playback, allocating or reusing an adapter as the C++ does.
    pub fn start_play(&mut self, id: StreamId, protocol: Protocol) -> Result<(), PlayError> {
        let Some(index) = self.slot_index(id) else {
            return Err(PlayError::new(454, None));
        };
        {
            let stream = self.streams[index].as_mut().expect("checked slot");
            stream.rtsp_error.clear();
            if stream.stream_type == StreamType::None && protocol == Protocol::Http {
                stream.stream_type = StreamType::Http;
            }
            if stream.stream_type == StreamType::None {
                return Err(PlayError::new(454, None));
            }
        }

        let mut adapter = self.streams[index].as_ref().expect("checked slot").adapter;
        let params = self.streams[index]
            .as_ref()
            .expect("checked slot")
            .params
            .clone();
        if self.sched.compare_tuning_parameters(adapter, &params) {
            if adapter >= 0 && !self.sched.compare_slave_parameters(adapter, &params) {
                self.sched.close_adapter_for_stream(id, adapter);
                if let Some(stream) = self.streams[index].as_mut() {
                    stream.adapter = -1;
                }
            }
            adapter = self.streams[index].as_ref().expect("checked slot").adapter;
        }

        let needs_adapter = adapter < 0
            || (self.sched.compare_tuning_parameters(adapter, &params)
                && self.sched.adapter(adapter as usize).map(|ad| ad.master_sid) != Some(id));
        if needs_adapter {
            if params.msys == DeliverySystem::Undefined && params.freq == 0 {
                if let Some(stream) = self.streams[index].as_mut() {
                    stream.do_play = true;
                }
                return Ok(());
            }
            if adapter >= 0 {
                self.sched.close_adapter_for_stream(id, adapter);
                if let Some(stream) = self.streams[index].as_mut() {
                    stream.adapter = -1;
                }
            }
            let Some(aid) = self.sched.get_free_adapter(&params) else {
                return Err(self.allocation_error(index, &params));
            };
            if let Some(stream) = self.streams[index].as_mut() {
                stream.adapter = aid as i32;
            }
            self.sched.set_adapter_for_stream(id, aid as i32);
            adapter = aid as i32;
        }

        if let Err(error) = self.sched.set_adapter_parameters(adapter, id, &params) {
            let (status, body) = match error {
                SetParamsError::SlaveConflict => (503, "No-More: frontends"),
                SetParamsError::MissingAdapter | SetParamsError::PidFailure => {
                    (503, "No-More: pids")
                }
            };
            self.set_play_error(index, body);
            return Err(PlayError::new(status, body.to_owned()));
        }
        if let Some(stream) = self.streams[index].as_mut() {
            stream.do_play = true;
        }

        let outcome = self.sched.tune(adapter, id);
        for close in &outcome.close_streams {
            self.disable_stream(*close);
        }
        match outcome.result {
            Ok(()) => Ok(()),
            Err(TuneError::OutOfRange) => {
                self.set_play_error(index, "Out-of-Range: freq");
                Err(PlayError::new(403, "Out-of-Range: freq".to_owned()))
            }
            Err(TuneError::Pids) => {
                self.set_play_error(index, "No-More: pids");
                Err(PlayError::new(503, "No-More: pids".to_owned()))
            }
            Err(TuneError::Hardware) => Err(PlayError::new(500, None)),
        }
    }

    fn allocation_error(&mut self, index: usize, params: &TuneParams) -> PlayError {
        let Some(attr) = self.sched.out_of_range_attrs(params) else {
            self.set_play_error(index, "No-More: frontends");
            return PlayError::new(503, "No-More: frontends".to_owned());
        };
        let body = format!("Out-of-Range: {attr}");
        self.set_play_error(index, &body);
        PlayError::new(403, body)
    }

    fn set_play_error(&mut self, index: usize, text: &str) {
        if let Some(stream) = self.streams[index].as_mut() {
            stream.rtsp_error = text.to_owned();
        }
    }

    /// Disables a stream whose adapter was already released by the scheduler.
    fn disable_stream(&mut self, id: StreamId) {
        if let Some(index) = self.slot_index(id)
            && let Some(stream) = self.streams[index].as_mut()
        {
            stream.enabled = false;
            stream.adapter = -1;
            stream.do_play = false;
        }
    }

    /// Closes a stream and releases its adapter.
    pub fn close_stream(&mut self, id: StreamId) {
        let Some(index) = self.slot_index(id) else {
            return;
        };
        let adapter = self.streams[index].as_ref().expect("checked slot").adapter;
        if let Some(stream) = self.streams[index].as_mut() {
            stream.enabled = false;
            stream.adapter = -1;
            stream.do_play = false;
        }
        if adapter >= 0 {
            self.sched.close_adapter_for_stream(id, adapter);
        }
    }

    /// Applies a `Transport` header.
    pub fn decode_transport(&mut self, id: StreamId, value: &str) -> Result<(), TransportError> {
        let Some(index) = self.slot_index(id) else {
            return Err(TransportError);
        };
        let spec = parse_transport(value);
        let stream = self.streams[index].as_mut().expect("checked slot");
        match spec.kind {
            TransportKind::Srt => {
                if spec.srt_streamid.is_none() {
                    return Err(TransportError);
                }
                stream.stream_type = StreamType::RtspSrt;
            }
            TransportKind::Tcp => stream.stream_type = StreamType::RtspTcp,
            TransportKind::Udp => stream.stream_type = StreamType::RtspUdp,
        }
        if let Some(port) = spec.client_port.or(spec.port) {
            stream.remote_port = port;
        }
        if let Some(destination) = spec.destination {
            stream.remote_host = destination;
        }
        Ok(())
    }

    /// Stream details in handler form.
    pub fn stream_info(&self, id: StreamId) -> Option<StreamInfo> {
        self.stream(id).map(|stream| StreamInfo {
            stream_type: stream.stream_type,
            session_id: stream.session_id,
            stream_number: id + 1,
            remote_host: stream.remote_host.clone(),
            remote_port: stream.remote_port,
            server_port: stream.server_port,
            rtcp_server_port: stream.rtcp_server_port,
            rtp_seq: stream.rtp_seq,
            timeout_ms: Some(stream.timeout_ms),
        })
    }

    /// Builds SDP for the selected stream, or every live stream when unselected.
    ///
    /// Mirrors `describe_streams` in `stream.cpp:82`: a query string selects the
    /// socket stream; `stream=` always selects and fails with `None` if unknown.
    pub fn describe_streams(&mut self, current: Option<StreamId>, url: &str) -> Option<String> {
        let mut do_all = !url.contains('?');
        let mut selected = current.filter(|id| self.slot_index(*id).is_some());
        if let Some(position) = url.find("stream=") {
            do_all = false;
            let number = parse_int_prefix(&url[position + "stream=".len()..], 0) - 1;
            selected = self.slot_index(number).map(|index| index as StreamId);
            selected?;
        }

        // stream.cpp:92: the header uses the socket's stream, not `stream=`.
        let header_session = self.describe.session_id.unwrap_or_else(|| {
            current
                .and_then(|id| self.stream(id))
                .map_or(0, |stream| stream.session_id)
        });
        let header_version = self.describe.session_version.unwrap_or(header_session);
        let (s2, t, c) = self.tuner_counts();
        let mut sdp = format!(
            "v=0\r\no=- {:010} {:010} IN {} {}\r\ns=SatIPServer:1 {},{},{}\r\nt={}\r\n",
            header_session,
            header_version,
            if self.describe.local_host.contains(':') {
                "IP6"
            } else {
                "IP4"
            },
            self.describe.local_host,
            s2,
            t,
            c,
            self.describe.time
        );

        let mut streams_enabled = 0;
        if do_all {
            for index in 0..STREAM_SLOTS {
                let Some(stream) = self.streams[index].as_ref().filter(|stream| stream.enabled)
                else {
                    continue;
                };
                let id = index as StreamId;
                let state = if stream.do_play {
                    "sendonly"
                } else {
                    "inactive"
                };
                let port = stream.remote_port;
                let fmtp = self.describe_adapter(id);
                sdp.push_str(&format!(
                    "m=video {port} RTP/AVP 33\r\nc=IN IP4 0.0.0.0\r\na=control:stream={}\r\na=fmtp:33 {fmtp}\r\na={state}\r\n",
                    id + 1
                ));
                streams_enabled += 1;
            }
        }

        // stream.cpp:134: no stream was listed, describe the selected one.
        if streams_enabled == 0 {
            let id = selected?;
            let stream = self.stream(id)?;
            let state = if stream.do_play {
                "sendonly"
            } else {
                "inactive"
            };
            let fmtp = self.describe_adapter(id);
            sdp.push_str(&format!(
                "m=video 0 RTP/AVP 33\r\nc=IN IP4 0.0.0.0\r\na=control:stream={}\r\na=fmtp:33 {fmtp}\r\nb=AS:5000\r\na={state}\r\n",
                id + 1
            ));
        }
        Some(sdp)
    }

    /// One `a=fmtp:33` value, mirroring `describe_adapter` (`adapter.cpp:1448`).
    fn describe_adapter(&self, id: StreamId) -> String {
        let Some(stream) = self.stream(id) else {
            return String::new();
        };
        let mut aid = stream.adapter;
        let adapter = usize::try_from(aid)
            .ok()
            .and_then(|index| self.sched.adapter(index))
            .filter(|adapter| adapter.config.enabled);
        let use_ad = adapter.is_some() && stream.do_play;
        if !use_ad && aid < 0 {
            aid = 0;
        }
        let params = match adapter {
            Some(adapter) if use_ad => &adapter.params,
            _ => &stream.params,
        };
        // No signal source yet: print the C++ values for a tuner without status.
        let (strength, status, snr) = (255, 1, 15);
        let tuner = adapter
            .and_then(|adapter| adapter.params.fe)
            .filter(|fe| *fe > 0)
            .unwrap_or(aid + 1);

        let body = match params.msys {
            // adapter.cpp:1488-1490: the undefined branch always prints `aid + 1`.
            DeliverySystem::Undefined => {
                format!("ver=1.0;src=1;tuner={},0,0,0,0,,,,,,,;pids=", aid + 1)
            }
            DeliverySystem::DvbS | DeliverySystem::DvbS2 => format!(
                "ver=1.0;src={};tuner={tuner},{strength},{status},{snr},{},{},{},{},{},{},{},{};pids=",
                i32::from(params.src),
                params.freq,
                polarization_name(params.pol),
                delivery_system_name(params.msys),
                modulation_name(params),
                pilot_name(params.plts),
                rolloff_name(params.ro),
                params.sr,
                fec_name(params),
            ),
            DeliverySystem::DvbT | DeliverySystem::DvbT2 => format!(
                "ver=1.1;tuner={tuner},{strength},{status},{snr},{:.2},{},{},{},{},{},{},{},{},{};pids=",
                f64::from(params.freq),
                params.bw.unwrap_or(8_000_000) / 1_000_000,
                delivery_system_name(params.msys),
                transmission_mode_name(params.tmode),
                modulation_name(params),
                guard_interval_name(params.gi),
                fec_name(params),
                positive_or_empty(params.plp),
                params.t2id.unwrap_or(0),
                params.sm.unwrap_or(0),
            ),
            _ => format!(
                "ver=1.2;tuner={tuner},{strength},{status},{snr},{:.2},{},{},{},{},{},{},{},{};pids=",
                f64::from(params.freq),
                params.bw.unwrap_or(8_000_000) / 1_000_000,
                delivery_system_name(params.msys),
                modulation_name(params),
                params.sr,
                params.c2tft.unwrap_or(0),
                positive_or_empty(params.ds),
                positive_or_empty(params.plp),
                inversion_name(params.specinv),
            ),
        };
        format!("{body}{}", self.stream_pids_text(id, aid, params, use_ad))
    }

    fn stream_pids_text(
        &self,
        id: StreamId,
        aid: i32,
        params: &TuneParams,
        use_ad: bool,
    ) -> String {
        if use_ad {
            let pids = self.sched.stream_pids(aid, id);
            if pids.is_empty() {
                return "none".to_owned();
            }
            return pids
                .iter()
                .map(|pid| {
                    if *pid == i32::from(PID_ALL) {
                        "all".to_owned()
                    } else {
                        pid.to_string()
                    }
                })
                .collect::<Vec<_>>()
                .join(",");
        }
        if params.pids.is_all() {
            return "all".to_owned();
        }
        if params.pids.is_empty() {
            return "none".to_owned();
        }
        params
            .pids
            .iter()
            .map(|pid| {
                if pid == PID_ALL {
                    "all".to_owned()
                } else {
                    pid.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn tuner_counts(&self) -> (usize, usize, usize) {
        let mut s2 = 0;
        let mut t = 0;
        let mut c = 0;
        for aid in 0..ms_sched::MAX_ADAPTERS {
            let Some(adapter) = self.sched.adapter(aid) else {
                continue;
            };
            if !adapter.config.enabled {
                continue;
            }
            let sys = &adapter.config.sys;
            if sys.contains(&DeliverySystem::DvbS) || sys.contains(&DeliverySystem::DvbS2) {
                s2 += 1;
            }
            if sys.contains(&DeliverySystem::DvbT) || sys.contains(&DeliverySystem::DvbT2) {
                t += 1;
            }
            if sys.contains(&DeliverySystem::DvbC) || sys.contains(&DeliverySystem::DvbC2) {
                c += 1;
            }
        }
        (s2, t, c)
    }
}

impl StreamBackend for SessionManager {
    fn find_session(&mut self, session_id: i64) -> Option<StreamId> {
        (0..STREAM_SLOTS).find_map(|index| {
            self.streams[index]
                .as_ref()
                .filter(|stream| stream.enabled && stream.session_id == session_id)
                .map(|stream| stream.id)
        })
    }

    fn get_stream(&self, id: StreamId) -> Option<StreamInfo> {
        self.stream_info(id)
    }

    fn streams_full(&self) -> bool {
        SessionManager::streams_full(self)
    }

    fn setup_stream(&mut self, url: &str, current: Option<StreamId>) -> Option<StreamId> {
        SessionManager::setup_stream_request(self, url, current, "")
    }

    fn setup_stream_for_request(
        &mut self,
        url: &str,
        current: Option<StreamId>,
        method: &str,
    ) -> Option<StreamId> {
        SessionManager::setup_stream_request(self, url, current, method)
    }

    fn set_session_id(&mut self, id: StreamId, session_id: i64) {
        if let Some(index) = self.slot_index(id)
            && let Some(stream) = self.streams[index].as_mut()
        {
            stream.session_id = session_id;
        }
    }

    fn touch_stream(&mut self, id: StreamId) {
        if let Some(index) = self.slot_index(id)
            && let Some(stream) = self.streams[index].as_mut()
        {
            stream.touched = true;
        }
    }

    fn set_user_agent(&mut self, id: StreamId, agent: &str) {
        if let Some(index) = self.slot_index(id)
            && let Some(stream) = self.streams[index].as_mut()
        {
            stream.user_agent = agent.to_owned();
        }
    }

    fn disable_timeout(&mut self, id: StreamId) {
        if let Some(index) = self.slot_index(id)
            && let Some(stream) = self.streams[index].as_mut()
        {
            stream.timeout_ms = 0;
        }
    }

    fn start_play(&mut self, id: StreamId, protocol: Protocol) -> Result<(), PlayError> {
        SessionManager::start_play(self, id, protocol)
    }

    fn decode_transport(&mut self, id: StreamId, value: &str) -> Result<(), TransportError> {
        SessionManager::decode_transport(self, id, value)
    }

    fn close_stream(&mut self, id: StreamId) {
        SessionManager::close_stream(self, id);
    }

    fn describe_streams(&mut self, current: Option<StreamId>, url: &str) -> Option<String> {
        SessionManager::describe_streams(self, current, url)
    }

    fn http_resource(&mut self, _uri: &str) -> Option<HttpResource> {
        None
    }
}

fn polarization_name(pol: Polarization) -> &'static str {
    match pol {
        Polarization::Vertical => "v",
        Polarization::Horizontal => "h",
        Polarization::Right => "r",
        Polarization::Left => "l",
        Polarization::None => "",
    }
}

fn delivery_system_name(sys: DeliverySystem) -> &'static str {
    match sys {
        DeliverySystem::DvbS => "dvbs",
        DeliverySystem::DvbS2 => "dvbs2",
        DeliverySystem::DvbT => "dvbt",
        DeliverySystem::DvbT2 => "dvbt2",
        DeliverySystem::DvbC => "dvbc",
        DeliverySystem::DvbC2 => "dvbc2",
        DeliverySystem::Undefined => "",
        DeliverySystem::Other(value) => match value {
            2 => "dvbcb",
            4 => "dss",
            7 => "dvbh",
            8 => "isdbt",
            9 => "isdbs",
            10 => "isdbc",
            11 => "atsc",
            12 => "atscmh",
            13 => "dmbth",
            14 => "cmmb",
            15 => "dab",
            17 => "turbo",
            18 => "dvbcc",
            _ => "",
        },
    }
}

fn modulation_name(params: &TuneParams) -> &'static str {
    if !params.mtype_specified {
        return "auto";
    }
    match params.mtype {
        0 => "qpsk",
        1 => "16qam",
        2 => "32qam",
        3 => "64qam",
        4 => "128qam",
        5 => "256qam",
        6 => "auto",
        7 => "8vsb",
        8 => "16vsb",
        9 => "8psk",
        10 => "16apsk",
        11 => "32apsk",
        12 => "dqpsk",
        13 => "qam_4_nr",
        14 => "64apsk",
        15 => "128apsk",
        16 => "256apsk",
        _ => "",
    }
}

fn pilot_name(pilot: Option<bool>) -> &'static str {
    match pilot {
        Some(true) => "on",
        Some(false) => "off",
        None => "auto",
    }
}

fn rolloff_name(rolloff: Option<u8>) -> &'static str {
    match rolloff {
        Some(0) => "0.35",
        Some(1) => "0.20",
        Some(2) => "0.25",
        Some(4) => "0.15",
        Some(5) => "0.10",
        Some(6) => "0.05",
        // ROLLOFF_AUTO, and the value_or default when unset.
        Some(3) | None => "auto",
        Some(_) => "",
    }
}

fn fec_name(params: &TuneParams) -> &'static str {
    if !params.fec_specified {
        return "auto";
    }
    match params.fec {
        0 => "none",
        1 => "12",
        2 => "23",
        3 => "34",
        4 => "45",
        5 => "56",
        6 => "67",
        7 => "78",
        8 => "89",
        9 => "auto",
        10 => "35",
        11 => "910",
        12 => "25",
        13 => "14",
        14 => "13",
        _ => "",
    }
}

fn transmission_mode_name(mode: Option<i32>) -> &'static str {
    match mode {
        Some(0) => "2k",
        Some(1) => "8k",
        Some(3) => "4k",
        Some(4) => "1k",
        Some(5) => "16k",
        Some(6) => "32k",
        Some(7) => "c1",
        Some(8) => "c3780",
        // TRANSMISSION_MODE_AUTO, and the value_or default when unset.
        Some(2) | None => "auto",
        Some(_) => "",
    }
}

fn guard_interval_name(guard: Option<i32>) -> &'static str {
    match guard {
        Some(0) => "132",
        Some(1) => "116",
        Some(2) => "18",
        Some(3) => "14",
        Some(5) => "1128",
        Some(6) => "19128",
        Some(7) => "19256",
        Some(8) => "pn420",
        Some(9) => "pn595",
        Some(10) => "pn945",
        // GUARD_INTERVAL_AUTO, and the value_or default when unset.
        Some(4) | None => "auto",
        Some(_) => "",
    }
}

fn inversion_name(inversion: Option<i32>) -> &'static str {
    match inversion {
        Some(0) => "off",
        Some(1) => "on",
        // INVERSION_AUTO, and the value_or default when unset.
        Some(2) | None => "auto",
        Some(_) => "",
    }
}

fn positive_or_empty(value: Option<i32>) -> String {
    value
        .filter(|value| *value >= 0)
        .map_or_else(String::new, |value| value.to_string())
}
