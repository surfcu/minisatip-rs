//! Socket driver for [`Client`] and the [`SatipcTuner`] backend.
//!
//! One I/O thread per adapter reads the RTSP (and, for UDP, RTCP) sockets and
//! runs the timers; the data-plane thread reads RTP, SRT or drains interleaved
//! TS.

use std::io::{self, Read, Write};
use std::net::{IpAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::os::fd::AsRawFd;
#[cfg(feature = "srt")]
use std::os::fd::IntoRawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::client::{Client, Op, ReplyOutcome, Transport};
use super::config::SatipServer;
use super::media::{
    Frame, MAX_RTP_MSG, MAX_UDP_PACKET_SIZE, TcpDemux, process_rtp_datagram, push_packets,
};
use super::sys;
#[cfg(feature = "srt")]
use super::util::random_u32;
use super::util::{delsys_code, delsys_name};
use crate::tuner::{
    PidSet, Result, SignalStatus, TS_PACKET_SIZE, TsBuf, TuneParams, Tuner, TunerError,
};

/// `satipc.cpp`'s `#define DEFAULT_LOG LOG_SATIPC`.
const DEFAULT_LOG: u32 = ms_log::LOG_SATIPC;

/// Reconnect backoff of `satipc_open_device`.
const OPEN_BACKOFF_MS: i64 = 5_000;
/// Connect timeout per resolved address.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Maximum time non-blocking RTSP writes may remain stalled before closing.
const TX_STALL_TIMEOUT: Duration = Duration::from_secs(2);
/// Upper bound on [`SatipcTuner`] teardown wait before detaching the I/O thread.
const DROP_JOIN_TIMEOUT: Duration = Duration::from_secs(2);
/// Cap on buffered outbound RTSP bytes while a peer stops reading.
const TX_BUF_MAX: usize = 256 * 1024;
/// I/O thread sleep when no socket had data.
const IO_IDLE: Duration = Duration::from_millis(5);
/// Interleaved TS kept for the data plane (`TCP_DATA_MAX`-like bound).
const TCP_TS_MAX: usize = 4 * 2000 * TS_PACKET_SIZE;
/// Largest UDP datagram read (header plus `MAX_UDP_PACKET_SIZE`).
const DATAGRAM_MAX: usize = 2048;
/// `SRT_LATENCY_MS`.
#[cfg(feature = "srt")]
const SRT_LATENCY_MS: i32 = 1000;

/// Settings for one satipc adapter.
#[derive(Debug, Clone)]
pub struct SatipcConfig {
    /// Adapter index (`ad->id`).
    pub adapter_id: usize,
    /// Remote tuner.
    pub server: SatipServer,
    /// `opts.start_rtp`; RTP listens on `start_rtp + 1000 + 2 * id`.
    pub start_rtp: u16,
    /// `User-Agent` value (`app_name version`).
    pub user_agent: String,
    /// `opts.adapter_timeout` in ms; 0 keeps an idle device open.
    pub adapter_timeout_ms: i64,
    /// Both signal multipliers are 0.
    pub ignore_signal: bool,
    /// `bind_dev_ip()`: source of the RTSP connection and address of the
    /// RTP/RTCP sockets (`src_satipc.cpp:395-400`, `816-822`).
    pub bind_dev_ip: Option<IpAddr>,
    /// `opts.satipc_buffer` in bytes; 0 keeps the kernel default.
    pub receive_buffer: i32,
}

/// [`Client`] plus the sockets it drives.
#[derive(Debug)]
pub struct Driver {
    /// The protocol state machine.
    pub client: Client,
    /// RTSP connection (also the media socket for TCP transport).
    pub rtsp: Option<TcpStream>,
    /// RTP socket for UDP transport (`ad->dvr`).
    pub rtp: Option<UdpSocket>,
    /// RTCP socket for UDP transport (`sip->rtcp`).
    pub rtcp: Option<UdpSocket>,
    /// SRT media socket (`sip->srt_sock`), or `SRT_INVALID_SOCK`.
    #[cfg(feature = "srt")]
    pub srt_sock: i32,
    /// UDP descriptor owned by the SRT socket (`sip->udp_sock`, also
    /// `ad->dvr`), or -1.
    #[cfg(feature = "srt")]
    pub srt_udp: i32,
    /// The device is open (`ad->enabled`).
    pub device_open: bool,
    /// Interleaved TS waiting for the data plane.
    pub tcp_ts: Vec<u8>,
    start_rtp: u16,
    rbuf: Vec<u8>,
    demux: TcpDemux,
    fe_rtime: i64,
    standby_since: Option<i64>,
    adapter_timeout_ms: i64,
    bind_dev_ip: Option<IpAddr>,
    receive_buffer: i32,
    tx_buf: Vec<u8>,
    tx_offset: usize,
    tx_since: Option<Instant>,
    connect_epoch: u64,
    deferred_reconnect: bool,
    defer_open_rtsp: bool,
}

struct ConnectTarget {
    host: String,
    port: u16,
    source: Option<String>,
    epoch: u64,
}

impl Driver {
    /// Creates a closed driver for `config`.
    pub fn new(config: &SatipcConfig) -> Self {
        let server = &config.server;
        let mut client = Client::new(config.adapter_id, &server.host, server.port);
        client.sys0 = server.delsys;
        client.tp.msys = server.delsys;
        client.satip_fe = server.fe;
        client.source_ip = server.source_ip.clone();
        client.transport = server.transport;
        client.no_pids_all = server.no_pids_all;
        client.user_agent = config.user_agent.clone();
        client.ignore_signal = config.ignore_signal;
        // satipc.cpp:2059-2066; the adapter total is not known here.
        let systems = server.systems();
        ms_log::log!(
            "AD{}: Satip device {} port {} delsys {}: {} {}, fe {}, total number of devices {}",
            config.adapter_id,
            server.host,
            server.port,
            delsys_code(server.delsys),
            delsys_name(systems[0]),
            delsys_name(systems[1]),
            server.fe,
            0
        );
        Self {
            client,
            rtsp: None,
            rtp: None,
            rtcp: None,
            #[cfg(feature = "srt")]
            srt_sock: ms_srt::SRT_INVALID_SOCK,
            #[cfg(feature = "srt")]
            srt_udp: -1,
            device_open: false,
            tcp_ts: Vec::new(),
            start_rtp: config.start_rtp,
            rbuf: Vec::new(),
            demux: TcpDemux::new(),
            fe_rtime: 0,
            standby_since: None,
            adapter_timeout_ms: config.adapter_timeout_ms,
            bind_dev_ip: config.bind_dev_ip,
            receive_buffer: config.receive_buffer,
            tx_buf: Vec::new(),
            tx_offset: 0,
            tx_since: None,
            connect_epoch: 0,
            deferred_reconnect: false,
            defer_open_rtsp: false,
        }
    }

    /// Snapshot of frontend status suitable for lock-free callers.
    pub fn signal_status(&self) -> SignalStatus {
        let client = &self.client;
        SignalStatus {
            locked: client.status,
            strength: u16::try_from(client.strength.clamp(0, 255)).unwrap_or(0),
            snr: u16::try_from(client.snr.clamp(0, 255)).unwrap_or(0),
            // SAT>IP status carries no BER or dB SNR (ad->db stays MAX_DB).
            ber: 0,
            db: SignalStatus::DB_UNKNOWN,
            // satipc.cpp:620: `ad->status` is set from every server signal report.
            status_valid: true,
        }
    }

    fn clear_tx(&mut self) {
        self.tx_buf.clear();
        self.tx_offset = 0;
        self.tx_since = None;
    }

    fn flush_tx(&mut self) -> io::Result<bool> {
        let Some(stream) = self.rtsp.as_mut() else {
            self.clear_tx();
            return Ok(false);
        };
        let mut made_progress = false;
        while self.tx_offset < self.tx_buf.len() {
            match stream.write(&self.tx_buf[self.tx_offset..]) {
                Ok(0) => {
                    self.clear_tx();
                    return Err(io::ErrorKind::WriteZero.into());
                }
                Ok(written) => {
                    self.tx_offset += written;
                    made_progress = true;
                    if self.tx_offset < self.tx_buf.len() {
                        self.tx_since = Some(Instant::now());
                    }
                }
                Err(error) if is_transient(&error) => {
                    let since = self.tx_since.get_or_insert_with(Instant::now);
                    if since.elapsed() >= TX_STALL_TIMEOUT {
                        self.clear_tx();
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "satipc RTSP write stalled",
                        ));
                    }
                    return Ok(made_progress);
                }
                Err(error) => {
                    self.clear_tx();
                    return Err(error);
                }
            }
        }
        self.clear_tx();
        Ok(made_progress)
    }

    fn prepare_connect_target(&self) -> ConnectTarget {
        // satipc_open_rtsp_socket: the per-server source overrides bind_dev_ip().
        let source = if self.client.source_ip.is_empty() {
            self.bind_dev_ip.map(|ip| ip.to_string())
        } else {
            if let Some(bind) = self.bind_dev_ip {
                // satipc.cpp:396-397
                ms_log::log!(
                    "per-server source {} overrides --bind-dev source {}",
                    self.client.source_ip,
                    bind
                );
            }
            Some(self.client.source_ip.clone())
        };
        ConnectTarget {
            host: self.client.host.clone(),
            port: self.client.port,
            source,
            epoch: self.connect_epoch,
        }
    }

    /// `satipc_open_rtsp_socket`: connects and installs the RTSP socket.
    pub fn open_rtsp_socket(&mut self, is_init: bool) -> io::Result<()> {
        self.client.last_connect = self.client.tick();
        let target = self.prepare_connect_target();
        let stream = match connect(&target.host, target.port, target.source.as_deref()) {
            Ok(stream) => stream,
            // satipc.cpp:402-403
            Err(error) => ms_log::log_and_return!(
                Err(error),
                "could not connect to {}:{}, error {}",
                target.host,
                target.port,
                ms_log::strerror(&error)
            ),
        };
        self.finish_rtsp_connect(stream, is_init)
    }

    fn finish_rtsp_connect(&mut self, stream: TcpStream, is_init: bool) -> io::Result<()> {
        stream.set_nonblocking(true)?;
        let _ = stream.set_nodelay(true);
        if self.client.transport == Transport::Tcp {
            sys::set_receive_buffer(stream.as_raw_fd(), self.receive_buffer);
        }
        let fd = stream.as_raw_fd();
        self.clear_tx();
        self.rtsp = Some(stream);
        self.rbuf.clear();
        self.demux.reset();
        self.client.rtsp_socket_closed = false;
        self.client.rtsp_connected = true;
        self.client.fe_timeout_ms = 25_000;
        self.fe_rtime = self.client.tick();
        self.client.data_open =
            self.client.transport == Transport::Tcp || self.rtp.is_some() || self.has_srt_udp();
        if self.client.data_open && !is_init {
            self.client.post_init();
        }
        // satipc.cpp:427-430
        ms_log::log!(
            "satipc: connected to SAT>IP server {} port {} {} handle {} {} {}",
            self.client.host,
            self.client.port,
            self.client.transport.name(),
            fd,
            if self.client.source_ip.is_empty() {
                ""
            } else {
                "from source"
            },
            if self.client.source_ip.is_empty() {
                ""
            } else {
                self.client.source_ip.as_str()
            }
        );
        Ok(())
    }

    /// `satipc_setup_rtp_udp_sockets`: binds RTP and RTCP on consecutive ports.
    pub fn setup_rtp_udp_sockets(&mut self) -> io::Result<()> {
        if self.client.transport != Transport::Udp {
            return Ok(());
        }
        let port = u32::from(self.start_rtp) + 1000 + 2 * self.client.id as u32;
        let port = u16::try_from(port).map_err(|_| io::Error::other("RTP port out of range"))?;
        let rtp = match sys::udp_bind(self.bind_dev_ip, port) {
            Ok(socket) => socket,
            // satipc.cpp:818-819
            Err(error) => ms_log::log_and_return!(
                Err(error),
                "Could not listen on port {}: err {}: {}",
                port,
                ms_log::errno(&error),
                ms_log::strerror(&error)
            ),
        };
        let rtcp = match sys::udp_bind(self.bind_dev_ip, port.wrapping_add(1)) {
            Ok(socket) => socket,
            // satipc.cpp:826-827
            Err(error) => ms_log::log_and_return!(
                Err(error),
                "Could not listen on port {}: err {}: {}",
                port.wrapping_add(1),
                ms_log::errno(&error),
                ms_log::strerror(&error)
            ),
        };
        rtp.set_nonblocking(true)?;
        rtcp.set_nonblocking(true)?;
        sys::set_receive_buffer(rtp.as_raw_fd(), self.receive_buffer);
        self.client.listen_udp = port;
        self.rtp = Some(rtp);
        self.rtcp = Some(rtcp);
        Ok(())
    }

    #[cfg(feature = "srt")]
    fn has_srt_udp(&self) -> bool {
        self.srt_udp >= 0
    }

    #[cfg(not(feature = "srt"))]
    fn has_srt_udp(&self) -> bool {
        false
    }

    /// `satipc_open_srt`: reuses a connected SRT socket or connects a new one
    /// to the server's RTSP port with a fresh stream id; 0 on success, 2 on
    /// failure.
    #[cfg(feature = "srt")]
    pub fn open_srt(&mut self) -> i32 {
        use ms_srt::{SRT_INVALID_SOCK, SockOpt};

        if self.client.transport != Transport::Srt {
            return 0;
        }
        if self.srt_sock != SRT_INVALID_SOCK && ms_srt::is_connected(self.srt_sock) {
            // satipc.cpp:715-716
            ms_log::log!(
                "Reusing existing SRT socket {} for adapter {}",
                self.srt_sock,
                self.client.id
            );
            self.client.srt_connected = true;
            return 0;
        }
        self.client.srt_connected = false;
        if self.srt_sock != SRT_INVALID_SOCK {
            // satipc.cpp:723-724
            ms_log::log!("Closing old SRT socket {} (not connected)", self.srt_sock);
            // Not connected any more; srt_close also closes the acquired UDP fd.
            ms_srt::close(self.srt_sock);
            self.srt_sock = SRT_INVALID_SOCK;
        }
        // satipc.cpp:731-733; the stale UDP descriptor went with the SRT socket.
        if self.srt_udp >= 0 {
            ms_log::log!(
                "Closing stale UDP socket {} for adapter {}",
                self.srt_udp,
                self.client.id
            );
        }
        self.srt_udp = -1;

        srt_startup_once();
        let Ok(udp) = sys::udp_bind(self.bind_dev_ip, 0) else {
            // satipc.cpp:746
            ms_log::log_and_return!(2, "SRT UDP bind failed on ephemeral port");
        };
        let sock = ms_srt::create_socket();
        if sock == SRT_INVALID_SOCK {
            // satipc.cpp:751-752
            ms_log::log_and_return!(2, "srt_create_socket failed: {}", ms_srt::last_error());
        }
        self.srt_sock = sock;
        ms_srt::set_flag(sock, SockOpt::Latency, SRT_LATENCY_MS);
        // No SRTO_RENDEZVOUS: caller mode, blocking for the synchronous connect.
        self.client.srt_streamid = random_u32().to_string();
        ms_srt::set_streamid(sock, &self.client.srt_streamid);
        if !ms_srt::bind_acquire(sock, udp.as_raw_fd()) {
            // satipc.cpp:764
            ms_log::log!("srt_bind_acquire failed: {}", ms_srt::last_error());
            ms_srt::close(sock);
            self.srt_sock = SRT_INVALID_SOCK;
            self.client.srt_streamid.clear();
            return 2;
        }

        // fill_sockaddr: the server address in the family of the UDP socket.
        let ipv4 = udp.local_addr().is_ok_and(|local| local.is_ipv4());
        let address = (self.client.host.as_str(), self.client.port)
            .to_socket_addrs()
            .ok()
            .and_then(|mut addresses| addresses.find(|address| address.is_ipv4() == ipv4));
        // SRT now owns the UDP descriptor.
        self.srt_udp = udp.into_raw_fd();
        if !address.is_some_and(|address| ms_srt::connect(sock, address)) {
            // satipc.cpp:786-787
            ms_log::log!(
                "SRT connect to {}:{} failed: {}",
                self.client.host,
                self.client.port,
                ms_srt::last_error()
            );
            ms_srt::close(sock);
            self.srt_sock = SRT_INVALID_SOCK;
            self.client.srt_streamid.clear();
            // srt_close also closed the acquired UDP descriptor.
            self.srt_udp = -1;
            return 2;
        }
        // Non-blocking reads after the connect.
        ms_srt::set_flag(sock, SockOpt::RcvSyn, 0);
        self.client.srt_connected = true;
        // satipc.cpp:804-805
        ms_log::log!(
            "SRT connected for adapter {} to {}:{}, streamid={}, srt_sock={}",
            self.client.id,
            self.client.host,
            self.client.port,
            self.client.srt_streamid,
            sock
        );
        0
    }

    /// `satipc_abort_srt`: releases SRT state published while the adapter was
    /// closing; only the expected socket `fresh` is touched.
    #[cfg(feature = "srt")]
    pub fn abort_srt(&mut self, fresh: i32) {
        if self.srt_sock != fresh || fresh == ms_srt::SRT_INVALID_SOCK {
            return;
        }
        ms_srt::close(self.srt_sock);
        self.srt_sock = ms_srt::SRT_INVALID_SOCK;
        self.client.srt_streamid.clear();
        self.client.srt_connected = false;
        // The media entry (`ad->sock`) stays, now without a descriptor.
        self.srt_udp = -1;
    }

    // The SRT branch of `satipc_timeout`, after the claim.
    #[cfg(feature = "srt")]
    fn restart_srt(&mut self) {
        // satipc.cpp:539
        ms_log::log!(
            "SRT connection lost for adapter {}. Restarting SRT.",
            self.client.id
        );
        let fresh = if self.open_srt() == 0 && !self.device_open {
            self.srt_sock
        } else {
            ms_srt::SRT_INVALID_SOCK
        };
        self.abort_srt(fresh);
        self.client.srt_restart_claimed = false;
    }

    // Mirrors the lock-free `srt_socket_is_connected` the C++ checks inline.
    fn sync_srt(&mut self) {
        #[cfg(feature = "srt")]
        {
            self.client.srt_connected = ms_srt::is_connected(self.srt_sock);
        }
    }

    /// `satipc_drop_rtsp_socket`: releases an RTSP socket after a failed open.
    pub fn drop_rtsp_socket(&mut self) {
        self.clear_tx();
        self.rtsp = None;
        self.rbuf.clear();
        self.demux.reset();
        self.client.rtsp_connected = false;
        if self.client.transport == Transport::Tcp {
            self.client.data_open = false;
        }
        self.client.rtsp_socket_closed = true;
    }

    fn begin_open(&mut self) -> std::result::Result<ConnectTarget, i32> {
        let now = self.client.tick();
        if self.client.last_connect > 0 && now - self.client.last_connect < OPEN_BACKOFF_MS {
            return Err(3);
        }
        self.client.last_connect = now;
        self.connect_epoch = self.connect_epoch.wrapping_add(1);
        self.deferred_reconnect = false;
        self.sync_srt();
        Ok(self.prepare_connect_target())
    }

    fn finish_open_with_rtsp(&mut self, stream: TcpStream) -> i32 {
        if self.finish_rtsp_connect(stream, true).is_err() {
            self.drop_rtsp_socket();
            return 2;
        }
        #[cfg(feature = "srt")]
        if self.client.transport == Transport::Srt {
            let rv = self.open_srt();
            if rv != 0 {
                self.drop_rtsp_socket();
                return rv;
            }
        }
        if self.setup_rtp_udp_sockets().is_err() {
            self.drop_rtsp_socket();
            return -1;
        }
        self.client.data_open = true;
        self.client.reset_for_open();
        self.tcp_ts.clear();
        self.device_open = true;
        0
    }

    /// `satipc_open_device`: 0 on success, else the C++ return code.
    pub fn open_device(&mut self) -> i32 {
        let target = match self.begin_open() {
            Ok(target) => target,
            Err(rv) => return rv,
        };
        let stream = match connect(&target.host, target.port, target.source.as_deref()) {
            Ok(stream) => stream,
            Err(error) => {
                // satipc.cpp:402-403
                ms_log::log!(
                    "could not connect to {}:{}, error {}",
                    target.host,
                    target.port,
                    ms_log::strerror(&error)
                );
                return 2;
            }
        };
        self.finish_open_with_rtsp(stream)
    }

    /// `satip_close_device` plus the socket deletions.
    pub fn close_device(&mut self) {
        self.connect_epoch = self.connect_epoch.wrapping_add(1);
        self.deferred_reconnect = false;
        self.client.close_device();
        self.run_ops();
        self.clear_tx();
        self.rtsp = None;
        self.rtp = None;
        self.rtcp = None;
        self.rbuf.clear();
        self.demux.reset();
        self.tcp_ts.clear();
        self.client.rtsp_connected = false;
        self.client.data_open = false;
        self.client.ops.clear();
        // A dropped RestartSrt must not leave its claim behind.
        #[cfg(feature = "srt")]
        {
            self.client.srt_restart_claimed = false;
        }
        self.device_open = false;
        self.standby_since = None;
    }

    /// Executes the client's queued ops in order.
    pub fn run_ops(&mut self) {
        while let Some(op) = self.client.ops.pop_front() {
            match op {
                Op::Send(data) => {
                    if self.rtsp.is_some() {
                        if self.tx_buf.len().saturating_add(data.len()) > TX_BUF_MAX {
                            self.rtsp_eof();
                        } else {
                            self.tx_buf.extend_from_slice(&data);
                            if self.flush_tx().is_err() {
                                self.rtsp_eof();
                            }
                        }
                    }
                }
                Op::CloseData => {
                    // UDP keeps its RTP sockets so the restart can reuse the ports.
                    self.clear_tx();
                    self.rtsp = None;
                    self.rbuf.clear();
                    self.demux.reset();
                    self.client.rtsp_connected = false;
                }
                Op::CloseAdapter => {
                    if self.device_open {
                        self.close_device();
                    }
                    return;
                }
                Op::OpenRtsp => {
                    if self.defer_open_rtsp {
                        self.deferred_reconnect = true;
                    } else if self.open_rtsp_socket(false).is_err() {
                        self.client.rtsp_socket_closed = true;
                        self.client.ops.retain(|op| !matches!(op, Op::Send(_)));
                    }
                }
                #[cfg(feature = "srt")]
                Op::OpenSrt => {
                    if self.open_srt() == 0 {
                        self.client.request();
                    }
                }
                #[cfg(feature = "srt")]
                Op::RestartSrt => self.restart_srt(),
            }
        }
    }

    /// `satipc_timeout` with its socket work done.
    pub fn timeout(&mut self) -> i32 {
        self.sync_srt();
        let rv = self.client.timeout();
        self.fe_rtime = self.client.tick();
        self.run_ops();
        rv
    }

    // The RTSP connection ended or failed.
    fn rtsp_eof(&mut self) {
        // satipc.cpp:366-368; the socketworks id is not tracked.
        let fd = self.rtsp.as_ref().map_or(-1, |stream| stream.as_raw_fd());
        ms_log::log!(
            "satip_close called for adapter {}, socket_id {}, handle {} timeout {}",
            self.client.id,
            0,
            fd,
            self.client.fe_timeout_ms
        );
        self.clear_tx();
        self.rtsp = None;
        self.client.rtsp_connected = false;
        match self.client.transport {
            Transport::Tcp => {
                if self.client.can_keep_adapter_open {
                    self.client.keep_adapter_open = true;
                }
                self.client.data_open = false;
                self.client.on_data_closed();
            }
            Transport::Udp => self.client.ops.push_back(Op::CloseAdapter),
            #[cfg(feature = "srt")]
            Transport::Srt => self.client.ops.push_back(Op::CloseAdapter),
        }
    }

    fn handle_rtsp_bytes(&mut self, bytes: &[u8]) {
        match self.client.transport {
            Transport::Tcp => {
                // satipc.cpp:1201-1203
                ms_log::debugm!(
                    "start satipc_tcp_read tcp_pos {} tcp_len {} tcp_size {}, buffer len {}",
                    self.demux.position(),
                    self.demux.position() + self.demux.buffered(),
                    self.demux.capacity(),
                    bytes.len()
                );
                self.demux.push(bytes);
                self.drain_demux();
            }
            _ => {
                self.rbuf.extend_from_slice(bytes);
                if self.client.reply(&self.rbuf) != ReplyOutcome::Incomplete {
                    self.rbuf.clear();
                }
            }
        }
    }

    fn drain_demux(&mut self) {
        let before = self.tcp_ts.len();
        while self.tcp_ts.len() + 65_536 <= TCP_TS_MAX {
            let Some(frame) = self.demux.next_frame() else {
                break;
            };
            match frame {
                Frame::Rtsp(message) => {
                    // satipc.cpp:1299
                    ms_log::debugm!(
                        "ad {} processed {}, socket {}",
                        self.client.id,
                        message.len(),
                        0
                    );
                    let _ = self.client.reply(&message);
                }
                Frame::Rtcp(_) | Frame::Rtp(_) if self.client.ignore_packets => {}
                Frame::Rtcp(packet) => self.client.rtcp_reply(&packet),
                Frame::Rtp(payload) => {
                    let whole = payload.len() / TS_PACKET_SIZE * TS_PACKET_SIZE;
                    self.tcp_ts.extend_from_slice(&payload[..whole]);
                }
            }
        }
        // satipc.cpp:1370
        ms_log::debugm!(
            "satipc_tcp_read: returning {} bytes",
            self.tcp_ts.len() - before
        );
    }

    /// One I/O pass: sockets, timers and queued ops; `true` when data arrived.
    pub fn poll(&mut self) -> bool {
        if !self.device_open {
            return false;
        }
        self.sync_srt();
        let mut work = false;
        if !self.tx_buf.is_empty() {
            match self.flush_tx() {
                Ok(true) => work = true,
                Ok(false) => {}
                Err(_) => self.rtsp_eof(),
            }
        }
        let mut buf = [0u8; 16 * 1024];
        let room =
            self.client.transport != Transport::Tcp || self.tcp_ts.len() + 65_536 <= TCP_TS_MAX;
        if !room {
            // satipc.cpp:1249-1252; the interleaved buffer is `tcp_ts` here.
            ms_log::logm!(
                "buffer is full, skipping read, {}, pos {} [{:02X} {:02X}, {:02X} {:02X}]",
                self.tcp_ts.len(),
                0,
                self.tcp_ts.first().copied().unwrap_or(0),
                self.tcp_ts.get(1).copied().unwrap_or(0),
                self.tcp_ts.get(2).copied().unwrap_or(0),
                self.tcp_ts.get(3).copied().unwrap_or(0)
            );
        }
        if room && let Some(stream) = self.rtsp.as_mut() {
            let fd = stream.as_raw_fd();
            match stream.read(&mut buf) {
                Ok(0) => {
                    if self.client.transport == Transport::Tcp {
                        // satipc.cpp:1238-1239
                        ms_log::log!(
                            "read 0 from RTSP sock {}, handle {}, errno 0, {}",
                            0,
                            fd,
                            io::Error::from_raw_os_error(0)
                        );
                    }
                    self.rtsp_eof();
                }
                Ok(read) => {
                    work = true;
                    if self.client.transport != Transport::Tcp {
                        self.fe_rtime = self.client.tick();
                    } else {
                        // satipc.cpp:1232-1236; the socketworks id is not tracked.
                        ms_log::debugm!(
                            "read {} (from {}) from rtsp socket {} (id {}) [{:02X} {:02X}, {:02X} {:02X}]",
                            read,
                            buf.len(),
                            fd,
                            0,
                            buf.first().copied().unwrap_or(0),
                            buf.get(1).copied().unwrap_or(0),
                            buf.get(2).copied().unwrap_or(0),
                            buf.get(3).copied().unwrap_or(0)
                        );
                    }
                    self.handle_rtsp_bytes(&buf[..read]);
                }
                Err(error) if is_transient(&error) => {}
                Err(error) => {
                    if self.client.transport == Transport::Tcp {
                        // satipc.cpp:1238-1239
                        ms_log::log!(
                            "read -1 from RTSP sock {}, handle {}, errno {}, {}",
                            0,
                            fd,
                            ms_log::errno(&error),
                            ms_log::strerror(&error)
                        );
                    }
                    self.rtsp_eof();
                }
            }
        }
        if self.client.transport == Transport::Tcp {
            self.drain_demux();
        }
        if let Some(rtcp) = self.rtcp.as_ref()
            && let Ok(read) = rtcp.recv(&mut buf)
        {
            work = true;
            self.client.rtcp_reply(&buf[..read]);
        }
        let now = self.client.tick();
        if now - self.fe_rtime > self.client.fe_timeout_ms {
            self.timeout();
        }
        self.run_ops();
        if self.client.sid_cnt == 0
            && self.adapter_timeout_ms > 0
            && let Some(since) = self.standby_since
            && now - since > self.adapter_timeout_ms
        {
            self.close_device();
        }
        work
    }

    /// Reads RTP datagrams (UDP) or interleaved TS (TCP) into `buf`.
    pub fn read_media(&mut self, buf: &mut TsBuf) -> usize {
        match self.client.transport {
            Transport::Udp => {
                let Some(rtp) = self.rtp.as_ref() else {
                    return 0;
                };
                let mut datagram = [0u8; DATAGRAM_MAX];
                let mut total = 0;
                let mut messages = 0;
                for _ in 0..MAX_RTP_MSG {
                    if buf.remaining_bytes() < MAX_UDP_PACKET_SIZE {
                        break;
                    }
                    let read = match rtp.recv(&mut datagram) {
                        Ok(read) => read,
                        Err(error) => {
                            // satipc.cpp:1054-1055; a transient error is the
                            // normal "no datagram ready" case in Rust.
                            if !is_transient(&error) {
                                ms_log::debugm!("satipc_read: read error or zero datagrams");
                            }
                            break;
                        }
                    };
                    messages += 1;
                    total += process_rtp_datagram(&mut self.client, &datagram[..read], buf);
                }
                // satipc.cpp:1067-1068
                if messages == MAX_RTP_MSG {
                    ms_log::debugm!("satipc_read: read full, so flushing output");
                }
                total
            }
            Transport::Tcp => {
                let take =
                    self.tcp_ts.len().min(buf.remaining_bytes()) / TS_PACKET_SIZE * TS_PACKET_SIZE;
                let appended = push_packets(&self.tcp_ts[..take], buf);
                self.tcp_ts.drain(..appended);
                appended
            }
            #[cfg(feature = "srt")]
            Transport::Srt => self.read_srt(buf),
        }
    }

    /// `satipc_srt_read`: drains ready SRT messages while a whole one fits.
    #[cfg(feature = "srt")]
    fn read_srt(&mut self, buf: &mut TsBuf) -> usize {
        // Without the media entry (`ad->sock`) nothing reads, even though the
        // SRT socket itself is kept for reuse.
        if !self.client.data_open || !ms_srt::is_connected(self.srt_sock) {
            return 0;
        }
        let mut message = [0u8; MAX_UDP_PACKET_SIZE];
        let mut total = 0;
        while buf.remaining_bytes() >= MAX_UDP_PACKET_SIZE {
            // SRT_EASYNCRCV (nothing ready) and errors both end the pass.
            let read = ms_srt::recv(self.srt_sock, &mut message);
            let Ok(read) = usize::try_from(read) else {
                if ms_srt::last_error_code() != ms_srt::SRT_EASYNCRCV {
                    // satipc.cpp:697-699
                    ms_log::log!(
                        "SRT recv failed for adapter {}, socket {}: {}",
                        self.client.id,
                        self.srt_sock,
                        ms_srt::last_error()
                    );
                }
                break;
            };
            if read == 0 {
                break;
            }
            total += push_packets(&message[..read], buf);
        }
        total
    }

    /// Tunes, opening the device on first use (`init_hw` + `ad->tune`).
    pub fn tune(&mut self, params: &TuneParams) -> io::Result<()> {
        self.sync_srt();
        if !self.device_open {
            let rv = self.open_device();
            if rv != 0 {
                return Err(io::Error::other(format!(
                    "satipc: cannot open {}:{} (rc {rv})",
                    self.client.host, self.client.port
                )));
            }
            self.client.post_init();
            self.run_ops();
        }
        self.tune_open(params);
        Ok(())
    }

    fn tune_open(&mut self, params: &TuneParams) {
        let mut tp = params.clone();
        tp.pids.clear();
        self.client.tp = tp;
        self.client.sid_cnt = 1;
        self.standby_since = None;
        self.client.tune();
        self.run_ops();
    }

    fn take_pending_reconnect(&mut self) -> Option<ConnectTarget> {
        if !self.deferred_reconnect {
            return None;
        }
        self.deferred_reconnect = false;
        self.client.last_connect = self.client.tick();
        Some(self.prepare_connect_target())
    }

    /// Takes `ad->wait_new_stream` for the data plane.
    pub fn take_wait_new_stream(&mut self) -> bool {
        std::mem::take(&mut self.client.wait_new_stream)
    }

    /// Applies the adapter PID set and commits.
    pub fn set_pids(&mut self, pids: &PidSet) {
        if !self.device_open {
            return;
        }
        self.sync_srt();
        self.client.mark_pids(pids);
        self.client.update_pids();
        self.run_ops();
    }

    /// `close_adapter_for_stream` for the last stream: standby, then clear `tp`.
    pub fn standby(&mut self) {
        if !self.device_open || self.standby_since.is_some() {
            return;
        }
        self.sync_srt();
        self.client.standby();
        self.client.tp = TuneParams {
            msys: self.client.sys0,
            ..TuneParams::default()
        };
        self.client.sid_cnt = 0;
        self.standby_since = Some(self.client.tick());
        self.run_ops();
    }
}

