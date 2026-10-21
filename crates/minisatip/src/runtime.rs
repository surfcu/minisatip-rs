//! Server runtime: listeners, the RTSP/HTTP control loop and adapter data planes.
//!
//! The control plane is a single thread driving [`SessionManager`]; every
//! accepted connection has a reader thread that parses requests and forwards
//! them here. Adapters run on their own data-plane threads.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use ms_config::Options;
use ms_net::bandwidth::Bandwidth;
use ms_net::bind::{BindOptions, resolve_bind_opts};
use ms_net::dataplane::{AdapterHandle, Command, DataPlaneConfig};
use ms_net::handler::{HandlerContext, StreamBackend, StreamType, handle_http, handle_rtsp};
use ms_net::output::{OutputConfig, StreamOutput, UdpTarget};
use ms_net::response::format_http_date;
use ms_net::rtsp::{ParseOutcome, Protocol, Request, parse_request_with};
use ms_net::session::{DescribeOptions, SessionManager};
use ms_net::socket::DEFAULT_FIFO_SIZE;
use ms_net::ssdp::{
    ANNOUNCE_INTERVAL_MS, SharedIdentity, SsdpConfig, SsdpServer, UdpSsdpIo, read_boot_id, uuid4,
};
use ms_net::web::{RuntimeState, WebConfig, WebService};
use ms_sched::{AdapterConfig, AdapterTuner, MAX_ADAPTERS, Scheduler, TuneError};
use ms_tuner::{DeliverySystem, FileReplayConfig, FileReplayTuner, Pacing, PidSet, TuneParams};

use crate::sys;

const READ_TIMEOUT: Duration = Duration::from_millis(100);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const LOOP_TICK: Duration = Duration::from_millis(20);
const ACCEPT_POLL: Duration = Duration::from_millis(20);
/// `set_linux_socket_timeout`: 1 s 100 ms (socketworks.cpp:374-386).
const SSDP_READ_TIMEOUT: Duration = Duration::from_millis(1100);

/// Fixed runtime values shared by every request.
struct ServerConfig {
    bind_ip: IpAddr,
    advertised_host: String,
    http_host: String,
    rtsp_host: String,
    start_rtp: u16,
    rtsp_port: u16,
    timeout_ms: u32,
    udp_threshold: i32,
    tcp_threshold: i32,
    fifo_size: usize,
    rrtp: Option<String>,
    /// `--bind-dev`: applied as `SO_BINDTODEVICE` to every socket.
    bind_dev: Option<String>,
    /// `-H`/`--output-buffer`: RTP/RTCP `SO_SNDBUF` when positive.
    output_buffer: i32,
}

/// An RTSP connection request forwarded by a reader thread.
struct ConnectionRequest {
    conn_id: u64,
    protocol: Protocol,
    peer: SocketAddr,
    local_ip: String,
    writer: TcpStream,
    request: Request,
    /// Read events on the connection so far (`sockets::iteration`).
    reads: u64,
}

enum Inbound {
    Request(Box<ConnectionRequest>),
    /// An interleaved RTP/RTCP frame was received (minisatip.cpp:1187-1189).
    Interleaved(u64),
    Closed(u64),
}

/// Outputs started by PLAY/GET, tracked so they can be stopped or re-targeted.
struct ActiveOutput {
    conn_id: u64,
    adapter: i32,
    epoch: u64,
    stream_type: StreamType,
    dest: Option<SocketAddr>,
    /// SSRC last pushed to the data plane (`sid->ssrc`).
    ssrc: u32,
}

/// Adapter tuner that forwards tunes to the data-plane thread.
struct DataPlaneTuner {
    commands: Sender<Command>,
}

impl AdapterTuner for DataPlaneTuner {
    fn tune(&mut self, params: &TuneParams) -> Result<(), TuneError> {
        self.commands
            .send(Command::Tune(Box::new(params.clone())))
            .map_err(|_| TuneError::Hardware)
    }
}

struct Runtime {
    config: ServerConfig,
    session: SessionManager,
    handles: Vec<Option<AdapterHandle>>,
    outputs: HashMap<i32, ActiveOutput>,
    conn_streams: HashMap<u64, i32>,
    started: Instant,
    start_unix: i64,
}

