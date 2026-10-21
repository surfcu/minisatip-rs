//! Stream table and RTSP session manager implementing [`StreamBackend`].
//!
//! The manager owns all mutable state; adapter decisions are delegated to
//! [`ms_sched::Scheduler`].

use ms_sched::{MAX_ADAPTERS, Scheduler, SetParamsError, StreamId, TuneError};
use ms_tuner::{DeliverySystem, PID_ALL, Polarization, TuneParams};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Instant, SystemTime};

use crate::handler::{
    HttpResource, HttpRoute, MAX_STREAMS, PlayError, StreamBackend, StreamInfo, StreamType,
    TransportError,
};
use crate::rtsp::{Protocol, Request, TransportKind, parse_int_prefix, parse_transport};
use crate::satip::{merge_satip_url, parse_satip_url_new};
#[cfg(feature = "srt")]
use crate::srt::{LibSrtOps, SRT_INVALID_SOCK, SrtListener};
use crate::web::{
    ADAPTER_DVB, AdapterState, GUARD_INTERVAL_AUTO, QAM_AUTO, RuntimeState, StreamState,
    WebService, WebSnapshot,
};

const STREAM_SLOTS: usize = MAX_STREAMS as usize;
/// Extra time a client has to respond before the stream is closed
/// (`stream_timeout`, stream.cpp:1366).
pub const TIMEOUT_GRACE_MS: i64 = 10_000;
/// Timeout value meaning "close now" (`sid->timeout == 1`).
pub const TIMEOUT_FORCED: u32 = 1;
/// Default session timeout (`opts.timeout_sec`).
pub const DEFAULT_TIMEOUT_MS: u32 = 30_000;

/// Time source for session timeouts; tests install [`ManualClock`].
pub trait Clock: Send + Sync {
    /// Milliseconds since an arbitrary origin.
    fn now_ms(&self) -> i64;
}

/// Wall-independent clock used by the server.
#[derive(Debug)]
pub struct MonotonicClock {
    origin: Instant,
}

impl MonotonicClock {
    /// Starts the clock now.
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Default for MonotonicClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for MonotonicClock {
    fn now_ms(&self) -> i64 {
        self.origin.elapsed().as_millis() as i64
    }
}

/// Clock advanced explicitly by tests.
#[derive(Debug, Default)]
pub struct ManualClock {
    // A mutex rather than `AtomicI64`, which 32-bit MIPS does not provide.
    now_ms: Mutex<i64>,
}

impl ManualClock {
    /// Creates a clock at `now_ms`.
    pub fn new(now_ms: i64) -> Self {
        Self {
            now_ms: Mutex::new(now_ms),
        }
    }

    /// Sets the current time.
    pub fn set(&self, now_ms: i64) {
        *self.lock() = now_ms;
    }

    /// Moves the clock forward by `delta_ms`.
    pub fn advance(&self, delta_ms: i64) {
        let mut now = self.lock();
        *now = now.wrapping_add(delta_ms);
    }

    fn lock(&self) -> MutexGuard<'_, i64> {
        self.now_ms.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> i64 {
        *self.lock()
    }
}

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
    /// Idle timeout in milliseconds; zero disables it, one forces a close.
    pub timeout_ms: u32,
    /// Last request or RTCP time (`sid->rtime`).
    pub rtime_ms: i64,
    /// Client user agent.
    pub user_agent: String,
    /// Accepted SRT socket, or `-1`.
    pub srt_sock: i32,
    /// Stream id used to match the SRT socket.
    pub srt_streamid: String,
    /// Whether the stream was touched since creation.
    pub touched: bool,
}

impl Stream {
    fn new(
        id: StreamId,
        sock: i32,
        session_id: i64,
        start_rtp: u16,
        timeout_ms: u32,
        now_ms: i64,
    ) -> Self {
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
            timeout_ms,
            rtime_ms: now_ms,
            user_agent: String::new(),
            srt_sock: -1,
            srt_streamid: String::new(),
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
    clock: Arc<dyn Clock>,
    server_timeout_ms: u32,
    adapter_ccerr: Vec<u64>,
    web: Option<WebService>,
    web_runtime: RuntimeState,
    #[cfg(feature = "srt")]
    srt: Option<SrtListener<LibSrtOps>>,
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
        Self::with_clock(sched, Arc::new(MonotonicClock::new()))
    }