// The SRT socket outlives `close_device` for reuse, like C++; it ends with
// the driver.
#[cfg(feature = "srt")]
impl Drop for Driver {
    fn drop(&mut self) {
        ms_srt::close(self.srt_sock);
    }
}

/// `srt_startup` once per process; the runtime may also start the library for
/// its SRT listener, which libsrt reference-counts.
#[cfg(feature = "srt")]
fn srt_startup_once() {
    static STARTUP: std::sync::Once = std::sync::Once::new();
    STARTUP.call_once(|| {
        ms_srt::startup();
    });
}

fn connect(host: &str, port: u16, source: Option<&str>) -> io::Result<TcpStream> {
    let mut last = io::Error::new(io::ErrorKind::NotFound, format!("cannot resolve {host}"));
    for address in (host, port).to_socket_addrs()? {
        match sys::tcp_connect_src(address, source, CONNECT_TIMEOUT) {
            Ok(stream) => return Ok(stream),
            Err(error) => last = error,
        }
    }
    Err(last)
}

fn is_transient(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted | io::ErrorKind::TimedOut
    )
}

type ConnectFn = Arc<dyn Fn(&str, u16, Option<&str>) -> io::Result<TcpStream> + Send + Sync>;

struct ExitSignal(Arc<(Mutex<bool>, Condvar)>);