impl Runtime {
    fn new(
        opts: &Options,
        config: ServerConfig,
        uuid: String,
        bootid: i32,
        device_id: i32,
        identity: SharedIdentity,
    ) -> Result<Self, String> {
        let mut scheduler = Scheduler::new();
        let replay = std::env::var_os("MINISATIP_REPLAY").map(PathBuf::from);
        let adapter_ids = adapter_ids(opts);
        let mut handles: Vec<Option<AdapterHandle>> = (0..MAX_ADAPTERS).map(|_| None).collect();
        let bandwidth = Arc::new(Bandwidth::new());

        if let Some(path) = replay {
            for aid in adapter_ids {
                let tuner = FileReplayTuner::open(
                    &path,
                    FileReplayConfig {
                        looping: true,
                        pacing: Pacing::None,
                    },
                )
                .map_err(|error| format!("cannot replay {}: {error}", path.display()))?;
                let buffer = usize::try_from(opts.adapter_buffer)
                    .ok()
                    .filter(|size| *size > 0)
                    .unwrap_or(ms_net::dataplane::ADAPTER_BUFFER);
                let handle = AdapterHandle::spawn_with_bandwidth(
                    Box::new(tuner),
                    DataPlaneConfig {
                        adapter_id: aid as i32,
                        adapter_buffer: buffer,
                        threshold_ms: ms_net::dataplane::clamp_threshold(opts.udp_threshold),
                    },
                    Some(Arc::clone(&bandwidth)),
                )
                .map_err(|error| format!("cannot start adapter {aid}: {error}"))?;
                let bridge = DataPlaneTuner {
                    commands: handle.command_sender(),
                };
                scheduler.add_adapter(aid, adapter_config(), Box::new(bridge));
                handles[aid] = Some(handle);
            }
        }

        let mut session = SessionManager::with_scheduler(scheduler);
        session.set_bandwidth(Arc::clone(&bandwidth));
        session.set_start_rtp(config.start_rtp);
        session.set_transport_defaults(config.rrtp.clone(), &opts.disc_host);
        session.set_server_timeout_ms(config.timeout_ms);
        session.set_describe_options(DescribeOptions {
            local_host: config.advertised_host.clone(),
            session_id: None,
            session_version: None,
            time: "0 0".to_owned(),
        });

        let web = WebService::new(WebConfig {
            document_root: PathBuf::from(&opts.document_root),
            name_app: opts.name_app.clone(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            uuid: uuid.clone(),
            http_host: config.http_host.clone(),
            rtsp_host: config.rtsp_host.clone(),
            http_port: opts.http_port as i32,
            rtsp_port: opts.rtsp_port,
            xml_path: opts.xml_path.clone(),
            playlist: opts.playlist.clone(),
            bootid,
            device_id,
            force_sadapter: opts.force_sadapter,
            force_tadapter: opts.force_tadapter,
            force_cadapter: opts.force_cadapter,
        })
        .with_identity(identity);
        session.set_web(web);
        session.set_web_runtime(RuntimeState {
            datetime_compile: env!("MINISATIP_DATETIME_COMPILE").to_owned(),
            datetime_start: sys::asctime_now(),
            datetime_current: sys::asctime_now(),
            time_running: "00:00:00".to_owned(),
            run_user: sys::run_user(),
            run_pid: i32::try_from(std::process::id()).unwrap_or(0),
            command_line: opts.command_line.clone(),
        });

        #[cfg(feature = "srt")]
        if let Some(listener) = start_srt_listener(&config) {
            session.set_srt_listener(listener);
        }

        Ok(Self {
            config,
            session,
            handles,
            outputs: HashMap::new(),
            conn_streams: HashMap::new(),
            started: Instant::now(),
            start_unix: sys::now_seconds(),
        })
    }

    fn run(&mut self, receiver: &Receiver<Inbound>, shutdown: &AtomicBool) {
        while !shutdown.load(Ordering::SeqCst) && !sys::shutdown_requested() {
            match receiver.recv_timeout(LOOP_TICK) {
                Ok(Inbound::Request(message)) => self.handle_request(*message),
                Ok(Inbound::Interleaved(conn_id)) => {
                    if let Some(id) = self.conn_streams.get(&conn_id).copied() {
                        self.session.touch_stream(id);
                    }
                }
                Ok(Inbound::Closed(conn_id)) => self.close_connection(conn_id),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            self.expire_streams();
            self.drain_events();
        }
    }

    /// Closes streams whose idle timeout expired (`stream_timeout`).
    fn expire_streams(&mut self) {
        let expired = self.session.expired_streams();
        if expired.is_empty() {
            return;
        }
        for id in expired {
            self.stop_output(id);
            self.session.close_stream(id);
            self.conn_streams.retain(|_, sid| *sid != id);
        }
        self.update_thresholds();
    }

    fn handle_request(&mut self, message: ConnectionRequest) {
        let ConnectionRequest {
            conn_id,
            protocol,
            peer,
            local_ip,
            mut writer,
            request,
            reads,
        } = message;
        let before = self.conn_streams.get(&conn_id).copied();
        if request.is_method("PLAY")
            && let Some(id) = before
        {
            self.refresh_sequence(id);
        }
        if !protocol.is_rtsp() {
            let running = sys::now_seconds() - self.start_unix;
            self.session
                .set_web_clock(sys::asctime_now(), sys::elapsed_string(running));
        }
        // describe_streams prints the socket's local address (stream.cpp:97-105).
        self.session.describe_options_mut().local_host = local_ip.clone();

        let (response, sid) = {
            let mut ctx = HandlerContext::new(&mut self.session, protocol);
            ctx.sid = before;
            ctx.peer_host = peer.ip().to_string();
            ctx.peer_port = peer.port();
            ctx.local_host = local_ip;
            ctx.timeout_ms = self.config.timeout_ms;
            ctx.start_rtp = self.config.start_rtp;
            ctx.rtsp_port = self.config.rtsp_port;
            ctx.rtptime_seconds = self.started.elapsed().as_secs() as i64;
            let response = if protocol.is_rtsp() {
                handle_rtsp(&mut ctx, &request)
            } else {
                handle_http(&mut ctx, &request)
            };
            (response, ctx.sid)
        };

        // Interleaved replies take the socket's priority lane
        // (minisatip.cpp:2177) so they never split a `$` frame.
        let priority = before.and_then(|id| {
            self.outputs
                .get(&id)
                .filter(|active| {
                    active.conn_id == conn_id && active.stream_type == StreamType::RtspTcp
                })
                .map(|active| (id, active.adapter))
        });
        match sid {
            Some(id) => {
                self.conn_streams.insert(conn_id, id);
            }
            None => {
                self.conn_streams.remove(&conn_id);
            }
        }
        self.prune_outputs();

        // A 200 to a repeated HTTP GET is hidden (minisatip.cpp:2096-2102).
        let hidden = protocol == Protocol::Http
            && reads > 1
            && response
                .as_ref()
                .is_some_and(|response| response.status == 200);
        if let Some(response) = response.filter(|_| !hidden) {
            let bytes = response.to_bytes();
            let queued = priority.is_some_and(|(stream_id, adapter)| {
                self.handles
                    .get(adapter as usize)
                    .and_then(Option::as_ref)
                    .is_some_and(|handle| {
                        handle.send(Command::WritePriority {
                            stream_id,
                            data: bytes.clone(),
                        })
                    })
            });
            if !queued && (writer.write_all(&bytes).is_err() || writer.flush().is_err()) {
                return;
            }
        }
        // read_http closes every connection that is not a `GET /?` stream
        // (minisatip.cpp:1520-1525).
        if protocol == Protocol::Http
            && !(request.method.eq_ignore_ascii_case("GET") && request.uri.contains("/?"))
        {
            let _ = writer.shutdown(std::net::Shutdown::Both);
        }

        let Some(id) = sid else {
            return;
        };
        if self.session.stream(id).is_none() {
            self.stop_output(id);
            if self.conn_streams.get(&conn_id) == Some(&id) {
                self.conn_streams.remove(&conn_id);
            }
            return;
        }
        self.sync_output(id, conn_id, peer, writer);
        self.update_thresholds();
    }

    /// Copies the live RTP sequence into the session for `RTP-Info`
    /// (minisatip.cpp:1416-1418).
    fn refresh_sequence(&mut self, id: i32) {
        let Some(active) = self.outputs.get(&id) else {
            return;
        };
        let Some(handle) = self
            .handles
            .get(active.adapter as usize)
            .and_then(Option::as_ref)
        else {
            return;
        };
        if let Some(stats) = handle.query(id) {
            self.session.set_rtp_seq(id, stats.seq);
        }
    }

    /// Reconciles a live output with a re-SETUP/PLAY (`stream.cpp:513-556`):
    /// a new client port or transport replaces the RTP socket, an unchanged
    /// one keeps streaming.
    fn sync_output(&mut self, id: i32, conn_id: u64, peer: SocketAddr, writer: TcpStream) {
        let Some(stream) = self.session.stream(id) else {
            return;
        };
        let (stream_type, adapter, do_play, ssrc) = (
            stream.stream_type,
            stream.adapter,
            stream.do_play,
            stream.session_id as u32,
        );
        let dest = udp_destination(stream);
        let Some(active) = self
            .outputs
            .get(&id)
            .map(|active| (active.adapter, active.stream_type, active.dest, active.ssrc))
        else {
            if do_play {
                self.start_output(id, conn_id, peer, writer);
            }
            return;
        };
        let (active_adapter, active_type, active_dest, active_ssrc) = active;
        if active_adapter != adapter || active_type != stream_type {
            self.stop_output(id);
            if adapter >= 0 {
                self.start_output(id, conn_id, peer, writer);
            }
            return;
        }
        if let Some(handle) = self.handles.get(adapter as usize).and_then(Option::as_ref) {
            if let Some(dest) = dest
                && active_dest != Some(dest)
            {
                handle.send(Command::SetDestination {
                    stream_id: id,
                    dest,
                });
            }
            // A re-SETUP Session header changes the SSRC of the live output
            // (stream.cpp:1409-1416).
            if active_ssrc != ssrc {
                handle.send(Command::SetSsrc {
                    stream_id: id,
                    ssrc,
                });
                if let Some(active) = self.outputs.get_mut(&id) {
                    active.ssrc = ssrc;
                }
            }
        }
        self.sync_pids(id);
    }

    fn start_output(&mut self, id: i32, conn_id: u64, peer: SocketAddr, writer: TcpStream) {
        let Some(stream) = self.session.stream(id) else {
            return;
        };
        let (stream_type, adapter, ssrc, seq, server_port, remote_host, remote_port, srt_sock) = (
            stream.stream_type,
            stream.adapter,
            stream.session_id as u32,
            stream.rtp_seq,
            stream.server_port,
            stream.remote_host.clone(),
            stream.remote_port,
            stream.srt_sock,
        );
        if adapter < 0 {
            return;
        }
        let Some(handle) = self.handles.get(adapter as usize).and_then(Option::as_ref) else {
            return;
        };
        #[cfg(not(feature = "srt"))]
        let _ = srt_sock;
        let config = OutputConfig {
            ssrc,
            seq,
            fifo_size: self.config.fifo_size,
        };
        let now_ms = self.started.elapsed().as_millis() as i64;
        let dest = match stream_type {
            StreamType::RtspUdp => match remote_host.parse::<IpAddr>() {
                Ok(destination) => Some(SocketAddr::new(destination, remote_port)),
                Err(_) => {
                    crate::sys::log_error(format_args!(
                        "minisatip: invalid RTP destination {remote_host}"
                    ));
                    return;
                }
            },
            _ => None,
        };
        let output = match stream_type {
            StreamType::RtspUdp => {
                let dest = dest.expect("parsed above");
                // decode_transport binds `bind_dev_ip()` or the wildcard of
                // the destination family, not `--bind`
                // (stream.cpp:556-561, socketworks.cpp:333-341).
                let local_ip = if self.config.bind_dev.is_some() {
                    self.config.bind_ip
                } else if dest.is_ipv4() {
                    IpAddr::V4(Ipv4Addr::UNSPECIFIED)
                } else {
                    IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED)
                };
                let target = UdpTarget {
                    local: SocketAddr::new(local_ip, server_port),
                    dest,
                    ttl: None,
                };
                StreamOutput::udp_with_options(
                    target,
                    config,
                    now_ms,
                    self.config.bind_dev.as_deref(),
                    self.config.output_buffer,
                )
            }
            StreamType::RtspTcp => Ok(StreamOutput::rtsp_tcp(
                Box::new(writer),
                Some(peer),
                config,
                now_ms,
            )),
            StreamType::Http => Ok(StreamOutput::http(
                Box::new(writer),
                Some(peer),
                config,
                now_ms,
            )),
            #[cfg(feature = "srt")]
            StreamType::RtspSrt => {
                if srt_sock == ms_net::srt::SRT_INVALID_SOCK {
                    return;
                }
                let Some(listener) = self.session.srt_listener() else {
                    return;
                };
                // flush_stream sends through the same library handle as the listener.
                let ops: Arc<dyn ms_net::srt::SrtOps> = listener.ops().clone();
                let remote = remote_host
                    .parse::<IpAddr>()
                    .ok()
                    .map(|ip| SocketAddr::new(ip, remote_port));
                Ok(StreamOutput::srt(srt_sock, ops, remote, config, now_ms))
            }
            #[cfg(not(feature = "srt"))]
            StreamType::RtspSrt => return,
            StreamType::None => return,
        };
        match output {
            Ok(output) => {
                let pids = self.stream_pids(id, adapter);
                let setup_age_ms = self.session.setup_age_ms(id).unwrap_or(0);
                if let Some(epoch) = handle.start_output_with_age(id, output, pids, setup_age_ms) {
                    self.outputs.insert(
                        id,
                        ActiveOutput {
                            conn_id,
                            adapter,
                            epoch,
                            stream_type,
                            dest,
                            ssrc,
                        },
                    );
                }
            }
            Err(error) => {
                crate::sys::log_error(format_args!("minisatip: cannot start stream {id}: {error}"))
            }
        }
        self.update_thresholds();
    }

    fn sync_pids(&self, id: i32) {
        let Some(active) = self.outputs.get(&id) else {
            return;
        };
        if let Some(handle) = self
            .handles
            .get(active.adapter as usize)
            .and_then(Option::as_ref)
        {
            handle.set_pids(id, self.stream_pids(id, active.adapter));
        }
    }

    fn stream_pids(&self, id: i32, adapter: i32) -> PidSet {
        let mut set = PidSet::new();
        for pid in self.session.scheduler().stream_pids(adapter, id) {
            if let Ok(pid) = u16::try_from(pid) {
                set.insert(pid);
            }
        }
        set
    }

    /// Stops outputs whose stream was closed behind the runtime's back
    /// (`close_stream`, stream.cpp:351-379).
    fn prune_outputs(&mut self) {
        let dead: Vec<i32> = self
            .outputs
            .keys()
            .copied()
            .filter(|id| self.session.stream(*id).is_none())
            .collect();
        for id in dead {
            self.stop_output(id);
            self.conn_streams.retain(|_, sid| *sid != id);
        }
    }

    fn stop_output(&mut self, id: i32) {
        if let Some(active) = self.outputs.remove(&id)
            && let Some(handle) = self
                .handles
                .get(active.adapter as usize)
                .and_then(Option::as_ref)
        {
            handle.stop_output(id);
        }
    }

    fn close_connection(&mut self, conn_id: u64) {
        let streams: Vec<i32> = self
            .outputs
            .iter()
            .filter(|(_, active)| active.conn_id == conn_id)
            .map(|(id, _)| *id)
            .collect();
        for id in streams {
            self.close_control_stream(id);
        }
        if let Some(id) = self.conn_streams.remove(&conn_id) {
            self.close_control_stream(id);
        }
        self.update_thresholds();
    }

    /// Applies `close_http` (minisatip.cpp:1678): RTSP/UDP and undecoded
    /// streams with a timeout survive their control connection.
    fn close_control_stream(&mut self, id: i32) {
        if !self.session.closes_on_control_close(id) {
            return;
        }
        self.stop_output(id);
        self.session.close_stream(id);
    }

    fn drain_events(&mut self) {
        for (aid, handle) in self.handles.iter().enumerate() {
            let Some(handle) = handle else {
                continue;
            };
            while let Ok(event) = handle.events().try_recv() {
                match event {
                    // Only the tracked generation may close the stream; a
                    // stale event belongs to a restarted output.
                    ms_net::dataplane::Event::OutputClosed(id, epoch) => {
                        if self.outputs.get(&id).map(|active| active.epoch) == Some(epoch) {
                            self.outputs.remove(&id);
                            self.session.close_stream(id);
                        }
                    }
                    // StopOutput is always requested here, and the caller
                    // decides whether the stream lives on (stream.cpp:513-556).
                    ms_net::dataplane::Event::OutputStopped(..) => {}
                    ms_net::dataplane::Event::TunerError(error) => {
                        crate::sys::log_error(format_args!("minisatip: adapter {aid}: {error}"));
                    }
                    ms_net::dataplane::Event::RtcpReceived(id) => {
                        // rtcp_confirm refreshes the idle timer (stream.cpp:1448).
                        self.session.touch_stream(id);
                    }
                    ms_net::dataplane::Event::ContinuityErrors(errors) => {
                        self.session.set_adapter_ccerr(aid, errors);
                    }
                }
            }
        }
    }

    fn update_thresholds(&self) {
        let any_live_udp = self
            .outputs
            .values()
            .any(|active| active.stream_type == StreamType::RtspUdp);
        for (aid, handle) in self.handles.iter().enumerate() {
            let Some(handle) = handle else {
                continue;
            };
            let Some(adapter) = self.session.scheduler().adapter(aid) else {
                continue;
            };
            let master_type = self
                .session
                .stream(adapter.master_sid)
                .map(|stream| stream.stream_type);
            let threshold = ms_net::dataplane::adapter_threshold(
                self.config.udp_threshold,
                self.config.tcp_threshold,
                adapter.sid_cnt.max(0) as usize,
                master_type,
                any_live_udp,
            );
            handle.send(Command::SetThreshold(threshold));
        }
    }

    fn shutdown(&mut self) {
        let ids: Vec<i32> = self.outputs.keys().copied().collect();
        for id in ids {
            self.stop_output(id);
            self.session.close_stream(id);
        }
        for id in 0..ms_net::handler::MAX_STREAMS {
            self.session.close_stream(id);
        }
        for handle in self.handles.drain(..).flatten() {
            handle.stop();
        }
        #[cfg(feature = "srt")]
        {
            let had_listener = self.session.srt_listener().is_some();
            if let Some(mut listener) = self.session.take_srt_listener() {
                listener.close();
            }
            if had_listener {
                ms_net::srt::LibSrtOps::cleanup();
            }
        }
    }
}

/// Resolves `--bind`/`--bind-http` like `getaddrinfo` (socketworks.cpp:55-98).
fn resolve_bind_address(host: Option<&str>) -> Result<Option<IpAddr>, String> {
    let Some(host) = host.filter(|host| !host.is_empty()) else {
        return Ok(None);
    };
    if let Ok(address) = host.parse() {
        return Ok(Some(address));
    }
    (host, 0)
        .to_socket_addrs()
        .map_err(|error| format!("cannot resolve {host}: {error}"))?
        .next()
        .map(|address| Some(address.ip()))
        .ok_or_else(|| format!("cannot resolve {host}"))
}

/// Runs the server until SIGINT/SIGTERM.
pub fn run(opts: &Options) -> Result<(), String> {
    sys::install_shutdown_handlers();

    // Ports come from atoi and reach htons()/getaddrinfo as 16-bit values, so
    // out-of-range numbers wrap instead of being rejected
    // (minisatip.cpp:965, socketworks.cpp:55-103). Advertising uses the
    // configured values verbatim, including 0 (minisatip.cpp:1130-1136).
    let rtsp_port = port16(opts.rtsp_port);
    let http_port = port16(opts.http_port as i32);
    let start_rtp = port16(opts.start_rtp as i32);
    // resolve_bind_opts: --bind-dev sets both addresses, and hostnames are
    // resolved through getaddrinfo (socketworks.cpp:138-168).
    let mut binds = BindOptions::new(
        opts.bind.as_deref(),
        opts.bind_http.as_deref(),
        opts.bind_dev.as_deref(),
    );
    resolve_bind_opts(&mut binds, &ms_net::sys::SystemDevices)
        .map_err(|error| format!("bind options: {error}"))?;
    // fill_sockaddr binds the IPv6 wildcard unless `-4`/--ipv4-only is in
    // effect (socketworks.cpp:96-102).
    let wildcard = if opts.use_ipv4_only != 0 {
        IpAddr::V4(Ipv4Addr::UNSPECIFIED)
    } else {
        IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED)
    };
    let bind_ip = resolve_bind_address(binds.bind.as_deref())?.unwrap_or(wildcard);
    let http_bind_ip = resolve_bind_address(binds.bind_http.as_deref())?.unwrap_or(bind_ip);
    // set_options: `lip` is the resolved `--bind` or getlocalip()
    // (minisatip.cpp:1125-1128).
    let advertised_host = if binds.bind.is_some() {
        bind_ip.to_string()
    } else {
        local_ip(&opts.disc_host, opts.use_ipv4_only != 0)
    };