    /// Creates an empty manager around an existing scheduler and clock.
    pub fn with_clock(sched: Scheduler, clock: Arc<dyn Clock>) -> Self {
        let mut streams = Vec::with_capacity(STREAM_SLOTS);
        streams.resize_with(STREAM_SLOTS, || None);
        Self {
            streams,
            sched,
            sock: -1,
            next_session_id: 1000,
            start_rtp: 5500,
            describe: DescribeOptions::default(),
            clock,
            server_timeout_ms: DEFAULT_TIMEOUT_MS,
            adapter_ccerr: vec![0; MAX_ADAPTERS],
            web: None,
            web_runtime: RuntimeState::default(),
            #[cfg(feature = "srt")]
            srt: None,
        }
    }

    /// Installs the web UI and generated HTTP endpoints.
    pub fn set_web(&mut self, web: WebService) {
        self.web = Some(web);
    }

    /// Sets the runtime values reported by `state.json`.
    pub fn set_web_runtime(&mut self, runtime: RuntimeState) {
        self.web_runtime = runtime;
    }

    /// Installs the listener whose pending pool serves SRT SETUPs.
    #[cfg(feature = "srt")]
    pub fn set_srt_listener(&mut self, listener: SrtListener<LibSrtOps>) {
        self.srt = Some(listener);
    }

    /// Installed SRT listener, if any.
    #[cfg(feature = "srt")]
    pub fn srt_listener(&self) -> Option<&SrtListener<LibSrtOps>> {
        self.srt.as_ref()
    }

    /// Removes the SRT listener so the caller can close it.
    #[cfg(feature = "srt")]
    pub fn take_srt_listener(&mut self) -> Option<SrtListener<LibSrtOps>> {
        self.srt.take()
    }