impl Drop for ExitSignal {
    fn drop(&mut self) {
        let (lock, cvar) = &*self.0;
        *lock.lock().unwrap_or_else(PoisonError::into_inner) = true;
        cvar.notify_all();
    }
}

/// SAT>IP client tuner: the [`Tuner`] face of a [`Driver`].
pub struct SatipcTuner {
    adapter_id: usize,
    driver: Arc<Mutex<Driver>>,
    shared_status: Arc<Mutex<SignalStatus>>,
    stop: Arc<AtomicBool>,
    exited: Arc<(Mutex<bool>, Condvar)>,
    thread: Option<JoinHandle<()>>,
    connect_fn: ConnectFn,
}

impl SatipcTuner {
    /// Starts the I/O thread; the server is contacted on the first tune.
    pub fn new(config: &SatipcConfig) -> io::Result<Self> {
        Self::new_with_connector(config, Arc::new(connect))
    }

    fn new_with_connector(config: &SatipcConfig, connect_fn: ConnectFn) -> io::Result<Self> {
        let mut inner = Driver::new(config);
        inner.defer_open_rtsp = true;
        let initial_status = inner.signal_status();
        let driver = Arc::new(Mutex::new(inner));
        let shared_status = Arc::new(Mutex::new(initial_status));
        let stop = Arc::new(AtomicBool::new(false));
        let exited = Arc::new((Mutex::new(false), Condvar::new()));
        let thread = {
            let driver = Arc::clone(&driver);
            let shared_status = Arc::clone(&shared_status);
            let stop = Arc::clone(&stop);
            let exited = Arc::clone(&exited);
            let connect_fn = Arc::clone(&connect_fn);
            thread::Builder::new()
                .name(format!("satipc{}", config.adapter_id))
                .spawn(move || io_loop(&driver, &shared_status, &stop, &exited, &connect_fn))?
        };
        Ok(Self {
            adapter_id: config.adapter_id,
            driver,
            shared_status,
            stop,
            exited,
            thread: Some(thread),
            connect_fn,
        })
    }