    // tcp_listen applies SO_BINDTODEVICE before bind and fails hard when the
    // device cannot be selected (socketworks.cpp:469-505).
    let bind_dev = opts.bind_dev.clone();
    let rtsp_listener =
        ms_net::sys::tcp_listen(SocketAddr::new(bind_ip, rtsp_port), bind_dev.as_deref())
            .map_err(|error| format!("cannot bind RTSP port {rtsp_port}: {error}"))?;
    let http_listener = ms_net::sys::tcp_listen(
        SocketAddr::new(http_bind_ip, http_port),
        bind_dev.as_deref(),
    )
    .map_err(|error| format!("cannot bind HTTP port {http_port}: {error}"))?;

    let (mut uuid, mut bootid, mut device_id) =
        (opts.uuid.clone(), opts.bootid.max(0), opts.device_id.max(0));
    let cache_dir = PathBuf::from(&opts.cache_dir);
    // minisatip.cpp:1934-1937 creates the cache dir and exits on failure.
    std::fs::create_dir_all(&cache_dir)
        .map_err(|error| format!("cannot create {}: {error}", cache_dir.display()))?;
    if let Ok((stored_bootid, stored_device_id)) = read_boot_id(&cache_dir, device_id, &mut uuid) {
        bootid = stored_bootid;
        device_id = stored_device_id;
    }
    if uuid.is_empty() {
        uuid = uuid4();
    }
    if bootid < 1 {
        bootid = 1;
    }
    if device_id < 1 {
        device_id = 1;
    }

