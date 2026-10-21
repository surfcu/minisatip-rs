//! Stream table and RTSP session manager implementing [`StreamBackend`].
//!
//! The manager owns all mutable state; adapter decisions are delegated to
//! [`ms_sched::Scheduler`].

use ms_sched::{Scheduler, SetParamsError, StreamId, TuneError};
use ms_tuner::{DeliverySystem, Polarization, TuneParams};

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
            remote_port: server_port,
            server_port,
            rtcp_server_port: server_port.wrapping_add(1),
            rtp_seq: 0,
            timeout_ms: 30_000,
            user_agent: String::new(),
            touched: false,
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
    pub fn describe_streams(&mut self, current: Option<StreamId>, url: &str) -> Option<String> {
        let selected = if let Some(position) = url.find("stream=") {
            let number = parse_int_prefix(&url[position + "stream=".len()..], 0) - 1;
            Some(self.slot_index(number)?)
        } else {
            current
                .filter(|id| self.slot_index(*id).is_some())
                .and_then(|id| self.slot_index(id))
        };
        let ids: Vec<StreamId> = match selected {
            Some(index) => vec![index as StreamId],
            None => (0..STREAM_SLOTS)
                .filter(|index| {
                    self.streams[*index]
                        .as_ref()
                        .is_some_and(|stream| stream.enabled)
                })
                .map(|index| index as StreamId)
                .collect(),
        };
        if ids.is_empty() {
            return None;
        }

        let (s2, t, c) = self.tuner_counts();
        let first = self.stream(ids[0]).expect("live stream");
        let mut sdp = format!(
            "v=0\r\no=- {:010} {:010} IN IP4 0.0.0.0\r\ns=SatIPServer:1 {},{},{}\r\nt=0 0\r\n",
            first.session_id, first.session_id, s2, t, c
        );
        for id in ids {
            let Some(stream) = self.stream(id) else {
                continue;
            };
            let state = if stream.do_play {
                "sendonly"
            } else {
                "inactive"
            };
            sdp.push_str(&format!(
                "m=video {} RTP/AVP 33\r\nc=IN IP4 0.0.0.0\r\na=control:stream={}\r\n",
                stream.remote_port,
                id + 1
            ));
            sdp.push_str(&format!(
                "a=fmtp:33 ver=1.0;src={};tuner={},255,1,15,{},{};pids={}\r\n",
                stream.params.src,
                stream.adapter.max(0) + i32::from(stream.adapter >= 0),
                stream.params.freq,
                short_parameter(&stream.params),
                pids_string(&stream.params),
            ));
            sdp.push_str(&format!("a={state}\r\n"));
        }
        Some(sdp)
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

fn pids_string(params: &TuneParams) -> String {
    if params.pids.is_all() {
        "all".to_owned()
    } else if params.pids.is_empty() {
        "none".to_owned()
    } else {
        params
            .pids
            .iter()
            .map(|pid| pid.to_string())
            .collect::<Vec<_>>()
            .join(",")
    }
}

fn short_parameter(params: &TuneParams) -> String {
    let pol = match params.pol {
        Polarization::Vertical => "v",
        Polarization::Horizontal => "h",
        Polarization::Right => "r",
        Polarization::Left => "l",
        Polarization::None => "h",
    };
    let msys = match params.msys {
        DeliverySystem::DvbS => "dvbs",
        DeliverySystem::DvbS2 => "dvbs2",
        DeliverySystem::DvbT => "dvbt",
        DeliverySystem::DvbT2 => "dvbt2",
        DeliverySystem::DvbC => "dvbc",
        DeliverySystem::DvbC2 => "dvbc2",
        DeliverySystem::Undefined | DeliverySystem::Other(_) => "undefined",
    };
    format!("{pol},{msys},{},{}", params.sr, params.fec)
}