    /// Shared driver state, for status pages and tests.
    pub fn driver(&self) -> MutexGuard<'_, Driver> {
        lock(&self.driver)
    }
}

fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value.lock().unwrap_or_else(PoisonError::into_inner)
}

fn io_loop(
    driver: &Mutex<Driver>,
    shared_status: &Mutex<SignalStatus>,
    stop: &AtomicBool,
    exited: &Arc<(Mutex<bool>, Condvar)>,
    connect_fn: &ConnectFn,
) {
    let _exit_signal = ExitSignal(Arc::clone(exited));
    while !stop.load(Ordering::SeqCst) {
        let (work, status, reconnect) = {
            let mut guard = lock(driver);
            let work = guard.poll();
            let status = guard.signal_status();
            let reconnect = guard.take_pending_reconnect();
            (work, status, reconnect)
        };
        *lock(shared_status) = status;
        if let Some(target) = reconnect {
            let connected = connect_fn(&target.host, target.port, target.source.as_deref());
            if !stop.load(Ordering::SeqCst) {
                let mut guard = lock(driver);
                if guard.connect_epoch == target.epoch && !stop.load(Ordering::SeqCst) {
                    match connected {
                        Ok(stream) => {
                            if let Err(error) = guard.finish_rtsp_connect(stream, false) {
                                ms_log::log!(
                                    "could not configure {}:{}, error {}",
                                    target.host,
                                    target.port,
                                    ms_log::strerror(&error)
                                );
                                guard.client.rtsp_socket_closed = true;
                                guard.client.ops.retain(|op| !matches!(op, Op::Send(_)));
                            } else {
                                guard.run_ops();
                            }
                        }
                        Err(error) => {
                            ms_log::log!(
                                "could not connect to {}:{}, error {}",
                                target.host,
                                target.port,
                                ms_log::strerror(&error)
                            );
                            guard.client.rtsp_socket_closed = true;
                            guard.client.ops.retain(|op| !matches!(op, Op::Send(_)));
                        }
                    }
                    *lock(shared_status) = guard.signal_status();
                }
            }
        } else if !work {
            thread::sleep(IO_IDLE);
        }
    }
}