    // minisatip.cpp:1130-1136: `-w` overrides http_host; rtsp_host always has
    // the port, printed from the configured (pre-htons) value.
    let http_host = opts
        .http_host
        .clone()
        .unwrap_or_else(|| format!("{advertised_host}:{}", opts.http_port));
    let config = ServerConfig {
        bind_ip,
        advertised_host: advertised_host.clone(),
        http_host: http_host.clone(),
        rtsp_host: format!("{advertised_host}:{}", opts.rtsp_port),
        start_rtp,
        rtsp_port,
        timeout_ms: u32::try_from(opts.timeout_sec).unwrap_or(30_000),
        udp_threshold: opts.udp_threshold,
        tcp_threshold: opts.tcp_threshold,
        fifo_size: fifo_size(opts.max_sbuf),
        rrtp: opts.rrtp.clone(),
        bind_dev: opts.bind_dev.clone(),
        output_buffer: opts.output_buffer,
    };
    // C++ keeps these in `opts`: the SSDP thread and HTTP endpoints see the
    // same values, including a DEVICEID changed by conflict handling.
    let identity = SharedIdentity::new(uuid.clone(), device_id, bootid);
    let mut runtime = Runtime::new(
        opts,
        config,
        uuid.clone(),
        bootid,
        device_id,
        identity.clone(),
    )?;

