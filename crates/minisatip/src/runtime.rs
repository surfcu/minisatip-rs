//! Server runtime: listeners, the RTSP/HTTP control loop and adapter data planes.
//!
//! The control plane is a single thread driving [`SessionManager`]; every
//! accepted connection has a reader thread that parses requests and forwards
//! them here. Adapters run on their own data-plane threads.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use ms_config::Options;
use ms_net::dataplane::{AdapterHandle, Command, DataPlaneConfig};
use ms_net::handler::{HandlerContext, StreamType, handle_http, handle_rtsp};
use ms_net::output::{OutputConfig, StreamOutput, UdpTarget};
use ms_net::response::format_http_date;
use ms_net::rtsp::{ParseOutcome, Protocol, Request, parse_request};
use ms_net::session::{DescribeOptions, SessionManager};
use ms_net::socket::DEFAULT_FIFO_SIZE;
use ms_net::ssdp::{ANNOUNCE_INTERVAL_MS, SsdpConfig, SsdpServer, UdpSsdpIo, read_boot_id, uuid4};
use ms_net::web::{RuntimeState, WebConfig, WebService};
use ms_sched::{AdapterConfig, AdapterTuner, MAX_ADAPTERS, Scheduler, TuneError};
use ms_tuner::satipc::{
    MAX_DVBAPI_SYSTEMS, SatipServer, SatipcConfig, SatipcTuner, delsys_code, fetch_satip_xml,
    parse_satip_servers, parse_satip_xml,
};
use ms_tuner::{
    DeliverySystem, FileReplayConfig, FileReplayTuner, Pacing, PidSet, TuneParams, Tuner,
};

use crate::sys;

const READ_TIMEOUT: Duration = Duration::from_millis(100);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const LOOP_TICK: Duration = Duration::from_millis(20);
const ACCEPT_POLL: Duration = Duration::from_millis(20);
const SSDP_READ_TIMEOUT: Duration = Duration::from_millis(200);

/// Fixed runtime values shared by every request.
struct ServerConfig {
    bind_ip: IpAddr,
    advertised_host: String,
    start_rtp: u16,
    rtsp_port: u16,
    http_port: u16,
    timeout_ms: u32,
    udp_threshold: i32,
    tcp_threshold: i32,
    fifo_size: usize,
    rrtp: Option<String>,
}

/// An RTSP connection request forwarded by a reader thread.
struct ConnectionRequest {
    conn_id: u64,
    protocol: Protocol,
    peer: SocketAddr,
    local_ip: String,
    writer: TcpStream,
    request: Request,
}

enum Inbound {
    Request(Box<ConnectionRequest>),
    Closed(u64),
}

/// Outputs started by PLAY/GET, tracked so they can be stopped.
struct ActiveOutput {
    conn_id: u64,
    adapter: i32,
    stream_type: StreamType,
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
}