impl Tuner for SatipcTuner {
    fn tune(&mut self, params: &TuneParams) -> Result<()> {
        let target = {
            let mut driver = lock(&self.driver);
            driver.sync_srt();
            if driver.device_open {
                driver.tune_open(params);
                *lock(&self.shared_status) = driver.signal_status();
                return Ok(());
            }
            driver.begin_open().map_err(|rv| {
                TunerError::Io(io::Error::other(format!(
                    "satipc: cannot open {}:{} (rc {rv})",
                    driver.client.host, driver.client.port
                )))
            })?
        };
        let stream = (self.connect_fn)(&target.host, target.port, target.source.as_deref())
            .map_err(|error| {
                ms_log::log!(
                    "could not connect to {}:{}, error {}",
                    target.host,
                    target.port,
                    ms_log::strerror(&error)
                );
                TunerError::Io(io::Error::other(format!(
                    "satipc: cannot open {}:{} (rc 2)",
                    target.host, target.port
                )))
            })?;
        let mut driver = lock(&self.driver);
        if self.stop.load(Ordering::SeqCst) || driver.connect_epoch != target.epoch {
            return Err(TunerError::Io(io::Error::other("satipc open cancelled")));
        }
        if !driver.device_open {
            let rv = driver.finish_open_with_rtsp(stream);
            if rv != 0 {
                *lock(&self.shared_status) = driver.signal_status();
                return Err(TunerError::Io(io::Error::other(format!(
                    "satipc: cannot open {}:{} (rc {rv})",
                    driver.client.host, driver.client.port
                ))));
            }
            driver.client.post_init();
            driver.run_ops();
        }
        driver.tune_open(params);
        *lock(&self.shared_status) = driver.signal_status();
        Ok(())
    }