    let (requests, receiver) = mpsc::channel();
    let shutdown = Arc::new(AtomicBool::new(false));
    let next_id = Arc::new(AtomicU64::new(1));
    let listeners = vec![
        spawn_listener(
            rtsp_listener,
            Protocol::Rtsp,
            "rtsp",
            requests.clone(),
            Arc::clone(&shutdown),
            Arc::clone(&next_id),
        )?,
        spawn_listener(
            http_listener,
            Protocol::Http,
            "http",
            requests.clone(),
            Arc::clone(&shutdown),
            Arc::clone(&next_id),
        )?,
    ];

    let ssdp = if opts.disable_ssdp {
        None
    } else {
        let ssdp_config = SsdpConfig {
            uuid: uuid.clone(),
            device_id,
            bootid,
            name_app: opts.name_app.clone(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            http_host,
            xml_path: opts.xml_path.clone(),
            disc_host: opts.disc_host.clone(),
            local_ip: advertised_host.clone(),
            cache_dir: Some(cache_dir.clone()),
            disabled: false,
        };
        // Both SSDP sockets must bind or C++ exits (minisatip.cpp:1943-1947).
        let bind_dev_ip = if opts.bind_dev.is_some() {
            match resolve_bind_address(binds.bind.as_deref())? {
                Some(IpAddr::V4(address)) => Some(address),
                Some(IpAddr::V6(_)) => {
                    return Err("SSDP: --bind-dev requires an IPv4 address".to_owned());
                }
                None => None,
            }
        } else {
            None
        };
        let io = UdpSsdpIo::open(
            opts.bind.as_deref(),
            &opts.disc_host,
            opts.use_ipv4_only != 0,
            bind_dev.as_deref(),
            bind_dev_ip,
        )
        .map_err(|error| format!("SSDP: {error}"))?;
        Some(spawn_ssdp(ssdp_config, identity, io, Arc::clone(&shutdown)))
    };

    // minisatip.cpp:2018 writes the pid after initialisation; failures are
    // ignored (write_pid_file only checks fopen).
    sys::write_pid_file();
    runtime.run(&receiver, &shutdown);
    sys::remove_pid_file();

    shutdown.store(true, Ordering::SeqCst);
    drop(requests);
    for listener in listeners {
        let _ = listener.join();
    }
    if let Some(ssdp) = ssdp {
        let _ = ssdp.join();
    }
    runtime.shutdown();
    Ok(())
}

/// getlocalip (socketworks.cpp:223-248): the source address of a UDP socket
/// connected to `disc_host:1900`, or empty when there is no route.
fn local_ip(disc_host: &str, ipv4_only: bool) -> String {
    let Some(target) = (disc_host, 1900)
        .to_socket_addrs()
        .ok()
        .and_then(|mut addrs| addrs.find(|addr| !ipv4_only || addr.is_ipv4()))
    else {
        return String::new();
    };
    let any: SocketAddr = if target.is_ipv4() {
        (Ipv4Addr::UNSPECIFIED, 0).into()
    } else {
        (std::net::Ipv6Addr::UNSPECIFIED, 0).into()
    };
    UdpSocket::bind(any)
        .and_then(|socket| {
            socket.connect(target)?;
            socket.local_addr()
        })
        .map(|addr| addr.ip().to_string())
        .unwrap_or_default()
}

fn udp_destination(stream: &ms_net::session::Stream) -> Option<SocketAddr> {
    if stream.stream_type != StreamType::RtspUdp {
        return None;
    }
    let ip = stream.remote_host.parse::<IpAddr>().ok()?;
    Some(SocketAddr::new(ip, stream.remote_port))
}

fn fifo_size(max_sbuf: i32) -> usize {
    usize::try_from(max_sbuf)
        .ok()
        .filter(|megabytes| *megabytes > 0)
        .map(|megabytes| megabytes.saturating_mul(1024 * 1024))
        .unwrap_or(DEFAULT_FIFO_SIZE)
}

/// Truncates an `atoi` port to 16 bits, as `htons` does
/// (socketworks.cpp:55-103). The configured value is still what gets
/// advertised (minisatip.cpp:1130-1136).
fn port16(value: i32) -> u16 {
    value as u16
}

fn adapter_ids(opts: &Options) -> Vec<usize> {
    // `enable_adapters` disables every slot before applying the selection, so
    // `-e ""` leaves no adapters at all (adapter.cpp:1626-1646).
    let Some(selected) = opts.enabled_adapters.as_deref() else {
        return vec![0];
    };
    selected
        .iter()
        .map(|id| usize::from(*id))
        .filter(|id| *id < MAX_ADAPTERS)
        .collect()
}

fn adapter_config() -> AdapterConfig {
    let mut config = AdapterConfig {
        enabled: true,
        ..AdapterConfig::default()
    };
    // A file replay source stands in for a DVB-S2 frontend, whose Linux
    // capabilities are SYS_DVBS and SYS_DVBS2 (adapter.cpp:283-371 only
    // advertises the systems the frontend reports).
    config.sys[0] = DeliverySystem::DvbS2;
    config.sys[1] = DeliverySystem::DvbS;
    config
}

fn spawn_listener(
    listener: TcpListener,
    protocol: Protocol,
    name: &str,
    requests: Sender<Inbound>,
    shutdown: Arc<AtomicBool>,
    next_id: Arc<AtomicU64>,
) -> Result<JoinHandle<()>, String> {
    listener
        .set_nonblocking(true)
        .map_err(|error| format!("cannot configure {name} listener: {error}"))?;
    thread::Builder::new()
        .name(format!("{name}-accept"))
        .spawn(move || accept_loop(listener, protocol, requests, shutdown, next_id))
        .map_err(|error| format!("cannot start {name} listener: {error}"))
}

fn accept_loop(
    listener: TcpListener,
    protocol: Protocol,
    requests: Sender<Inbound>,
    shutdown: Arc<AtomicBool>,
    next_id: Arc<AtomicU64>,
) {
    while !shutdown.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _peer)) => {
                let conn_id = next_id.fetch_add(1, Ordering::SeqCst);
                let requests = requests.clone();
                let shutdown = Arc::clone(&shutdown);
                let spawn = thread::Builder::new()
                    .name(format!("conn{conn_id}"))
                    .spawn(move || serve_connection(stream, protocol, conn_id, requests, shutdown));
                if spawn.is_err() {
                    crate::sys::log_error(format_args!(
                        "minisatip: cannot start a connection thread"
                    ));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => thread::sleep(ACCEPT_POLL),
            Err(_) => break,
        }
    }
}

