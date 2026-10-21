//! Server runtime: listeners, the RTSP/HTTP control loop and adapter data planes.
//!
//! The control plane is a single thread driving [`SessionManager`]; every
//! accepted connection has a reader thread that parses requests and forwards
//! them here. Adapters run on their own data-plane threads.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use ms_config::Options;
use ms_net::bandwidth::Bandwidth;
use ms_net::bind::{BindOptions, resolve_bind_opts};
use ms_net::dataplane::{AdapterHandle, Command, DataPlaneConfig, DataPlaneTuner};
use ms_net::handler::{HandlerContext, StreamBackend, StreamType, handle_http, handle_rtsp};
use ms_net::output::{OutputConfig, StreamOutput, UdpTarget};
use ms_net::response::format_http_date;
use ms_net::rtsp::{ParseOutcome, Protocol, Request, parse_request_with};
use ms_net::session::{DescribeOptions, SessionManager};
use ms_net::socket::DEFAULT_FIFO_SIZE;
use ms_net::ssdp::{
    ANNOUNCE_INTERVAL_MS, SharedIdentity, SsdpConfig, SsdpServer, UdpSsdpIo, read_boot_id, uuid4,
};
use ms_net::web::{ApiFeatures, RuntimeState, WebConfig, WebService, format_asctime};
use ms_sched::{
    AdapterConfig, AdapterTuner, MAX_ADAPTERS, MAX_DELSYS, Scheduler, SwitchType, TuneError,
};
use ms_ts::TsStage;
use ms_tuner::satipc::{
    MAX_DVBAPI_SYSTEMS, SatipServer, SatipcConfig, SatipcTuner, delsys_code, fetch_satip_xml,
    parse_satip_servers, parse_satip_xml,
};
use ms_tuner::{
    DeliverySystem, FileReplayConfig, FileReplayTuner, Pacing, PidSet, TuneParams, Tuner,
};

#[cfg(all(feature = "linux-dvb", target_os = "linux"))]
use ms_tuner::linux_dvb::{
    self, DiseqcParams, LinuxDeviceTree, LinuxDvbTuner, LinuxFrontendProbe, SWITCH_JESS,
    SWITCH_SLAVE, SWITCH_UNICABLE, TUNER_FORCE_NO, TUNER_FORCE_SNR_DECIBEL,
    TUNER_FORCE_SNR_PERCENT, TUNER_FORCE_STRENGTH_DECIBEL, TUNER_FORCE_STRENGTH_PERCENT,
};

use crate::ca::CaRuntime;
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

/// Settings shared by start-up and dynamically added satipc adapters.
#[derive(Debug, Clone)]
struct SatipSettings {
    start_rtp: u16,
    adapter_timeout_ms: i64,
    bind_dev_ip: Option<IpAddr>,
    receive_buffer: i32,
    adapter_buffer: i32,
    threshold_ms: i64,
    global_strength: f32,
    global_snr: f32,
    multipliers: Vec<(u8, f32, f32)>,
    enabled_adapters: Option<Vec<u8>>,
}

impl SatipSettings {
    fn from_options(opts: &Options, start_rtp: u16) -> Self {
        Self {
            start_rtp,
            adapter_timeout_ms: i64::from(opts.adapter_timeout),
            // bind_dev_ip(): the address of `--bind-dev`, never `--bind`.
            bind_dev_ip: opts
                .bind_dev
                .as_deref()
                .and_then(ms_net::sys::dev_ip)
                .map(IpAddr::V4),
            receive_buffer: opts.satipc_buffer,
            adapter_buffer: opts.adapter_buffer,
            threshold_ms: ms_net::dataplane::clamp_threshold(opts.udp_threshold),
            global_strength: opts.strength_multiplier,
            global_snr: opts.snr_multiplier,
            multipliers: opts
                .signal_multipliers
                .iter()
                .map(|entry| (entry.adapter, entry.strength, entry.snr))
                .collect(),
            enabled_adapters: opts.enabled_adapters.clone(),
        }
    }

    /// `ad->strength_multiplier` / `ad->snr_multiplier` for `aid`.
    fn signal_multipliers(&self, aid: usize) -> (f32, f32) {
        let adapter = u8::try_from(aid).ok();
        self.multipliers
            .iter()
            .rev()
            .find(|(entry, _, _)| Some(*entry) == adapter)
            .map_or(
                (self.global_strength, self.global_snr),
                |(_, strength, snr)| (*strength, *snr),
            )
    }

    fn adapter_disabled(&self, aid: usize) -> bool {
        self.enabled_adapters
            .as_ref()
            .is_some_and(|list| u8::try_from(aid).map_or(true, |id| !list.contains(&id)))
    }
}

/// One configured satipc adapter (`satip[i]` with its host and port).
struct SatipAdapter {
    aid: usize,
    host: String,
    port: u16,
    delsys: DeliverySystem,
}