impl Runtime {
    fn new(
        opts: &Options,
        config: ServerConfig,
        uuid: String,
        bootid: i32,
        device_id: i32,
    ) -> Result<Self, String> {
        let mut scheduler = Scheduler::new();
        let replay = std::env::var_os("MINISATIP_REPLAY").map(PathBuf::from);
        let adapter_ids = adapter_ids(opts);
        let mut handles: Vec<Option<AdapterHandle>> = (0..MAX_ADAPTERS).map(|_| None).collect();

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
                let handle = spawn_adapter(opts, aid, Box::new(tuner))?;
                let bridge = DataPlaneTuner {
                    commands: handle.command_sender(),
                };
                scheduler.add_adapter(aid, adapter_config(), Box::new(bridge));
                handles[aid] = Some(handle);
            }
        }

        // find_satip_adapter runs after the local adapters (find_adapters).
        for server in satip_servers(opts)? {
            let Some(aid) = (0..MAX_ADAPTERS)
                .find(|aid| handles[*aid].is_none() && !adapter_disabled(opts, *aid))
            else {
                eprintln!("minisatip: MAX_ADAPTERS reached");
                break;
            };
            let config = SatipcConfig {
                adapter_id: aid,
                server: server.clone(),
                start_rtp: config.start_rtp,
                user_agent: format!("minisatip {}", env!("CARGO_PKG_VERSION")),
                adapter_timeout_ms: i64::from(opts.adapter_timeout),
                ignore_signal: opts.strength_multiplier == 0.0 && opts.snr_multiplier == 0.0,
            };
            if !server.source_ip.is_empty() {
                eprintln!(
                    "minisatip: AD{aid}: source address {} is not supported, using the default route",
                    server.source_ip
                );
            }
            let tuner = SatipcTuner::new(&config)
                .map_err(|error| format!("cannot start satip adapter {aid}: {error}"))?;
            let handle = spawn_adapter(opts, aid, Box::new(tuner))?;
            let bridge = DataPlaneTuner {
                commands: handle.command_sender(),
            };
            scheduler.add_adapter(aid, satipc_adapter_config(&server), Box::new(bridge));
            handles[aid] = Some(handle);
        }

        let mut session = SessionManager::with_scheduler(scheduler);
        session.set_start_rtp(config.start_rtp);
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
            http_host: opts
                .http_host
                .clone()
                .unwrap_or_else(|| format!("{}:{}", config.advertised_host, config.http_port)),
            rtsp_host: opts
                .rtsp_host
                .clone()
                .unwrap_or_else(|| config.advertised_host.clone()),
            http_port: config.http_port,
            rtsp_port: config.rtsp_port,
            xml_path: opts.xml_path.clone(),
            playlist: opts.playlist.clone(),
            bootid,
            device_id,
        });
        session.set_web(web);
        session.set_web_runtime(RuntimeState {
            datetime_compile: String::new(),
            datetime_start: format_http_date(SystemTime::now()),
            datetime_current: format_http_date(SystemTime::now()),
            time_running: "0".to_owned(),
            run_user: 0,
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
        })
    }

    fn run(&mut self, receiver: &Receiver<Inbound>, shutdown: &AtomicBool) {
        while !shutdown.load(Ordering::SeqCst) && !sys::shutdown_requested() {
            match receiver.recv_timeout(LOOP_TICK) {
                Ok(Inbound::Request(message)) => self.handle_request(*message),
                Ok(Inbound::Closed(conn_id)) => self.close_connection(conn_id),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            self.drain_events();
        }
    }

    fn handle_request(&mut self, message: ConnectionRequest) {
        let ConnectionRequest {
            conn_id,
            protocol,
            peer,
            local_ip,
            mut writer,
            request,
        } = message;
        let before = self.conn_streams.get(&conn_id).copied();

        let (response, sid) = {
            let mut ctx = HandlerContext::new(&mut self.session, protocol);
            ctx.sid = before;
            ctx.peer_host = peer.ip().to_string();
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

        match sid {
            Some(id) => {
                self.conn_streams.insert(conn_id, id);
            }
            None => {
                self.conn_streams.remove(&conn_id);
            }
        }
        if let Some(id) = sid {
            self.apply_transport_defaults(id, peer.ip());
        }

        if let Some(response) = response {
            let bytes = response.to_bytes();
            if writer.write_all(&bytes).is_err() || writer.flush().is_err() {
                return;
            }
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
        if self.session.stream(id).is_some_and(|stream| stream.do_play) {
            if self.outputs.contains_key(&id) {
                self.sync_pids(id);
            } else {
                self.start_output(id, conn_id, peer, writer);
            }
        }
        self.update_thresholds();
    }

    /// C++ falls back to the RTSP peer when `destination=` is absent and lets
    /// `-r` override the destination (stream.cpp:503-508).
    fn apply_transport_defaults(&mut self, id: i32, peer: IpAddr) {
        let Some(stream) = self.session.stream(id) else {
            return;
        };
        if stream.stream_type != StreamType::RtspUdp {
            return;
        }
        let host = self.config.rrtp.clone().unwrap_or_else(|| {
            if stream.remote_host.is_empty() || stream.remote_host == "0.0.0.0" {
                peer.to_string()
            } else {
                stream.remote_host.clone()
            }
        });
        let port = if stream.remote_port == 0 {
            self.config.start_rtp
        } else {
            stream.remote_port
        };
        self.session.set_udp_target(id, &host, port);
    }

    fn start_output(&mut self, id: i32, conn_id: u64, peer: SocketAddr, writer: TcpStream) {
        let Some(stream) = self.session.stream(id) else {
            return;
        };
        let (stream_type, adapter, ssrc, seq, server_port, remote_host, remote_port) = (
            stream.stream_type,
            stream.adapter,
            stream.session_id as u32,
            stream.rtp_seq,
            stream.server_port,
            stream.remote_host.clone(),
            stream.remote_port,
        );
        if adapter < 0 {
            return;
        }
        let Some(handle) = self.handles.get(adapter as usize).and_then(Option::as_ref) else {
            return;
        };
        let config = OutputConfig {
            ssrc,
            seq,
            fifo_size: self.config.fifo_size,
        };
        let now_ms = self.started.elapsed().as_millis() as i64;
        let output = match stream_type {
            StreamType::RtspUdp => {
                let Ok(destination) = remote_host.parse::<IpAddr>() else {
                    eprintln!("minisatip: invalid RTP destination {remote_host}");
                    return;
                };
                let target = UdpTarget {
                    local: SocketAddr::new(self.config.bind_ip, server_port),
                    dest: SocketAddr::new(destination, remote_port),
                    ttl: None,
                };
                StreamOutput::udp(target, config, now_ms)
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
            StreamType::RtspSrt | StreamType::None => return,
        };
        match output {
            Ok(output) => {
                let pids = self.stream_pids(id, adapter);
                if handle.start_output(id, output, pids) {
                    self.outputs.insert(
                        id,
                        ActiveOutput {
                            conn_id,
                            adapter,
                            stream_type,
                        },
                    );
                }
            }
            Err(error) => eprintln!("minisatip: cannot start stream {id}: {error}"),
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
            self.stop_output(id);
            self.session.close_stream(id);
        }
        if let Some(id) = self.conn_streams.remove(&conn_id) {
            self.stop_output(id);
            self.session.close_stream(id);
        }
        self.update_thresholds();
    }

    fn drain_events(&mut self) {
        for (aid, handle) in self.handles.iter().enumerate() {
            let Some(handle) = handle else {
                continue;
            };
            while let Ok(event) = handle.events().try_recv() {
                match event {
                    ms_net::dataplane::Event::OutputClosed(id)
                    | ms_net::dataplane::Event::OutputStopped(id, _) => {
                        self.outputs.remove(&id);
                        self.session.close_stream(id);
                    }
                    ms_net::dataplane::Event::TunerError(error) => {
                        eprintln!("minisatip: adapter {aid}: {error}");
                    }
                    ms_net::dataplane::Event::RtcpReceived(_) => {}
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

/// Runs the server until SIGINT/SIGTERM.
pub fn run(opts: &Options) -> Result<(), String> {
    sys::install_shutdown_handlers();

    let rtsp_port = u16::try_from(opts.rtsp_port)
        .map_err(|_| format!("invalid RTSP port {}", opts.rtsp_port))?;
    let http_port = u16::try_from(opts.http_port)
        .map_err(|_| format!("invalid HTTP port {}", opts.http_port))?;
    let start_rtp = u16::try_from(opts.start_rtp)
        .map_err(|_| format!("invalid RTP port {}", opts.start_rtp))?;
    let bind_ip = opts
        .bind
        .as_deref()
        .and_then(|host| host.parse::<IpAddr>().ok())
        .unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    let http_bind_ip = opts
        .bind_http
        .as_deref()
        .and_then(|host| host.parse::<IpAddr>().ok())
        .unwrap_or(bind_ip);
    let advertised_host = match bind_ip {
        IpAddr::V4(ip) if ip.is_unspecified() => "127.0.0.1".to_owned(),
        IpAddr::V6(ip) if ip.is_unspecified() => "::1".to_owned(),
        ip => ip.to_string(),
    };

    let rtsp_listener = TcpListener::bind(SocketAddr::new(bind_ip, rtsp_port))
        .map_err(|error| format!("cannot bind RTSP port {rtsp_port}: {error}"))?;
    let http_listener = TcpListener::bind(SocketAddr::new(http_bind_ip, http_port))
        .map_err(|error| format!("cannot bind HTTP port {http_port}: {error}"))?;
    let rtsp_port = rtsp_listener
        .local_addr()
        .map_err(|error| format!("cannot read RTSP address: {error}"))?
        .port();
    let http_port = http_listener
        .local_addr()
        .map_err(|error| format!("cannot read HTTP address: {error}"))?
        .port();

    let (mut uuid, mut bootid, mut device_id) =
        (opts.uuid.clone(), opts.bootid.max(0), opts.device_id.max(0));
    let cache_dir = PathBuf::from(&opts.cache_dir);
    if cache_dir.is_dir()
        && let Ok((stored_bootid, stored_device_id)) =
            read_boot_id(&cache_dir, device_id, &mut uuid)
    {
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

    let config = ServerConfig {
        bind_ip,
        advertised_host: advertised_host.clone(),
        start_rtp,
        rtsp_port,
        http_port,
        timeout_ms: u32::try_from(opts.timeout_sec).unwrap_or(30_000),
        udp_threshold: opts.udp_threshold,
        tcp_threshold: opts.tcp_threshold,
        fifo_size: fifo_size(opts.max_sbuf),
        rrtp: opts.rrtp.clone(),
    };
    let mut runtime = Runtime::new(opts, config, uuid.clone(), bootid, device_id)?;

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
            http_host: format!("{advertised_host}:{http_port}"),
            xml_path: opts.xml_path.clone(),
            disc_host: opts.disc_host.clone(),
            local_ip: advertised_host.clone(),
            cache_dir: cache_dir.is_dir().then_some(cache_dir.clone()),
            disabled: false,
        };
        Some(spawn_ssdp(
            ssdp_config,
            opts.bind.clone(),
            opts.disc_host.clone(),
            Arc::clone(&shutdown),
        ))
    };

    runtime.run(&receiver, &shutdown);

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

fn fifo_size(max_sbuf: i32) -> usize {
    usize::try_from(max_sbuf)
        .ok()
        .filter(|megabytes| *megabytes > 0)
        .map(|megabytes| megabytes.saturating_mul(1024 * 1024))
        .unwrap_or(DEFAULT_FIFO_SIZE)
}

fn adapter_ids(opts: &Options) -> Vec<usize> {
    let ids: Vec<usize> = opts
        .enabled_adapters
        .as_deref()
        .unwrap_or(&[0])
        .iter()
        .map(|id| usize::from(*id))
        .filter(|id| *id < MAX_ADAPTERS)
        .collect();
    if ids.is_empty() { vec![0] } else { ids }
}

fn adapter_config() -> AdapterConfig {
    let mut config = AdapterConfig {
        enabled: true,
        ..AdapterConfig::default()
    };
    config.sys[0] = DeliverySystem::DvbS2;
    config.sys[1] = DeliverySystem::DvbS;
    config.sys[2] = DeliverySystem::DvbT;
    config.sys[3] = DeliverySystem::DvbT2;
    config.sys[4] = DeliverySystem::DvbC;
    config.sys[5] = DeliverySystem::DvbC2;
    config
}

fn satipc_adapter_config(server: &SatipServer) -> AdapterConfig {
    let [sys0, sys1] = server.systems();
    let mut config = AdapterConfig::new(sys0);
    config.sys[1] = sys1;
    config
}

fn spawn_adapter(
    opts: &Options,
    aid: usize,
    tuner: Box<dyn Tuner + Send>,
) -> Result<AdapterHandle, String> {
    let buffer = usize::try_from(opts.adapter_buffer)
        .ok()
        .filter(|size| *size > 0)
        .unwrap_or(ms_net::dataplane::ADAPTER_BUFFER);
    let adapter_id = i32::try_from(aid).map_err(|_| format!("adapter index {aid} out of range"))?;
    AdapterHandle::spawn(
        tuner,
        DataPlaneConfig {
            adapter_id,
            adapter_buffer: buffer,
            threshold_ms: ms_net::dataplane::clamp_threshold(opts.udp_threshold),
        },
    )
    .map_err(|error| format!("cannot start adapter {aid}: {error}"))
}

fn adapter_disabled(opts: &Options, aid: usize) -> bool {
    opts.enabled_adapters
        .as_ref()
        .is_some_and(|list| u8::try_from(aid).map_or(true, |id| !list.contains(&id)))
}

/// `find_satip_adapter` plus the start-up `satip_getxml` pass.
fn satip_servers(opts: &Options) -> Result<Vec<SatipServer>, String> {
    let over_tcp = opts.satip_rtsp_over_tcp != 0;
    let mut servers = match opts.satip_servers.as_deref() {
        Some(spec) => parse_satip_servers(spec, over_tcp)?,
        None => Vec::new(),
    };
    let Some(urls) = opts.satip_xml.as_deref() else {
        return Ok(servers);
    };
    for url in urls
        .split(|c: char| c == ',' || (c as u32) < 33)
        .filter(|url| !url.is_empty())
    {
        let Ok((host, xml)) = fetch_satip_xml(url) else {
            eprintln!("minisatip: cannot read satip xml from {url}");
            continue;
        };
        let Some(description) = parse_satip_xml(&xml, &host) else {
            eprintln!("minisatip: no satipcap found for {url}");
            continue;
        };
        // detect_tuners_satip counts the adapters already configured for host:port.
        let mut detected = [0i32; MAX_DVBAPI_SYSTEMS];
        for server in &servers {
            if server.host == description.host
                && server.port == description.port
                && let Ok(code) = usize::try_from(delsys_code(server.delsys))
                && let Some(count) = detected.get_mut(code)
            {
                *count += 1;
            }
        }
        if detected == description.tuners {
            continue;
        }
        servers.retain(|server| server.host != description.host || server.port != description.port);
        servers.extend(description.servers(over_tcp));
    }
    Ok(servers)
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
                    eprintln!("minisatip: cannot start a connection thread");
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
    while !shutdown.load(Ordering::SeqCst) {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => {
                buffer.extend_from_slice(&chunk[..read]);
                loop {
                    match parse_request(&buffer) {
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
                            };
                            if requests.send(Inbound::Request(Box::new(message))).is_err() {
                                return;
                            }
                        }
                        ParseOutcome::Incomplete { consumed }
                        | ParseOutcome::Ignored { consumed } => {
                            if consumed > 0 {
                                buffer.drain(..consumed);
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
    bind: Option<String>,
    disc_host: String,
    shutdown: Arc<AtomicBool>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("ssdp".to_owned())
        .spawn(move || ssdp_loop(config, bind, disc_host, shutdown))
        .expect("spawn SSDP thread")
}

fn ssdp_loop(
    config: SsdpConfig,
    bind: Option<String>,
    disc_host: String,
    shutdown: Arc<AtomicBool>,
) {
    let io = match UdpSsdpIo::open(bind.as_deref(), &disc_host) {
        Ok(io) => io,
        Err(error) => {
            eprintln!("minisatip: SSDP disabled: {error}");
            return;
        }
    };
    let _ = io.set_read_timeout(Some(SSDP_READ_TIMEOUT));
    let mut server = SsdpServer::new(io, config);
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
        eprintln!(
            "minisatip: SRT disabled: {}",
            ms_net::srt::LibSrtOps::last_error()
        );
        return None;
    }
    let socket = match std::net::UdpSocket::bind(SocketAddr::new(config.bind_ip, config.rtsp_port))
    {
        Ok(socket) => socket,
        Err(error) => {
            eprintln!("minisatip: SRT disabled: UDP bind failed: {error}");
            ms_net::srt::LibSrtOps::cleanup();
            return None;
        }
    };
    let mut listener = ms_net::srt::SrtListener::new(Arc::new(ms_net::srt::LibSrtOps));
    if let Err(error) = listener.init(socket.into_raw_fd()) {
        eprintln!("minisatip: SRT disabled: {error}");
        ms_net::srt::LibSrtOps::cleanup();
        return None;
    }
    Some(listener)
}