fn serve_connection(
    mut stream: TcpStream,
    protocol: Protocol,
    conn_id: u64,
    requests: Sender<Inbound>,
    shutdown: Arc<AtomicBool>,
) {
    let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
    let _ = stream.set_write_timeout(Some(WRITE_TIMEOUT));
    let local_ip = stream
        .local_addr()
        .map(|address| address.ip().to_string())
        .unwrap_or_else(|_| "127.0.0.1".to_owned());
    let peer = match stream.peer_addr() {
        Ok(peer) => peer,
        Err(_) => return,
    };
    let mut buffer = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    let mut reads = 0u64;
    while !shutdown.load(Ordering::SeqCst) {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => {
                reads += 1;
                buffer.extend_from_slice(&chunk[..read]);
                loop {
                    match parse_request_with(&buffer, protocol == Protocol::Http) {
                        ParseOutcome::Complete { request, consumed } => {
                            buffer.drain(..consumed);
                            let Ok(writer) = stream.try_clone() else {
                                return;
                            };
                            let message = ConnectionRequest {
                                conn_id,
                                protocol,
                                peer,
                                local_ip: local_ip.clone(),
                                writer,
                                request,
                                reads,
                            };
                            if requests.send(Inbound::Request(Box::new(message))).is_err() {
                                return;
                            }
                        }
                        ParseOutcome::Incomplete { consumed }
                        | ParseOutcome::Ignored { consumed } => {
                            if consumed > 0 {
                                buffer.drain(..consumed);
                                let _ = requests.send(Inbound::Interleaved(conn_id));
                            }
                            break;
                        }
                        ParseOutcome::Discarded | ParseOutcome::Malformed => {
                            buffer.clear();
                            break;
                        }
                    }
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) =>
            {
                continue;
            }
            Err(_) => break,
        }
    }
    let _ = requests.send(Inbound::Closed(conn_id));
}