/// `--satip-xml` polling state (`satip_getxml` timer).
struct SatipXmlPoll {
    urls: Vec<String>,
    over_tcp: bool,
    next: Instant,
    interval: Duration,
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

/// Placeholder for adapters that exist in the device table but were not opened.
#[cfg(all(feature = "linux-dvb", target_os = "linux"))]
struct UnavailableTuner;

#[cfg(all(feature = "linux-dvb", target_os = "linux"))]
impl AdapterTuner for UnavailableTuner {
    fn tune(&mut self, _params: &TuneParams) -> Result<(), TuneError> {
        Err(TuneError::Hardware)
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
    /// Process-wide counters fed by every data plane, including satipc
    /// adapters added later by the XML poll.
    bandwidth: Arc<Bandwidth>,
    satip: SatipSettings,
    satip_adapters: Vec<SatipAdapter>,
    xml_poll: Option<SatipXmlPoll>,
    ca: CaRuntime,
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
        // Any `-A` entry enables virtual DiSEqC (adapter.cpp:1816).
        scheduler.set_absolute_switch(!opts.absolute_sources.is_empty());
        let replay = std::env::var_os("MINISATIP_REPLAY").map(PathBuf::from);
        let replay_enabled = replay.is_some();
        let adapter_ids = adapter_ids(opts);
        let mut handles: Vec<Option<AdapterHandle>> = (0..MAX_ADAPTERS).map(|_| None).collect();
        let bandwidth = Arc::new(Bandwidth::new());
        let mut ca = CaRuntime::new(opts);

        if let Some(path) = replay {
            for aid in adapter_ids {
                let tuner = FileReplayTuner::open(
                    &path,
                    FileReplayConfig {
                        looping: true,
                        pacing: replay_pacing(),
                    },
                )
                .map_err(|error| format!("cannot replay {}: {error}", path.display()))?;
                let handle = spawn_adapter(
                    opts,
                    aid,
                    Box::new(tuner),
                    Some(Arc::clone(&bandwidth)),
                    ca.stages(aid),
                )?;
                let bridge = DataPlaneTuner::new(handle.command_sender());
                scheduler.add_adapter(aid, adapter_config(), Box::new(bridge));
                // 1-based `fe=` -> adapter number, as for DVB adapters below.
                scheduler.set_fe_map_entry(aid, aid as i16);
                handles[aid] = Some(handle);
            }
        }
        #[cfg(all(feature = "linux-dvb", target_os = "linux"))]
        if !replay_enabled && !opts.disable_dvb {
            let dvbca = crate::dvbca::Dvbca::spawn(
                crate::dvbca::DvbcaSettings::from_options(opts),
                Box::new(ms_ca_drivers::en50221::sys::DvbCaOpener),
                Arc::new(ms_ca_drivers::en50221::SystemClock::default()),
            )
            .map_err(|error| format!("cannot start the CA thread: {error}"))?;
            ca.attach_dvbca(&mut scheduler, dvbca);
            if opts.hw_descrambler {
                let hw = crate::hw::Hw::new(ms_ca_drivers::hw_descrambler::LinuxCaDevice);
                ca.attach_hw(&mut scheduler, hw);
            }
        }
        #[cfg(all(feature = "linux-dvb", target_os = "linux"))]
        if !replay_enabled {
            // find_ddci_adapter: DDCI registers before the tuners get their stages.
            let nodes = if opts.disable_dvb {
                Vec::new()
            } else {
                crate::ddci::linux::find()
            };
            let hub = (!nodes.is_empty()).then(crate::ddci::DdciHub::new);
            if let Some(hub) = hub.as_ref() {
                ca.attach_ddci(&mut scheduler, hub.clone());
            }
            spawn_dvb_adapters(
                opts,
                &mut scheduler,
                &mut handles,
                Some(Arc::clone(&bandwidth)),
                &mut ca,
            )?;
            if let Some(hub) = hub {
                spawn_ddci_adapters(
                    opts,
                    &nodes,
                    &hub,
                    &mut scheduler,
                    &mut handles,
                    Some(Arc::clone(&bandwidth)),
                    &mut ca,
                )?;
            }
        }

        // find_satip_adapter runs after the local adapters (find_adapters).
        let satip = SatipSettings::from_options(opts, config.start_rtp);
        let mut satip_adapters = Vec::new();
        for server in satip_servers(opts)? {
            let Some(aid) = (0..MAX_ADAPTERS)
                .find(|aid| handles[*aid].is_none() && !satip.adapter_disabled(*aid))
            else {
                eprintln!("minisatip: MAX_ADAPTERS reached");
                break;
            };
            let handle = spawn_satip(
                &satip,
                &server,
                aid,
                Some(Arc::clone(&bandwidth)),
                ca.stages(aid),
            )?;
            let bridge = DataPlaneTuner::new(handle.command_sender());
            scheduler.add_adapter(aid, satipc_adapter_config(&server), Box::new(bridge));
            handles[aid] = Some(handle);
            satip_adapters.push(SatipAdapter {
                aid,
                host: server.host,
                port: server.port,
                delsys: server.delsys,
            });
        }
        let xml_poll = opts.satip_xml.as_deref().and_then(|urls| {
            let urls: Vec<String> = urls
                .split(|c: char| c == ',' || (c as u32) < 33)
                .filter(|url| !url.is_empty())
                .map(str::to_owned)
                .collect();
            if urls.is_empty() {
                return None;
            }
            let interval = xml_poll_interval();
            Some(SatipXmlPoll {
                urls,
                over_tcp: opts.satip_rtsp_over_tcp != 0,
                next: Instant::now() + interval,
                interval,
            })
        });

        // pmt_init_device: CAs registered at start-up may handle every adapter.
        for aid in 0..MAX_ADAPTERS {
            if scheduler.adapter(aid).is_some() {
                ca.adapter_added(&mut scheduler, aid);
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
            features: ApiFeatures::default(),
        })
        .with_identity(identity);
        session.set_web(web);
        // minisatip.cpp:1149-1166; read_http refreshes the current time.
        let start = SystemTime::now();
        session.set_web_runtime(RuntimeState {
            datetime_compile: env!("MINISATIP_DATETIME_COMPILE").to_owned(),
            datetime_start: format_asctime(start),
            datetime_current: " ".to_owned(),
            time_running: "0".to_owned(),
            run_user: sys::run_user(),
            run_pid: i32::try_from(std::process::id()).unwrap_or(0),
            command_line: opts.command_line.clone(),
            start_time: Some(start),
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
            bandwidth: Arc::clone(&bandwidth),
            satip,
            satip_adapters,
            xml_poll,
            ca,
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
            self.poll_satip_xml();
            self.ca.tick(self.session.scheduler_mut(), &self.handles);
        }
    }

    /// `add_satip_server`: installs one satipc adapter in the first free slot.
    fn add_satip_adapter(&mut self, server: SatipServer) -> Result<(), String> {
        let Some(aid) = (0..MAX_ADAPTERS)
            .find(|aid| self.handles[*aid].is_none() && !self.satip.adapter_disabled(*aid))
        else {
            eprintln!("minisatip: MAX_ADAPTERS reached");
            return Ok(());
        };
        let stages = self.ca.stages(aid);
        let handle = spawn_satip(
            &self.satip,
            &server,
            aid,
            Some(Arc::clone(&self.bandwidth)),
            stages,
        )?;
        let bridge = DataPlaneTuner::new(handle.command_sender());
        self.session.scheduler_mut().add_adapter(
            aid,
            satipc_adapter_config(&server),
            Box::new(bridge),
        );
        self.ca.adapter_added(self.session.scheduler_mut(), aid);
        self.handles[aid] = Some(handle);
        self.satip_adapters.push(SatipAdapter {
            aid,
            host: server.host,
            port: server.port,
            delsys: server.delsys,
        });
        Ok(())
    }

    /// `disable_satip_server`: closes the adapters and frees their slots.
    fn remove_satip_server(&mut self, host: &str, port: u16) {
        let aids: Vec<usize> = self
            .satip_adapters
            .iter()
            .filter(|entry| entry.host == host && entry.port == port)
            .map(|entry| entry.aid)
            .collect();
        for aid in aids {
            let adapter = i32::try_from(aid).unwrap_or(i32::MAX);
            let ids: Vec<i32> = (0..ms_net::handler::MAX_STREAMS)
                .filter(|id| {
                    self.session
                        .stream(*id)
                        .is_some_and(|stream| stream.adapter == adapter)
                })
                .collect();
            for id in ids {
                self.stop_output(id);
                self.session.close_stream(id);
            }
            let stale: Vec<i32> = self
                .outputs
                .iter()
                .filter(|(_, active)| active.adapter == adapter)
                .map(|(id, _)| *id)
                .collect();
            for id in stale {
                self.stop_output(id);
                self.session.close_stream(id);
            }
            if let Some(handle) = self.handles[aid].take() {
                handle.stop();
            }
            self.session.scheduler_mut().remove_adapter(aid);
            self.ca.adapter_removed(aid);
        }
        self.satip_adapters
            .retain(|entry| entry.host != host || entry.port != port);
    }

    /// `satip_getxml`: fetches every URL once the 120 s timer elapsed.
    fn poll_satip_xml(&mut self) {
        let Some(poll) = self.xml_poll.as_ref() else {
            return;
        };
        let now = Instant::now();
        if now < poll.next {
            return;
        }
        let urls = poll.urls.clone();
        let over_tcp = poll.over_tcp;
        let interval = poll.interval;
        if let Some(poll) = self.xml_poll.as_mut() {
            poll.next = now + interval;
        }
        for url in urls {
            self.reconcile_satip_xml(&url, over_tcp);
        }
    }

    /// `satip_getxml_data`: compares the advertised tuners with the configured
    /// adapters and replaces them when the counts differ.
    fn reconcile_satip_xml(&mut self, url: &str, over_tcp: bool) {
        let Ok((host, xml)) = fetch_satip_xml(url) else {
            eprintln!("minisatip: cannot read satip xml from {url}");
            return;
        };
        let Some(description) = parse_satip_xml(&xml, &host) else {
            eprintln!("minisatip: no satipcap found for {url}");
            return;
        };
        // detect_tuners_satip counts the adapters already configured for host:port.
        let mut detected = [0i32; MAX_DVBAPI_SYSTEMS];
        for entry in &self.satip_adapters {
            if entry.host == description.host
                && entry.port == description.port
                && let Ok(code) = usize::try_from(delsys_code(entry.delsys))
                && let Some(count) = detected.get_mut(code)
            {
                *count += 1;
            }
        }
        if detected == description.tuners {
            return;
        }
        self.remove_satip_server(&description.host, description.port);
        for server in description.servers(over_tcp) {
            if let Err(error) = self.add_satip_adapter(server) {
                eprintln!("minisatip: cannot update satip xml {url}: {error}");
            }
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
            ctx.rrtp = self.config.rrtp.clone();
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
        // PID changes move PMT claims (pmt_pid_updated_pids); push them to the data planes.
        self.ca
            .after_scheduler(self.session.scheduler_mut(), &self.handles);
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
        let mut tables = Vec::new();
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
                    ms_net::dataplane::Event::TablePackets {
                        generation,
                        packets,
                    } => tables.push((aid, generation, packets)),
                }
            }
        }
        for (aid, generation, packets) in tables {
            self.ca.on_table_packets(
                self.session.scheduler_mut(),
                &self.handles,
                aid,
                generation,
                &packets,
            );
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
        self.ca.shutdown();
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
    // A mutex rather than `AtomicU64`, which 32-bit MIPS does not provide.
    let next_id = Arc::new(Mutex::new(1_u64));
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

/// `MINISATIP_REPLAY_BITRATE=<bit/s>` paces the replay (DEV-120); unset or 0 is unpaced.
fn replay_pacing() -> Pacing {
    std::env::var("MINISATIP_REPLAY_BITRATE")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|bits| *bits > 0)
        .map_or(Pacing::None, Pacing::FixedBitrate)
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
    bandwidth: Option<Arc<Bandwidth>>,
    stages: Vec<Box<dyn TsStage>>,
) -> Result<AdapterHandle, String> {
    let buffer = usize::try_from(opts.adapter_buffer)
        .ok()
        .filter(|size| *size > 0)
        .unwrap_or(ms_net::dataplane::ADAPTER_BUFFER);
    spawn_adapter_with(
        aid,
        tuner,
        buffer,
        ms_net::dataplane::clamp_threshold(opts.udp_threshold),
        bandwidth,
        stages,
    )
}

fn spawn_adapter_with(
    aid: usize,
    tuner: Box<dyn Tuner + Send>,
    buffer: usize,
    threshold_ms: i64,
    bandwidth: Option<Arc<Bandwidth>>,
    stages: Vec<Box<dyn TsStage>>,
) -> Result<AdapterHandle, String> {
    let adapter_id = i32::try_from(aid).map_err(|_| format!("adapter index {aid} out of range"))?;
    AdapterHandle::spawn_with_stages(
        tuner,
        DataPlaneConfig {
            adapter_id,
            adapter_buffer: buffer,
            threshold_ms,
        },
        bandwidth,
        stages,
    )
    .map_err(|error| format!("cannot start adapter {aid}: {error}"))
}

/// `add_satip_server` for an allocated slot.
fn spawn_satip(
    settings: &SatipSettings,
    server: &SatipServer,
    aid: usize,
    bandwidth: Option<Arc<Bandwidth>>,
    stages: Vec<Box<dyn TsStage>>,
) -> Result<AdapterHandle, String> {
    let (strength, snr) = settings.signal_multipliers(aid);
    let config = SatipcConfig {
        adapter_id: aid,
        server: server.clone(),
        start_rtp: settings.start_rtp,
        user_agent: format!("minisatip {}", env!("CARGO_PKG_VERSION")),
        adapter_timeout_ms: settings.adapter_timeout_ms,
        // src_satipc.cpp:605: both multipliers zero hides the server signal.
        ignore_signal: strength == 0.0 && snr == 0.0,
        bind_dev_ip: settings.bind_dev_ip,
        receive_buffer: settings.receive_buffer,
    };
    if !server.source_ip.is_empty() && settings.bind_dev_ip.is_some() {
        eprintln!(
            "minisatip: AD{aid}: per-server source {} overrides --bind-dev source {:?}",
            server.source_ip, settings.bind_dev_ip
        );
    }
    let tuner = SatipcTuner::new(&config)
        .map_err(|error| format!("cannot start satip adapter {aid}: {error}"))?;
    let buffer = usize::try_from(settings.adapter_buffer)
        .ok()
        .filter(|size| *size > 0)
        .unwrap_or(ms_net::dataplane::ADAPTER_BUFFER);
    spawn_adapter_with(
        aid,
        Box::new(tuner),
        buffer,
        settings.threshold_ms,
        bandwidth,
        stages,
    )
}

/// `satip_getxml` timer period (src_minisatip.cpp:2006).
const SATIP_XML_POLL_MS: u64 = 120_000;

fn xml_poll_interval() -> Duration {
    let millis = std::env::var("MINISATIP_SATIP_XML_POLL_MS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(SATIP_XML_POLL_MS);
    Duration::from_millis(millis)
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

/// Opens every `/dev/dvb` frontend and installs it in the scheduler.
#[cfg(all(feature = "linux-dvb", target_os = "linux"))]
fn spawn_dvb_adapters(
    opts: &Options,
    scheduler: &mut Scheduler,
    handles: &mut [Option<AdapterHandle>],
    bandwidth: Option<Arc<Bandwidth>>,
    ca: &mut CaRuntime,
) -> Result<(), String> {
    if opts.disable_dvb {
        return Ok(());
    }
    let frontends = linux_dvb::enumerate(&LinuxDeviceTree, &LinuxFrontendProbe);
    let buffer = usize::try_from(opts.adapter_buffer)
        .ok()
        .filter(|size| *size > 0)
        .unwrap_or(ms_net::dataplane::ADAPTER_BUFFER);
    for frontend in frontends {
        let aid = frontend.adapter as usize;
        if aid >= MAX_ADAPTERS {
            continue;
        }
        let selected = opts
            .enabled_adapters
            .as_ref()
            .is_none_or(|list| list.contains(&(aid as u8)));
        if !selected {
            scheduler.set_disabled(aid, true);
        }
        let diseqc = diseqc_params(opts, aid);
        let mut config = dvb_adapter_config(opts, aid, &frontend.systems, &diseqc);
        config.enabled = selected;
        let bridge: Box<dyn AdapterTuner> = if selected {
            match LinuxDvbTuner::open_with_buffer(
                frontend.path_adapter,
                frontend.path_frontend,
                opts.dvr_buffer,
            ) {
                Ok(mut tuner) => {
                    apply_tuner_options(opts, aid, &diseqc, &mut tuner);
                    ca.set_hw_node(
                        aid,
                        frontend.path_adapter as i32,
                        frontend.path_frontend as i32,
                    );
                    let handle = spawn_adapter_with(
                        aid,
                        Box::new(tuner),
                        buffer,
                        ms_net::dataplane::clamp_threshold(opts.udp_threshold),
                        bandwidth.clone(),
                        ca.stages(aid),
                    )?;
                    let bridge = DataPlaneTuner::new(handle.command_sender());
                    handles[aid] = Some(handle);
                    // init_hw -> pmt_init_device -> dvbca_init_dev (ca.cpp:3891).
                    ca.dvbca_init_dev(ms_ca_drivers::en50221::AdapterInfo::dvb(
                        aid as i32,
                        frontend.path_adapter,
                        frontend.path_frontend,
                    ));
                    Box::new(bridge)
                }
                Err(error) => {
                    eprintln!(
                        "minisatip: adapter {aid} (adapter{}/frontend{}): {error}",
                        frontend.path_adapter, frontend.path_frontend
                    );
                    config.enabled = false;
                    Box::new(UnavailableTuner)
                }
            }
        } else {
            Box::new(UnavailableTuner)
        };
        scheduler.add_adapter(aid, config, bridge);
        // Default frontend map: the URL frontend number is 1-based and
        // `get_adapter_nw` reads `fe_map[fe - 1]`, so `?fe=N` selects
        // adapter N-1 unless `-s` installs another map.
        scheduler.set_fe_map_entry(aid, aid as i16);
    }
    Ok(())
}

/// Opens each DDCI device as a CI adapter after the tuners (`find_ddci_adapter`,
/// `ddci_open_device`, `ddci_post_init`).
#[cfg(all(feature = "linux-dvb", target_os = "linux"))]
fn spawn_ddci_adapters(
    opts: &Options,
    nodes: &[crate::ddci::linux::DdciNode],
    hub: &crate::ddci::DdciHub,
    scheduler: &mut Scheduler,
    handles: &mut [Option<AdapterHandle>],
    bandwidth: Option<Arc<Bandwidth>>,
    ca: &mut CaRuntime,
) -> Result<(), String> {
    for node in nodes {
        let Some(aid) = (0..MAX_ADAPTERS).find(|aid| handles[*aid].is_none()) else {
            break;
        };
        let (reader, writer) = match crate::ddci::linux::open(node) {
            Ok(pair) => pair,
            Err(error) => {
                eprintln!("minisatip: DDCI adapter{}/sec{}: {error}", node.pa, node.fe);
                continue;
            }
        };
        hub.open_device(aid, Box::new(writer));
        let buffer = usize::try_from(opts.adapter_buffer)
            .ok()
            .filter(|size| *size > 0)
            .unwrap_or(ms_net::dataplane::ADAPTER_BUFFER);
        let handle = spawn_adapter_with(
            aid,
            Box::new(reader),
            buffer,
            ms_net::dataplane::clamp_threshold(opts.udp_threshold),
            bandwidth.clone(),
            ca.ci_stages(aid),
        )?;
        // The CAM output is read without client streams (ddci_read_sec_data).
        handle.send(Command::KeepReading(true));
        let bridge = DataPlaneTuner::new(handle.command_sender());
        let config = AdapterConfig {
            enabled: true,
            adapter_type: ms_sched::AdapterType::Ci,
            ..AdapterConfig::default()
        };
        scheduler.add_adapter(aid, config, Box::new(bridge));
        handles[aid] = Some(handle);
        scheduler.post_tune_adapter(aid);
        ca.adapter_added(scheduler, aid);
        ca.dvbca_init_dev(ms_ca_drivers::en50221::AdapterInfo {
            id: aid as i32,
            enabled: true,
            kind: ms_ca_drivers::en50221::AdapterKind::Ci,
            pa: node.pa,
            fe: node.fe,
        });
    }
    Ok(())
}

/// Builds the scheduler view of one enumerated frontend.
#[cfg(all(feature = "linux-dvb", target_os = "linux"))]
fn dvb_adapter_config(
    opts: &Options,
    aid: usize,
    probed: &[DeliverySystem],
    diseqc: &DiseqcParams,
) -> AdapterConfig {
    let mut config = AdapterConfig {
        max_pids: opts.max_pids,
        ..AdapterConfig::default()
    };
    for (slot, system) in adapter_systems(opts, aid, probed)
        .into_iter()
        .filter(|system| *system != DeliverySystem::Undefined)
        .take(MAX_DELSYS)
        .enumerate()
    {
        config.sys[slot] = system;
    }
    config.absolute_table = absolute_table(opts, aid);
    config.master_source = slave_masters(opts)[aid];
    config.switch_type = match diseqc.switch_type {
        SWITCH_UNICABLE => SwitchType::Unicable,
        SWITCH_JESS => SwitchType::Jess,
        SWITCH_SLAVE => SwitchType::Slave,
        _ => SwitchType::None,
    };
    config.lnb_switch = diseqc.lnb_switch.max(0) as u32;
    config.lnb_circular = diseqc.lnb_circular.max(0) as u32;
    config
}

/// Delivery systems for one adapter: `-Y` wins over the probed capabilities.
#[cfg(all(feature = "linux-dvb", target_os = "linux"))]
fn adapter_systems(opts: &Options, aid: usize, probed: &[DeliverySystem]) -> Vec<DeliverySystem> {
    let Some(delsys) = opts
        .delsys
        .iter()
        .rev()
        .find(|entry| usize::from(entry.adapter) == aid)
    else {
        return probed.to_vec();
    };
    vec![
        to_tuner_delsys(delsys.system),
        to_tuner_delsys(delsys.secondary),
    ]
}

#[cfg(all(feature = "linux-dvb", target_os = "linux"))]
fn to_tuner_delsys(system: ms_config::DeliverySystem) -> DeliverySystem {
    use ms_config::DeliverySystem as Config;
    match system {
        Config::Undefined => DeliverySystem::Undefined,
        Config::Dvbs => DeliverySystem::DvbS,
        Config::Dvbs2 => DeliverySystem::DvbS2,
        Config::Dvbt => DeliverySystem::DvbT,
        Config::Dvbt2 => DeliverySystem::DvbT2,
        Config::Dvbc => DeliverySystem::DvbC,
        Config::Dvbc2 => DeliverySystem::DvbC2,
        Config::Dvbcb => DeliverySystem::Other(2),
        Config::Dss => DeliverySystem::Other(4),
        Config::Dvbh => DeliverySystem::Other(7),
        Config::Isdbt => DeliverySystem::Other(8),
        Config::Isdbs => DeliverySystem::Other(9),
        Config::Isdbc => DeliverySystem::Other(10),
        Config::Atsc => DeliverySystem::Other(11),
        Config::Atscmh => DeliverySystem::Other(12),
        Config::Dmbth => DeliverySystem::Other(13),
        Config::Cmmb => DeliverySystem::Other(14),
        Config::Dab => DeliverySystem::Other(15),
        Config::Turbo => DeliverySystem::Other(17),
        Config::Dvbcc => DeliverySystem::Other(18),
    }
}

/// `-A` table: `table[src] = position + (src - first)` (adapter.cpp:1804-1815).
#[cfg(all(feature = "linux-dvb", target_os = "linux"))]
fn absolute_table(opts: &Options, aid: usize) -> [u8; ms_sched::MAX_SOURCES] {
    let mut table = [0u8; ms_sched::MAX_SOURCES];
    for entry in &opts.absolute_sources {
        if usize::from(entry.adapter) != aid || entry.src > entry.src_end {
            continue;
        }
        for (count, src) in (entry.src..=entry.src_end).enumerate() {
            if let Some(slot) = table.get_mut(usize::from(src)) {
                *slot = entry.position.saturating_add(count as u8);
            }
        }
    }
    table
}

/// `-S` bindings with the C++ first-wins/force-master rules (adapter.cpp:2013).
#[cfg(all(feature = "linux-dvb", target_os = "linux"))]
fn slave_masters(opts: &Options) -> [Option<usize>; MAX_ADAPTERS] {
    let mut state = [-1i32; MAX_ADAPTERS];
    for entry in &opts.slaves {
        let slave = usize::from(entry.adapter);
        if slave >= MAX_ADAPTERS || state[slave] != -1 {
            continue;
        }
        if !(0..MAX_ADAPTERS as i32).contains(&entry.master) {
            continue;
        }
        let master = entry.master as usize;
        if state[master] < 0 {
            if state[master] == -1 {
                state[master] = -2;
            }
            state[slave] = entry.master;
        }
    }
    std::array::from_fn(|index| (state[index] >= 0).then_some(state[index] as usize))
}

/// Per-adapter DiSEqC/LNB/unicable parameters, starting from the global options.
#[cfg(all(feature = "linux-dvb", target_os = "linux"))]
fn diseqc_params(opts: &Options, aid: usize) -> DiseqcParams {
    let mut params = DiseqcParams {
        fast: opts.diseqc_fast,
        addr: opts.diseqc_addr,
        committed_no: opts.diseqc_committed_no,
        uncommitted_no: opts.diseqc_uncommitted_no,
        before_cmd: opts.diseqc_before_cmd,
        after_cmd: opts.diseqc_after_cmd,
        after_repeated_cmd: opts.diseqc_after_repeated_cmd,
        after_switch: opts.diseqc_after_switch,
        after_burst: opts.diseqc_after_burst,
        after_tone: opts.diseqc_after_tone,
        lnb_low: opts.lnb_low / 1000,
        lnb_high: opts.lnb_high / 1000,
        lnb_switch: opts.lnb_switch / 1000,
        lnb_circular: opts.lnb_circular / 1000,
        ..DiseqcParams::default()
    };
    if let Some(entry) = opts
        .diseqc
        .iter()
        .rev()
        .find(|entry| usize::from(entry.adapter) == aid)
    {
        params.fast = i32::from(entry.fast);
        params.addr = i32::from(entry.addr);
        params.committed_no = entry.committed_no;
        params.uncommitted_no = entry.uncommitted_no;
    }
    if let Some(entry) = opts
        .diseqc_timings
        .iter()
        .rev()
        .find(|entry| usize::from(entry.adapter) == aid)
    {
        params.before_cmd = entry.before_cmd;
        params.after_cmd = entry.after_cmd;
        params.after_repeated_cmd = entry.after_repeated_cmd;
        params.after_switch = entry.after_switch;
        params.after_burst = entry.after_burst;
        params.after_tone = entry.after_tone;
    }
    if let Some(entry) = opts
        .unicable
        .iter()
        .rev()
        .find(|entry| usize::from(entry.adapter) == aid)
    {
        params.switch_type = match entry.kind {
            ms_config::UnicableKind::Unicable => SWITCH_UNICABLE,
            ms_config::UnicableKind::Jess => SWITCH_JESS,
        };
        params.uslot = entry.slot;
        params.ufreq = entry.freq;
        params.pin = entry.pin;
        params.only13v = i32::from(entry.only_13v);
    }
    if let Some(entry) = opts
        .lnb
        .iter()
        .rev()
        .find(|entry| usize::from(entry.adapter) == aid)
    {
        params.lnb_low = entry.low / 1000;
        params.lnb_high = entry.high / 1000;
        params.lnb_switch = entry.switch / 1000;
        params.lnb_circular = entry.circular / 1000;
    }
    params
}

/// Applies per-adapter state the backend reads but the scheduler does not.
#[cfg(all(feature = "linux-dvb", target_os = "linux"))]
fn apply_tuner_options(
    opts: &Options,
    aid: usize,
    diseqc: &DiseqcParams,
    tuner: &mut LinuxDvbTuner,
) {
    tuner.set_diseqc_params(*diseqc);
    let signal = opts
        .signal_multipliers
        .iter()
        .rev()
        .find(|entry| usize::from(entry.adapter) == aid);
    let diseqc_multi = opts
        .diseqc_multis
        .iter()
        .rev()
        .find(|entry| usize::from(entry.adapter) == aid)
        .map_or(opts.diseqc_multi, |entry| entry.position);
    let state = tuner.state_mut();
    state.id = aid as i32;
    state.master_source = slave_masters(opts)[aid].map_or(-1, |master| master as i32);
    state.diseqc_multi = diseqc_multi;
    let (strength, snr, force) = match signal {
        Some(entry) => {
            let mut force = TUNER_FORCE_NO;
            if entry.force.strength_percent {
                force |= TUNER_FORCE_STRENGTH_PERCENT;
            }
            if entry.force.strength_decibel {
                force |= TUNER_FORCE_STRENGTH_DECIBEL;
            }
            if entry.force.snr_percent {
                force |= TUNER_FORCE_SNR_PERCENT;
            }
            if entry.force.snr_decibel {
                force |= TUNER_FORCE_SNR_DECIBEL;
            }
            (entry.strength, entry.snr, force)
        }
        None => (
            opts.strength_multiplier,
            opts.snr_multiplier,
            TUNER_FORCE_NO,
        ),
    };
    state.strength_multiplier = strength;
    state.snr_multiplier = snr;
    state.force_tuner_signal = force;
}

fn spawn_listener(
    listener: TcpListener,
    protocol: Protocol,
    name: &str,
    requests: Sender<Inbound>,
    shutdown: Arc<AtomicBool>,
    next_id: Arc<Mutex<u64>>,
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
    next_id: Arc<Mutex<u64>>,
) {
    while !shutdown.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _peer)) => {
                let conn_id = {
                    let mut next = next_id.lock().unwrap_or_else(PoisonError::into_inner);
                    let conn_id = *next;
                    *next = next.wrapping_add(1);
                    conn_id
                };
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
    use ms_config::{SignalForce, SignalMultiplier};

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

    fn multiplier(adapter: u8, strength: f32, snr: f32) -> SignalMultiplier {
        SignalMultiplier {
            adapter,
            strength,
            snr,
            force: SignalForce {
                strength_percent: false,
                strength_decibel: false,
                snr_percent: false,
                snr_decibel: false,
            },
        }
    }

    // src_adapter.cpp:2204-2217: a per-adapter -M entry overrides the globals;
    // adapters without an entry keep the global values.
    #[test]
    fn per_adapter_multipliers_override_globals() {
        let opts = Options {
            strength_multiplier: 1.0,
            snr_multiplier: 1.0,
            signal_multipliers: vec![multiplier(2, 0.0, 1.0), multiplier(3, 0.0, 0.0)],
            ..Options::default()
        };
        let settings = SatipSettings::from_options(&opts, 5500);
        assert_eq!(settings.signal_multipliers(0), (1.0, 1.0));
        assert_eq!(settings.signal_multipliers(2), (0.0, 1.0));
        assert_eq!(settings.signal_multipliers(3), (0.0, 0.0));
    }

    // src_satipc.cpp:605: only both-zero multipliers hide the server signal.
    #[test]
    fn satipc_ignores_signal_only_when_both_multipliers_are_zero() {
        let opts = Options {
            signal_multipliers: vec![multiplier(0, 0.0, 1.0), multiplier(1, 0.0, 0.0)],
            ..Options::default()
        };
        let settings = SatipSettings::from_options(&opts, 5500);
        let ignore = |aid| {
            let (strength, snr) = settings.signal_multipliers(aid);
            strength == 0.0 && snr == 0.0
        };
        assert!(!ignore(0), "one non-zero multiplier keeps the signal");
        assert!(ignore(1), "both zero multipliers hide the signal");
        assert!(!ignore(2), "globals default to one");
    }

    // src_satipc.cpp:816-822: bind_dev_ip() is the RTSP source and UDP bind
    // address; it is NULL unless --bind-dev was given.
    #[test]
    fn bind_dev_ip_comes_only_from_bind_dev() {
        let mut opts = Options {
            bind: Some("127.0.0.2".to_owned()),
            ..Options::default()
        };
        let settings = SatipSettings::from_options(&opts, 5500);
        assert_eq!(settings.bind_dev_ip, None, "--bind is not bind_dev_ip()");

        opts.bind_dev = Some("lo".to_owned());
        // `lo` resolves to 127.0.0.1 on every Linux host running the suite.
        let settings = SatipSettings::from_options(&opts, 5500);
        assert_eq!(
            settings.bind_dev_ip,
            Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
            "--bind-dev must resolve to its IPv4 address"
        );
    }
}
