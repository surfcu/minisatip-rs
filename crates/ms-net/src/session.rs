//! Stream table and RTSP session manager implementing [`StreamBackend`].
//!
//! The manager owns all mutable state; adapter decisions are delegated to
//! [`ms_sched::Scheduler`].

use ms_sched::pmt::PmtState as SchedPmtState;
use ms_sched::{AdapterType, MAX_ADAPTERS, Scheduler, SetParamsError, StreamId, TuneError};
use ms_tuner::{DeliverySystem, PID_ALL, Polarization, TuneParams};
use std::collections::HashMap;
use std::io;
use std::net::UdpSocket;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Instant, SystemTime};

use crate::bandwidth::Bandwidth;
use crate::handler::{
    HttpResource, HttpRoute, MAX_STREAMS, PlayError, StreamBackend, StreamInfo, StreamType,
    TransportError,
};
use crate::rtsp::{
    Protocol, Request, TransportKind, TransportMode, TransportSpec, parse_int_prefix,
    parse_transport, split_tokens,
};
use crate::satip::{merge_satip_url, parse_satip_url_new};
#[cfg(feature = "srt")]
use crate::srt::{LibSrtOps, SRT_INVALID_SOCK, SrtListener};
use crate::web::{
    ADAPTER_CI, ADAPTER_DVB, ADAPTER_NETCV, ADAPTER_SATIP, AdapterState, CaDeviceState,
    GUARD_INTERVAL_AUTO, KeyState, PmtState, QAM_AUTO, RuntimeState, StreamState, WebService,
    WebSnapshot,
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
    /// Creation time (`sid->wtime` assigned by `streams_add`, stream.cpp:634).
    pub setup_ms: i64,
    /// Client user agent.
    pub user_agent: String,
    /// Accepted SRT socket, or `-1`.
    pub srt_sock: i32,
    /// Stream id used to match the SRT socket.
    pub srt_streamid: String,
    /// Whether the stream was touched since creation.
    pub touched: bool,
    /// Whether `session_open` was emitted and `session_close` is still due.
    opened: bool,
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
            setup_ms: now_ms,
            user_agent: String::new(),
            srt_sock: -1,
            srt_streamid: String::new(),
            touched: false,
            opened: false,
        }
    }

    /// Protocol name used by `session_open`/`session_close`.
    fn diag_proto(&self) -> &'static str {
        match self.stream_type {
            StreamType::Http => "http",
            _ => "rtsp",
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

/// Opens the RTP/RTCP sockets of a UDP transport: server port, client host and port.
pub type UdpBinder = Box<dyn FnMut(u16, &str, u16) -> io::Result<(UdpSocket, UdpSocket)> + Send>;

/// Stream table plus adapter scheduler.
pub struct SessionManager {
    streams: Vec<Option<Stream>>,
    /// Binds at SETUP like `decode_transport`; unset, no UDP sockets are opened.
    udp_binder: Option<UdpBinder>,
    /// RTP/RTCP sockets of each UDP stream (`sid->rsock`/`sid->rtcp`).
    udp_sockets: HashMap<StreamId, (UdpSocket, UdpSocket)>,
    sched: Scheduler,
    sock: i32,
    next_session_id: i64,
    start_rtp: u16,
    rrtp: Option<String>,
    disc_host: String,
    describe: DescribeOptions,
    clock: Arc<dyn Clock>,
    server_timeout_ms: u32,
    adapter_ccerr: Vec<u64>,
    web: Option<WebService>,
    web_runtime: RuntimeState,
    /// `ca_sym` slots, pushed by the CA runtime.
    web_ca: Vec<Option<CaDeviceState>>,
    /// `dvbapi_sym` slots, pushed by the CA runtime.
    web_keys: Vec<Option<KeyState>>,
    /// `ad_decerr` per adapter, pushed by the CA runtime.
    web_decerr: Vec<i32>,
    bandwidth: Arc<Bandwidth>,
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
            udp_binder: None,
            udp_sockets: HashMap::new(),
            sched,
            sock: -1,
            next_session_id: 1000,
            start_rtp: 5500,
            rrtp: None,
            disc_host: "239.255.255.250".to_owned(),
            describe: DescribeOptions::default(),
            clock,
            server_timeout_ms: DEFAULT_TIMEOUT_MS,
            adapter_ccerr: vec![0; MAX_ADAPTERS],
            web: None,
            web_runtime: RuntimeState::default(),
            web_ca: Vec::new(),
            web_keys: Vec::new(),
            web_decerr: Vec::new(),
            bandwidth: Arc::new(Bandwidth::new()),
            #[cfg(feature = "srt")]
            srt: None,
        }
    }

    /// Makes `decode_transport` open the UDP sockets at SETUP (stream.cpp:556-584).
    pub fn set_udp_binder(&mut self, binder: UdpBinder) {
        self.udp_binder = Some(binder);
    }

    /// Handles to the RTP/RTCP sockets of `id`; `None` stands for C++ `rsock < 0`.
    pub fn udp_sockets(&self, id: StreamId) -> Option<(UdpSocket, UdpSocket)> {
        let (rtp, rtcp) = self.udp_sockets.get(&id)?;
        match (rtp.try_clone(), rtcp.try_clone()) {
            (Ok(rtp), Ok(rtcp)) => Some((rtp, rtcp)),
            (Err(error), _) | (_, Err(error)) => {
                ms_log::log!("cannot share the RTP sockets of sid {}: {}", id, error);
                None
            }
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

    /// Installs the CA device and DVBAPI key slots shown by `/state.json`.
    pub fn set_web_ca(&mut self, ca: Vec<Option<CaDeviceState>>, keys: Vec<Option<KeyState>>) {
        self.web_ca = ca;
        self.web_keys = keys;
    }

    /// Installs the per-adapter decrypt errors shown as `ad_decerr`.
    pub fn set_web_decerr(&mut self, decerr: Vec<i32>) {
        self.web_decerr = decerr;
    }

    /// Installs the process-wide counters reported by `/bandwidth.json`.
    pub fn set_bandwidth(&mut self, bandwidth: Arc<Bandwidth>) {
        self.bandwidth = bandwidth;
    }

    /// Time since the stream was created (`getTick() - sid->wtime`, with
    /// `wtime` assigned at SETUP; stream.cpp:634,1293).
    pub fn setup_age_ms(&self, id: StreamId) -> Option<i64> {
        let index = self.slot_index(id)?;
        let stream = self.streams[index].as_ref()?;
        Some(self.clock.now_ms().saturating_sub(stream.setup_ms))
    }

    /// Refreshes the clock values recomputed per HTTP request
    /// (minisatip.cpp:1155-1167).
    pub fn set_web_clock(&mut self, current: String, running: String) {
        self.web_runtime.datetime_current = current;
        self.web_runtime.time_running = running;
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

    /// Applies the destination defaults `decode_transport` takes from the RTSP
    /// peer and `-r` (stream.cpp:503-510), before the SETUP reply is built.
    pub fn apply_transport_defaults(
        &mut self,
        id: StreamId,
        peer_host: &str,
        default_rtp: Option<&str>,
        start_rtp: u16,
    ) {
        if let Some(index) = self.slot_index(id)
            && let Some(stream) = self.streams[index].as_mut()
            && stream.stream_type == StreamType::RtspUdp
        {
            stream.remote_host = match default_rtp {
                Some(host) => host.to_owned(),
                None if stream.remote_host.is_empty() || stream.remote_host == "0.0.0.0" => {
                    peer_host.to_owned()
                }
                None => stream.remote_host.clone(),
            };
            if stream.remote_port == 0 {
                stream.remote_port = start_rtp;
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
                // stream.cpp:1347.
                ms_log::log!("Stream forced close requested for sid {}", slot.id);
                expired.push(slot.id);
                continue;
            }
            if matches!(slot.stream_type, StreamType::Http | StreamType::RtspSrt) {
                continue;
            }
            if slot.timeout_ms != 0
                && now - slot.rtime_ms > i64::from(slot.timeout_ms) + TIMEOUT_GRACE_MS
            {
                // stream.cpp:1367 (note the space before the comma in C++).
                ms_log::log!(
                    "Stream timeout sid {}, closing (ctime {} , sid->rtime {}, sid->timeout {})",
                    slot.id,
                    now,
                    slot.rtime_ms,
                    slot.timeout_ms
                );
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
        let Some(index) = (0..STREAM_SLOTS).find(|index| {
            self.streams[*index]
                .as_ref()
                .is_none_or(|stream| !stream.enabled)
        }) else {
            // stream.cpp:614 (`LOG_AND_RETURN(-1, "streams_add failed")`).
            ms_log::log!("streams_add failed");
            return None;
        };
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
        // stream.cpp:175: `s->sid`, `s->id` and `s->sock`. The Rust backend has
        // no socket-table index, so `sock_id` is -1; `self.sock` is `s->sock`.
        ms_log::log!(
            "Setup stream sid {} parameters, sock_id {}, handle {}",
            current.unwrap_or(-1),
            -1,
            self.sock
        );
        let id = match existing {
            Some(id) => id,
            None => {
                let Some(id) = self.add_stream() else {
                    // stream.cpp:181.
                    ms_log::log_and_return!(None, "Could not add a new stream");
                };
                // stream.cpp:199 (the stream timeout socket has no Rust port,
                // so the handle stays `self.sock`).
                ms_log::log!(
                    "Setup stream done: sid {} for sock {} handle {}",
                    id,
                    -1,
                    self.sock
                );
                id
            }
        };
        let Some(index) = self.slot_index(id) else {
            // stream.cpp:203.
            ms_log::log_and_return!(
                None,
                "stream sid {} not enabled for sock_id {} handle {}",
                id,
                -1,
                self.sock
            );
        };
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

    /// [`Self::start_play`] for a request from `peer_host:peer_port`; an HTTP
    /// stream records the peer as its destination (stream.cpp:236-241).
    pub fn start_play_from(
        &mut self,
        id: StreamId,
        protocol: Protocol,
        peer_host: &str,
        peer_port: u16,
    ) -> Result<(), PlayError> {
        if protocol == Protocol::Http
            && let Some(index) = self.slot_index(id)
            && let Some(stream) = self.streams[index].as_mut()
            && stream.stream_type == StreamType::None
        {
            stream.remote_host = peer_host.to_owned();
            stream.remote_port = peer_port;
        }
        self.start_play(id, protocol)
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
                // stream.cpp:250.
                ms_log::log_and_return!(
                    Err(PlayError::new(454, None)),
                    "No Transport header was specified, for stream sid {}",
                    id
                );
            }
        }

        let mut adapter = self.streams[index].as_ref().expect("checked slot").adapter;
        // stream.cpp:254: the Rust backend has no `rsock`/`rsock_id`/socket
        // index, so those print -1; `handle` is `self.sock` (C++ `s->sock`).
        let stream_type = self.streams[index]
            .as_ref()
            .expect("checked slot")
            .stream_type;
        ms_log::log!(
            "Play for stream sid {}, type {}, rsock {}, adapter {}, sock_id {}, rsock_id {}, handle {}",
            id,
            stream_type_number(stream_type),
            -1,
            adapter,
            -1,
            -1,
            self.sock
        );
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
                self.diag_open(index);
                // stream.cpp:286.
                ms_log::log_and_return!(
                    Ok(()),
                    "Tune requested with no real parameters, ignoring ..."
                );
            }
            if adapter >= 0 {
                // stream.cpp:290.
                ms_log::log!("slave stream tuning to a new frequency, finding a new adapter");
                self.sched.close_adapter_for_stream(id, adapter);
                if let Some(stream) = self.streams[index].as_mut() {
                    stream.adapter = -1;
                }
            }
            let Some(aid) = self.sched.get_free_adapter(&params) else {
                return Err(self.allocation_error(index, &params));
            };
            // stream.cpp:295.
            ms_log::log!("Got adapter {} on sid {} socket {}", aid, id, -1);
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
            Ok(()) => {
                self.diag_open(index);
                Ok(())
            }
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

    /// Emits `session_open` once playback starts.
    fn diag_open(&mut self, index: usize) {
        let Some(stream) = self.streams[index].as_mut() else {
            return;
        };
        if stream.opened {
            return;
        }
        stream.opened = true;
        let session = i64::from(stream.id);
        let adapter = i64::from(stream.adapter);
        let proto = stream.diag_proto();
        ms_log::diag::events::session_open(session, proto, adapter, || {
            format!("{}:{}", stream.remote_host, stream.remote_port)
        });
    }

    /// Emits `session_close` when a live session ends.
    fn diag_close(&mut self, index: usize) {
        let Some(stream) = self.streams[index].as_mut() else {
            return;
        };
        if !stream.opened {
            return;
        }
        stream.opened = false;
        let session = i64::from(stream.id);
        let adapter = i64::from(stream.adapter);
        let proto = stream.diag_proto();
        ms_log::diag::events::session_close(session, proto, adapter, || {
            format!("{}:{}", stream.remote_host, stream.remote_port)
        });
    }

    /// Disables a stream whose adapter was already released by the scheduler.
    fn disable_stream(&mut self, id: StreamId) {
        let Some(index) = self.slot_index(id) else {
            return;
        };
        self.diag_close(index);
        if let Some(stream) = self.streams[index].as_mut() {
            stream.enabled = false;
            stream.adapter = -1;
            stream.do_play = false;
        }
    }

    /// Closes a stream and releases its adapter.
    pub fn close_stream(&mut self, id: StreamId) {
        // stream.cpp:352: logged before the validity check.
        ms_log::log!("closing stream {}", id);
        let Some(index) = self.slot_index(id) else {
            return;
        };
        self.diag_close(index);
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
        self.udp_sockets.remove(&id);
        // C++ returns a connected SRT socket to the pending pool (stream.cpp:379).
        #[cfg(feature = "srt")]
        if let (Some((sock, streamid)), Some(listener)) = (srt_socket, self.srt.as_ref()) {
            listener.return_socket(sock, &streamid);
        }
        // stream.cpp:408.
        ms_log::log!("closed stream {}", id);
    }

    /// Sets `-r` (`opts.rrtp`) and the `opts.disc_host` fallback used by
    /// `decode_transport` (stream.cpp:503-508).
    pub fn set_transport_defaults(&mut self, rrtp: Option<String>, disc_host: &str) {
        self.rrtp = rrtp.filter(|host| !host.is_empty());
        self.disc_host = disc_host.to_owned();
    }

    /// Applies a `Transport` header without a known RTSP peer.
    pub fn decode_transport(&mut self, id: StreamId, value: &str) -> Result<(), TransportError> {
        self.decode_transport_from(id, value, "", 0)
    }

    /// Applies a `Transport` header received from `peer_host:peer_port`.
    pub fn decode_transport_from(
        &mut self,
        id: StreamId,
        value: &str,
        peer_host: &str,
        peer_port: u16,
    ) -> Result<(), TransportError> {
        let Some(index) = self.slot_index(id) else {
            // stream.cpp:457: `s->id` has no Rust counterpart, so sock_id is -1.
            ms_log::log!(
                "Error: No stream to set transport to, sock_id {}, arg {} ",
                -1,
                if value.is_empty() { "NULL" } else { value }
            );
            return Err(TransportError);
        };
        let spec = parse_transport(value);
        let stream = self.streams[index].as_mut().expect("checked slot");
        match spec.kind {
            // stream.cpp:470-481: TCP takes the RTSP peer and ignores port tokens.
            TransportKind::Tcp => {
                // stream.cpp:471.
                ms_log::log!(
                    "Assuming RTSP over TCP for stream sid {}, arg {}",
                    id,
                    value
                );
                stream.stream_type = StreamType::RtspTcp;
                stream.remote_host = peer_host.to_owned();
                stream.remote_port = peer_port;
                self.udp_sockets.remove(&id);
                return Ok(());
            }
            TransportKind::Udp => {
                return self.decode_udp_transport(index, &spec, value, peer_host);
            }
            TransportKind::Srt => {}
        }
        match spec.kind {
            TransportKind::Srt => {
                // stream.cpp:466: logged before `decode_transport_srt`.
                ms_log::log!(
                    "Assuming SRT transport for stream sid {}, arg {}",
                    id,
                    value
                );
                #[cfg(not(feature = "srt"))]
                {
                    // C++ DISABLE_SRT builds reject the transport (stream.cpp:412).
                    let _ = &spec.srt_streamid;
                    // stream.cpp:446.
                    ms_log::log0!("SRT not supported, install libsrt-openssl-dev");
                    return Err(TransportError);
                }
                #[cfg(feature = "srt")]
                {
                    // stream.cpp:416.
                    let Some(listener) = self.srt.as_ref() else {
                        ms_log::log_and_return!(
                            Err(TransportError),
                            "SRT listener not initialized"
                        );
                    };
                    // stream.cpp:429.
                    let Some(streamid) = spec.srt_streamid.as_deref() else {
                        ms_log::log_and_return!(
                            Err(TransportError),
                            "SRT transport missing srt_streamid= parameter"
                        );
                    };
                    let sock = listener.take(streamid);
                    if sock == SRT_INVALID_SOCK {
                        // stream.cpp:436.
                        ms_log::log_and_return!(
                            Err(TransportError),
                            "Failed finding pending SRT socket for sid {}, srt_streamid {}",
                            id,
                            streamid
                        );
                    }
                    stream.srt_sock = sock;
                    stream.srt_streamid = streamid.to_owned();
                    stream.stream_type = StreamType::RtspSrt;
                    // stream.cpp:441.
                    ms_log::log!(
                        "SRT stream setup for sid {}, srt_streamid='{}', srt_sock={} (from pending)",
                        id,
                        streamid,
                        sock
                    );
                }
            }
            TransportKind::Tcp | TransportKind::Udp => {}
        }
        Ok(())
    }

    /// UDP branch of `decode_transport` (stream.cpp:484-606).
    fn decode_udp_transport(
        &mut self,
        index: usize,
        spec: &TransportSpec,
        value: &str,
        peer_host: &str,
    ) -> Result<(), TransportError> {
        // Tokens are scanned last to first, so the first `client_port=`/`port=` wins.
        let port = split_tokens(value, ';')
            .into_iter()
            .find_map(|token| {
                token
                    .strip_prefix("client_port=")
                    .or_else(|| token.strip_prefix("port="))
            })
            .map_or(0, |port| parse_int_prefix(port, 0));
        let port = u16::try_from(port)
            .ok()
            .filter(|port| *port != 0)
            .unwrap_or(self.start_rtp);
        let mut dest = self
            .rrtp
            .clone()
            .or_else(|| spec.destination.clone())
            .unwrap_or_default();
        if dest.is_empty() && spec.mode == Some(TransportMode::Unicast) {
            dest = peer_host.to_owned();
        }
        if dest.is_empty() {
            dest = self.disc_host.clone();
        }

        // stream.cpp:511: `p.type` (0/1/2) and `p.ttl`, with the resolved target.
        let type_number = match spec.mode {
            Some(TransportMode::Multicast) => 1,
            Some(TransportMode::Unicast) => 2,
            None => 0,
        };
        ms_log::log!(
            "decode_transport ->type {}, ttl {} new socket to: {}:{}",
            type_number,
            spec.ttl.unwrap_or(0),
            dest,
            port
        );

        let stream = self.streams[index].as_mut().expect("checked slot");
        match stream.stream_type {
            // A stream with another transport keeps it (stream.cpp:513-547).
            StreamType::RtspTcp | StreamType::RtspSrt | StreamType::Http => return Ok(()),
            // `sid->rsock < 0` after a failed bind skips both branches (stream.cpp:513-550).
            StreamType::RtspUdp
                if self.udp_binder.is_some() && !self.udp_sockets.contains_key(&stream.id) =>
            {
                if stream.remote_port == port && stream.remote_host == dest {
                    return Ok(());
                }
            }
            StreamType::RtspUdp if stream.remote_port == port && stream.remote_host == dest => {
                // stream.cpp:519: `handle` (`sid->rsock`) is not modelled here.
                ms_log::log!(
                    "Transport for this connection is already setup to {}:{}, leaving as it is: sid = {}, handle {}",
                    dest,
                    port,
                    stream.id,
                    -1
                );
                return Ok(());
            }
            StreamType::RtspUdp => {
                // stream.cpp:525: `closing %d` is `sid->rsock`.
                ms_log::log!(
                    "Stream has already a transport header associated with it to {}:{} - sid = {} type = {}, closing {}",
                    stream.remote_host,
                    stream.remote_port,
                    stream.id,
                    stream_type_number(stream.stream_type),
                    -1
                );
            }
            StreamType::None => {}
        }
        stream.stream_type = StreamType::RtspUdp;
        stream.remote_host = dest.clone();
        stream.remote_port = port;
        let id = stream.id;
        let server_port = stream.server_port;
        // The previous transport's sockets close first (stream.cpp:533-541).
        self.udp_sockets.remove(&id);
        if let Some(binder) = self.udp_binder.as_mut() {
            // udp_bind_connect failures answer 400 (stream.cpp:556-584).
            let sockets = binder(server_port, &dest, port).map_err(|_| TransportError)?;
            self.udp_sockets.insert(id, sockets);
        }

        // A new UDP target closes other streams sending to it (stream.cpp:591-605).
        let duplicates: Vec<(StreamId, i32)> = self
            .streams
            .iter()
            .flatten()
            .filter(|other| {
                other.enabled
                    && other.id != id
                    && other.remote_port == port
                    && other.remote_host == dest
            })
            .map(|other| (other.id, other.adapter))
            .collect();
        for (other, aid) in duplicates {
            // stream.cpp:598.
            ms_log::log!(
                "Detected stream with the same destination as sid {}: sid {} -> {}:{}, aid: {}",
                id,
                other,
                dest,
                port,
                aid
            );
            self.close_stream(other);
        }
        Ok(())
    }

    /// Records the next RTP sequence number reported by `RTP-Info`.
    pub fn set_rtp_seq(&mut self, id: StreamId, seq: u16) {
        if let Some(index) = self.slot_index(id)
            && let Some(stream) = self.streams[index].as_mut()
        {
            stream.rtp_seq = seq;
        }
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
        let description = format!("{body}{}", self.stream_pids_text(id, aid, params, use_ad));
        ms_log::logl!(
            ms_log::LOG_ADAPTER,
            "describe_adapter: sid {} aid {} => {}",
            id,
            aid,
            description
        );
        description
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

    /// Streams the status page counts as active (enabled slots).
    pub fn active_streams(&self) -> usize {
        self.streams
            .iter()
            .flatten()
            .filter(|stream| stream.enabled)
            .count()
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
                kind: adapter_kind(adapter.config.adapter_type),
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
                // adapter.cpp:2399-2410 uses QAM_AUTO/GUARD_INTERVAL_AUTO
                // defaults and exposes `ds` as ad_plp; copy_dvb_parameters
                // keeps the DVB-S/S2 defaults for an unspecified mtype.
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
                decerr: self.web_decerr.get(aid).copied().unwrap_or(0),
                pmts: self
                    .sched
                    .active_pmt_ids(aid)
                    .iter()
                    .filter_map(|&id| self.sched.pmt(id))
                    .map(|pmt| PmtState {
                        running: pmt.state == SchedPmtState::Running,
                        name: pmt.name.clone(),
                        pid: pmt.pid,
                    })
                    .collect(),
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
                // get_stream_pids: the adapter's active pids carrying this stream.
                pids: self.sched.stream_pids(stream.adapter, index as StreamId),
                ..StreamState::default()
            };
        }
        snapshot.ca = self.web_ca.clone();
        snapshot.keys = self.web_keys.clone();
        snapshot.runtime = self.web_runtime.at(SystemTime::now());
        self.bandwidth.calculate();
        snapshot.bandwidth = self.bandwidth.snapshot();
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
            // stream.cpp:1439 (space before the comma is in the C++ text).
            ms_log::log!(
                "recovered session id from a closed connection, sid {} , id: {}",
                id,
                session_id
            );
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
            && stream.session_id != session_id
        {
            // stream.cpp:1414.
            ms_log::log!("Forcing session id {} on stream sid {}", session_id, id);
            stream.session_id = session_id;
        }
    }

    fn touch_stream(&mut self, id: StreamId) {
        let now = self.clock.now_ms();
        if let Some(index) = self.slot_index(id)
            && let Some(stream) = self.streams[index].as_mut()
        {
            // minisatip.cpp:1268-1271; that file's DEFAULT_LOG is LOG_HTTP.
            ms_log::logl!(
                ms_log::LOG_HTTP,
                "Updating sid {} time to {}, current time {}",
                id,
                now,
                now
            );
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

    fn decode_transport_from(
        &mut self,
        id: StreamId,
        value: &str,
        peer_host: &str,
        peer_port: u16,
    ) -> Result<(), TransportError> {
        SessionManager::decode_transport_from(self, id, value, peer_host, peer_port)
    }

    fn start_play_from(
        &mut self,
        id: StreamId,
        protocol: Protocol,
        peer_host: &str,
        peer_port: u16,
    ) -> Result<(), PlayError> {
        SessionManager::start_play_from(self, id, protocol, peer_host, peer_port)
    }

    fn apply_transport_defaults(
        &mut self,
        id: StreamId,
        peer_host: &str,
        default_rtp: Option<&str>,
        start_rtp: u16,
    ) {
        SessionManager::apply_transport_defaults(self, id, peer_host, default_rtp, start_rtp);
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

/// Numeric `sid->type` (`STREAM_*`, stream.h:20-23), used in log values.
fn stream_type_number(stream_type: StreamType) -> i32 {
    match stream_type {
        StreamType::Http => 1,
        StreamType::RtspUdp => 2,
        StreamType::RtspTcp => 3,
        StreamType::RtspSrt => 4,
        StreamType::None => 0,
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

/// `adapter::type` for `ad_type`. An unset type reports `ADAPTER_DVB`, so the
/// status page does not skip the adapter.
fn adapter_kind(kind: AdapterType) -> i32 {
    match kind {
        AdapterType::Satip => ADAPTER_SATIP,
        AdapterType::NetCv => ADAPTER_NETCV,
        AdapterType::Ci => ADAPTER_CI,
        AdapterType::Dvb | AdapterType::Unknown => ADAPTER_DVB,
    }
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