fn spawn_ssdp(
    config: SsdpConfig,
    identity: SharedIdentity,
    io: UdpSsdpIo,
    shutdown: Arc<AtomicBool>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("ssdp".to_owned())
        .spawn(move || ssdp_loop(config, identity, io, shutdown))
        .expect("spawn SSDP thread")
}

fn ssdp_loop(
    config: SsdpConfig,
    identity: SharedIdentity,
    io: UdpSsdpIo,
    shutdown: Arc<AtomicBool>,
) {
    let _ = io.set_read_timeout(Some(SSDP_READ_TIMEOUT));
    let mut server = SsdpServer::new(io, config).with_identity(identity);
    let origin = Instant::now();
    let mut next_announce = 0i64;
    let mut buffer = [0u8; 4096];
    while !shutdown.load(Ordering::SeqCst) {
        let now = origin.elapsed().as_millis() as i64;
        if now >= next_announce {
            let _ = server.announce(now);
            next_announce = now + ANNOUNCE_INTERVAL_MS;
        }
        match server.io().recv_from(&mut buffer) {
            Ok((length, peer)) => {
                let date = format_http_date(SystemTime::now());
                let _ = server.handle(
                    &buffer[..length],
                    &peer.ip().to_string(),
                    peer.port(),
                    &date,
                    now,
                );
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) => {}
            Err(_) => break,
        }
    }
    let _ = server.byebye();
}