    /// Sets the UDP destination used by a unicast stream.
    ///
    /// `decode_transport` cannot know the RTSP peer, and C++ falls back to the
    /// peer address when the `Transport` header has no `destination=`
    /// (stream.cpp:505-506).
    pub fn set_udp_target(&mut self, id: StreamId, host: &str, port: u16) {
        if let Some(index) = self.slot_index(id)
            && let Some(stream) = self.streams[index].as_mut()
        {
            if !host.is_empty() {
                stream.remote_host = host.to_owned();
            }
            if port != 0 {
                stream.remote_port = port;
            }
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

    /// Installs a clock, rebasing the idle time of every live stream.
    pub fn set_clock(&mut self, clock: Arc<dyn Clock>) {
        self.clock = clock;
        let now = self.clock.now_ms();
        for stream in self.streams.iter_mut().flatten() {
            stream.rtime_ms = now;
        }
    }

    /// Time source used for idle timeouts.
    pub fn clock(&self) -> &Arc<dyn Clock> {
        &self.clock
    }

    /// Sets the default session timeout (`opts.timeout_sec`).
    pub fn set_server_timeout_ms(&mut self, timeout_ms: u32) {
        self.server_timeout_ms = timeout_ms;
    }

    /// Records the continuity error count of one adapter for the web UI.
    pub fn set_adapter_ccerr(&mut self, adapter: usize, errors: u64) {
        if let Some(slot) = self.adapter_ccerr.get_mut(adapter) {
            *slot = errors;
        }
    }

    /// Streams whose idle timeout expired, in slot order (`stream_timeout`,
    /// stream.cpp:1332-1378). HTTP and SRT streams have no idle timeout.
    pub fn expired_streams(&self) -> Vec<StreamId> {
        let now = self.clock.now_ms();
        let mut expired = Vec::new();
        for slot in self
            .streams
            .iter()
            .flatten()
            .filter(|stream| stream.enabled)
        {
            if slot.timeout_ms == TIMEOUT_FORCED {
                expired.push(slot.id);
                continue;
            }
            if matches!(slot.stream_type, StreamType::Http | StreamType::RtspSrt) {
                continue;
            }
            if slot.timeout_ms != 0
                && now - slot.rtime_ms > i64::from(slot.timeout_ms) + TIMEOUT_GRACE_MS
            {
                expired.push(slot.id);
            }
        }
        expired
    }

    /// Whether closing the control connection closes this stream
    /// (`close_http`, minisatip.cpp:1678-1692). RTSP/UDP and not-yet-decoded
    /// streams with an active timeout are kept.
    pub fn closes_on_control_close(&self, id: StreamId) -> bool {
        self.stream(id).is_none_or(|stream| {
            !(matches!(stream.stream_type, StreamType::RtspUdp | StreamType::None)
                && stream.timeout_ms != 0)
        })
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
        let timeout_ms = self.server_timeout_ms;
        let now_ms = self.clock.now_ms();
        self.streams[index] = Some(Stream::new(
            id,
            self.sock,
            session_id,
            self.start_rtp,
            timeout_ms,
            now_ms,
        ));
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
        #[cfg(feature = "srt")]
        let srt_socket = self.streams[index].as_ref().and_then(|stream| {
            (stream.stream_type == StreamType::RtspSrt && stream.srt_sock != SRT_INVALID_SOCK)
                .then(|| (stream.srt_sock, stream.srt_streamid.clone()))
        });
        if let Some(stream) = self.streams[index].as_mut() {
            stream.enabled = false;
            stream.adapter = -1;
            stream.do_play = false;
            stream.srt_sock = -1;
        }
        if adapter >= 0 {
            self.sched.close_adapter_for_stream(id, adapter);
        }
        // C++ returns a connected SRT socket to the pending pool (stream.cpp:379).
        #[cfg(feature = "srt")]
        if let (Some((sock, streamid)), Some(listener)) = (srt_socket, self.srt.as_ref()) {
            listener.return_socket(sock, &streamid);
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
                #[cfg(not(feature = "srt"))]
                {
                    // C++ DISABLE_SRT builds reject the transport (stream.cpp:412).
                    let _ = &spec.srt_streamid;
                    return Err(TransportError);
                }
                #[cfg(feature = "srt")]
                {
                    let Some(streamid) = spec.srt_streamid.as_deref() else {
                        return Err(TransportError);
                    };
                    let Some(listener) = self.srt.as_ref() else {
                        return Err(TransportError);
                    };
                    let sock = listener.take(streamid);
                    if sock == SRT_INVALID_SOCK {
                        return Err(TransportError);
                    }
                    stream.srt_sock = sock;
                    stream.srt_streamid = streamid.to_owned();
                    stream.stream_type = StreamType::RtspSrt;
                }
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
                params.freq / 1_000,
                polarization_name(params.pol),
                delivery_system_name(params.msys),
                modulation_name(params),
                pilot_name(params.plts),
                rolloff_name(params.ro),
                params.sr / 1_000,
                fec_name(params),
            ),
            DeliverySystem::DvbT | DeliverySystem::DvbT2 => format!(
                "ver=1.1;tuner={tuner},{strength},{status},{snr},{:.2},{},{},{},{},{},{},{},{},{};pids=",
                f64::from(params.freq) / 1_000.0,
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
                f64::from(params.freq) / 1_000.0,
                params.bw.unwrap_or(8_000_000) / 1_000_000,
                delivery_system_name(params.msys),
                modulation_name(params),
                params.sr / 1_000,
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

    /// Captures adapter, stream and runtime values for the web endpoints.
    pub fn web_snapshot(&self) -> WebSnapshot {
        let mut snapshot = WebSnapshot::new();
        snapshot.adapters = vec![AdapterState::default(); ms_sched::MAX_ADAPTERS];
        for aid in 0..ms_sched::MAX_ADAPTERS {
            snapshot.adapters[aid].disabled = self.sched.is_disabled(aid);
            let Some(adapter) = self.sched.adapter(aid) else {
                continue;
            };
            let params = &adapter.params;
            let tuned = params.freq > 0 || adapter.master_sid >= 0;
            snapshot.adapters[aid] = AdapterState {
                present: true,
                enabled: adapter.config.enabled,
                disabled: self.sched.is_disabled(aid),
                kind: ADAPTER_DVB,
                systems: adapter
                    .config
                    .sys
                    .iter()
                    .copied()
                    .filter(|sys| *sys != DeliverySystem::Undefined)
                    .collect(),
                pos: i32::from(params.src),
                freq: params.freq,
                pol: polarization_code(params.pol),
                sr: params.sr,
                bw: params.bw.unwrap_or(8_000_000),
                stream: params.plp.unwrap_or(-1),
                fe: params.fe.unwrap_or(0),
                master: adapter.master_sid,
                sid_count: adapter.sid_cnt,
                sys: if tuned {
                    delivery_system_code(params.msys)
                } else {
                    0
                },
                mtype: modulation_code(params, tuned),
                t2id: params.t2id.unwrap_or(0),
                c2tft: params.c2tft.unwrap_or(0),
                plp: params.ds.unwrap_or(-1),
                plsc: params.plsc.unwrap_or(-1),
                sm: params.sm.unwrap_or(0),
                gi: params.gi.unwrap_or(GUARD_INTERVAL_AUTO),
                pids: adapter.active_pids(),
                ccerr: self
                    .adapter_ccerr
                    .get(aid)
                    .copied()
                    .unwrap_or(0)
                    .min(i32::MAX as u64) as i32,
                ..AdapterState::default()
            };
        }
        snapshot.streams = vec![StreamState::default(); STREAM_SLOTS];
        for (index, stream) in self.streams.iter().enumerate() {
            let Some(stream) = stream else {
                continue;
            };
            snapshot.streams[index] = StreamState {
                enabled: stream.enabled,
                play: stream.do_play,
                adapter: stream.adapter,
                user_agent: stream.user_agent.clone(),
                rhost: stream.remote_host.clone(),
                rport: stream.remote_port,
                protocol: protocol_name(stream.stream_type).to_owned(),
                pids: stream.params.pids.iter().map(i32::from).collect(),
                ..StreamState::default()
            };
        }
        snapshot.runtime = self.web_runtime.at(SystemTime::now());
        snapshot
    }

    /// Serves the configured web UI, or `None` when it is not installed.
    pub fn web_route(&self, request: &Request) -> Option<HttpRoute> {
        let web = self.web.as_ref()?;
        web.route(request, &self.web_snapshot())
    }
}

impl StreamBackend for SessionManager {
    fn find_session(&mut self, session_id: i64) -> Option<StreamId> {
        let found = (0..STREAM_SLOTS).find_map(|index| {
            self.streams[index]
                .as_ref()
                .filter(|stream| stream.enabled && stream.session_id == session_id)
                .map(|stream| stream.id)
        });
        // C++ `find_session_id` refreshes `sid->rtime` (stream.cpp:1438).
        if let Some(id) = found {
            self.touch_stream(id);
        }
        found
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
        let now = self.clock.now_ms();
        if let Some(index) = self.slot_index(id)
            && let Some(stream) = self.streams[index].as_mut()
        {
            stream.touched = true;
            stream.rtime_ms = now;
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

    fn http_route(&mut self, request: &Request) -> Option<HttpRoute> {
        self.web_route(request)
    }
}

fn polarization_code(pol: Polarization) -> i32 {
    match pol {
        Polarization::Vertical => 1,
        Polarization::Horizontal => 2,
        Polarization::Right => 3,
        Polarization::Left => 4,
        Polarization::None => 0,
    }
}

/// `tp.mtype` after `copy_dvb_parameters` (dvb.cpp:456-460), or the
/// `QAM_AUTO` getter default when unset (adapter.cpp:2397).
fn modulation_code(params: &TuneParams, tuned: bool) -> u32 {
    match (params.mtype_specified, tuned, params.msys) {
        (true, _, _) => params.mtype,
        (false, true, DeliverySystem::DvbS2) => 9,
        (false, true, DeliverySystem::DvbS) => 0,
        _ => QAM_AUTO as u32,
    }
}

fn delivery_system_code(sys: DeliverySystem) -> i32 {
    match sys {
        DeliverySystem::DvbC => 1,
        DeliverySystem::DvbT => 3,
        DeliverySystem::DvbS => 5,
        DeliverySystem::DvbS2 => 6,
        DeliverySystem::DvbT2 => 16,
        DeliverySystem::DvbC2 => 19,
        DeliverySystem::Undefined | DeliverySystem::Other(_) => 0,
    }
}

fn protocol_name(stream_type: StreamType) -> &'static str {
    match stream_type {
        StreamType::Http => "HTTP",
        StreamType::RtspUdp => "RTP/UDP",
        StreamType::RtspTcp => "RTP/TCP",
        StreamType::RtspSrt => "SRT",
        StreamType::None => "unkown",
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

#[cfg(test)]
mod tests {
    use super::*;

    fn manual_manager(clock: &Arc<ManualClock>) -> SessionManager {
        let mut manager = SessionManager::new();
        let manual: Arc<ManualClock> = Arc::clone(clock);
        let clock: Arc<dyn Clock> = manual;
        manager.set_clock(clock);
        manager
    }

    #[test]
    fn srt_streams_never_expire() {
        let clock = Arc::new(ManualClock::new(0));
        let mut manager = manual_manager(&clock);
        let id = manager.add_stream().expect("slot");
        manager.streams[id as usize]
            .as_mut()
            .expect("stream")
            .stream_type = StreamType::RtspSrt;

        // stream.cpp:1353: the timeout block skips HTTP and SRT streams.
        clock.advance(10_000_000);
        assert!(manager.expired_streams().is_empty());
    }

    #[test]
    fn forced_timeout_closes_immediately() {
        let clock = Arc::new(ManualClock::new(0));
        let mut manager = manual_manager(&clock);
        let id = manager.add_stream().expect("slot");
        manager.streams[id as usize]
            .as_mut()
            .expect("stream")
            .timeout_ms = TIMEOUT_FORCED;

        // stream.cpp:1346: `sid->timeout == 1` closes without waiting.
        assert_eq!(manager.expired_streams(), vec![id]);
    }
}