    fn set_pids(&mut self, pids: &PidSet) -> Result<()> {
        let mut driver = lock(&self.driver);
        driver.set_pids(pids);
        *lock(&self.shared_status) = driver.signal_status();
        Ok(())
    }

    fn status(&self) -> SignalStatus {
        *lock(&self.shared_status)
    }

    fn read(&mut self, buf: &mut TsBuf) -> Result<usize> {
        Ok(lock(&self.driver).read_media(buf))
    }

    fn standby(&mut self) {
        let mut driver = lock(&self.driver);
        driver.standby();
        *lock(&self.shared_status) = driver.signal_status();
    }

    fn take_wait_new_stream(&mut self) -> bool {
        lock(&self.driver).take_wait_new_stream()
    }
}

impl Drop for SatipcTuner {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let Some(thread) = self.thread.take() else {
            return;
        };
        let (lock_ex, cvar) = &*self.exited;
        let mut exited = lock_ex.lock().unwrap_or_else(PoisonError::into_inner);
        let deadline = Instant::now() + DROP_JOIN_TIMEOUT;
        while !*exited {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                break;
            };
            let (guard, wait) = cvar
                .wait_timeout(exited, remaining)
                .unwrap_or_else(PoisonError::into_inner);
            exited = guard;
            if wait.timed_out() {
                break;
            }
        }
        if *exited {
            drop(exited);
            let _ = thread.join();
            let mut driver = lock(&self.driver);
            if driver.device_open {
                driver.close_device();
            }
        } else {
            ms_log::log!(
                "satipc adapter {} I/O thread did not exit within {} ms, detaching",
                self.adapter_id,
                DROP_JOIN_TIMEOUT.as_millis()
            );
            if let Ok(mut driver) = self.driver.try_lock()
                && driver.device_open
            {
                driver.close_device();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DeliverySystem;
    use std::net::TcpListener;

    fn test_config(port: u16) -> SatipcConfig {
        SatipcConfig {
            adapter_id: 99,
            server: SatipServer {
                host: "127.0.0.1".to_owned(),
                port,
                fe: -1,
                delsys: DeliverySystem::DvbS2,
                source_ip: String::new(),
                transport: Transport::Tcp,
                no_pids_all: false,
            },
            start_rtp: 6500,
            user_agent: "minisatip test".to_owned(),
            adapter_timeout_ms: 0,
            ignore_signal: false,
            bind_dev_ip: None,
            receive_buffer: 0,
        }
    }

    #[test]
    fn tune_releases_driver_lock_while_connecting() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let driver_slot: Arc<Mutex<Option<Arc<Mutex<Driver>>>>> = Arc::new(Mutex::new(None));
        let slot_ref = Arc::clone(&driver_slot);
        let lock_was_free = Arc::new(AtomicBool::new(false));
        let lock_free_ref = Arc::clone(&lock_was_free);

        let connector: ConnectFn = Arc::new(move |host, p, _src| {
            let free = slot_ref
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|drv| drv.try_lock().is_ok());
            lock_free_ref.store(free, Ordering::SeqCst);
            TcpStream::connect((host, p))
        });

        let mut tuner = SatipcTuner::new_with_connector(&test_config(port), connector).unwrap();
        *driver_slot.lock().unwrap() = Some(Arc::clone(&tuner.driver));
        assert!(tuner.tune(&TuneParams::default()).is_ok());
        assert!(
            lock_was_free.load(Ordering::SeqCst),
            "Mutex<Driver> was held across connect()"
        );
        let _ = tuner.status();
    }

    #[test]
    fn drop_returns_within_bounded_timeout_when_reconnect_stalled() {
        let entered = Arc::new((Mutex::new(false), Condvar::new()));
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let entered_c = Arc::clone(&entered);
        let release_c = Arc::clone(&release);

        let connector: ConnectFn = Arc::new(move |_, _, _| {
            {
                let (lock, cvar) = &*entered_c;
                *lock.lock().unwrap() = true;
                cvar.notify_all();
            }
            let (lock, cvar) = &*release_c;
            let mut go = lock.lock().unwrap();
            while !*go {
                let (guard, wait) = cvar.wait_timeout(go, Duration::from_secs(6)).unwrap();
                go = guard;
                if wait.timed_out() {
                    break;
                }
            }
            Err(io::Error::new(io::ErrorKind::TimedOut, "stalled"))
        });

        let tuner = SatipcTuner::new_with_connector(&test_config(554), connector).unwrap();
        {
            let mut drv = tuner.driver();
            drv.device_open = true;
            drv.deferred_reconnect = true;
        }
        {
            let (lock, cvar) = &*entered;
            let mut inside = lock.lock().unwrap();
            while !*inside {
                let (guard, wait) = cvar.wait_timeout(inside, Duration::from_secs(2)).unwrap();
                inside = guard;
                assert!(!wait.timed_out(), "io_loop did not enter connector");
            }
        }

        let start = Instant::now();
        drop(tuner);
        let elapsed = start.elapsed();
        assert!(
            elapsed >= DROP_JOIN_TIMEOUT - Duration::from_millis(200)
                && elapsed < Duration::from_millis(3000),
            "drop elapsed outside bounded window: {elapsed:?}"
        );

        let (lock, cvar) = &*release;
        *lock.lock().unwrap() = true;
        cvar.notify_all();
    }
}