#[cfg(feature = "srt")]
fn start_srt_listener(
    config: &ServerConfig,
) -> Option<ms_net::srt::SrtListener<ms_net::srt::LibSrtOps>> {
    use std::os::fd::IntoRawFd;

    if !ms_net::srt::LibSrtOps::startup() {
        crate::sys::log_error(format_args!(
            "minisatip: SRT disabled: {}",
            ms_net::srt::LibSrtOps::last_error()
        ));
        return None;
    }
    // srt_listener_init uses udp_bind, so the same SO_BINDTODEVICE and reuse
    // flags apply (srt.cpp:139-145, socketworks.cpp:250-322).
    let socket = match ms_net::sys::udp_bind_reuse(
        SocketAddr::new(config.bind_ip, config.rtsp_port),
        config.bind_dev.as_deref(),
    ) {
        Ok(socket) => socket,
        Err(error) => {
            crate::sys::log_error(format_args!(
                "minisatip: SRT disabled: UDP bind failed: {error}"
            ));
            ms_net::srt::LibSrtOps::cleanup();
            return None;
        }
    };
    let mut listener = ms_net::srt::SrtListener::new(Arc::new(ms_net::srt::LibSrtOps));
    if let Err(error) = listener.init(socket.into_raw_fd()) {
        crate::sys::log_error(format_args!("minisatip: SRT disabled: {error}"));
        ms_net::srt::LibSrtOps::cleanup();
        return None;
    }
    Some(listener)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atoi_ports_are_truncated_like_htons() {
        // socketworks.cpp:55-103 stores the port in a 16-bit sockaddr field;
        // minisatip.cpp:965 accepts any atoi result.
        assert_eq!(port16(554), 554);
        assert_eq!(port16(0), 0);
        assert_eq!(port16(70_000), 4_464);
        assert_eq!(port16(-1), 65_535);
    }

    #[test]
    fn datetime_compile_has_the_cpp_date_and_time_format() {
        // minisatip.cpp:1149-1150: `sprintf("%s | %s", __DATE__, __TIME__)`;
        // __DATE__ is "Mmm dd yyyy" and __TIME__ is "hh:mm:ss".
        let stamp = env!("MINISATIP_DATETIME_COMPILE");
        let (date, time) = stamp.split_once(" | ").expect("date | time");
        let parts: Vec<&str> = date.split(' ').collect();
        assert_eq!(parts.len(), 3, "{stamp}");
        assert!(
            [
                "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"
            ]
            .contains(&parts[0]),
            "{stamp}"
        );
        assert!((1..=31).contains(&parts[1].trim().parse::<u32>().unwrap()));
        assert!(parts[2].len() == 4);
        let clock: Vec<&str> = time.split(':').collect();
        assert_eq!(clock.len(), 3);
        assert!(clock.iter().all(|field| field.len() == 2));
    }

    #[test]
    fn local_ip_is_the_route_source_address() {
        // getlocalip connects a UDP socket and reads getsockname (socketworks.cpp:239-244).
        assert_eq!(local_ip("127.0.0.1", true), "127.0.0.1");
        assert_eq!(local_ip("not a host", true), "");
    }
}
