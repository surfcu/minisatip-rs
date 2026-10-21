//! Socket driver for [`Client`] and the [`SatipcTuner`] backend.
//!
//! One I/O thread per adapter reads the RTSP (and, for UDP, RTCP) sockets and
//! runs the timers; the data-plane thread reads RTP or drains interleaved TS.

use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use super::client::{Client, Op, ReplyOutcome, Transport};
use super::config::SatipServer;
use super::media::{
    Frame, MAX_RTP_MSG, MAX_UDP_PACKET_SIZE, TcpDemux, process_rtp_datagram, push_packets,
};
use crate::tuner::{
    PidSet, Result, SignalStatus, TS_PACKET_SIZE, TsBuf, TuneParams, Tuner, TunerError,
};

/// Reconnect backoff of `satipc_open_device`.
const OPEN_BACKOFF_MS: i64 = 5_000;
/// Connect timeout per resolved address.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// I/O thread sleep when no socket had data.
const IO_IDLE: Duration = Duration::from_millis(5);
/// Interleaved TS kept for the data plane (`TCP_DATA_MAX`-like bound).
const TCP_TS_MAX: usize = 4 * 2000 * TS_PACKET_SIZE;
/// Largest UDP datagram read (header plus `MAX_UDP_PACKET_SIZE`).
const DATAGRAM_MAX: usize = 2048;

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
        Self {
            client,
            rtsp: None,
            rtp: None,
            rtcp: None,
            device_open: false,
            tcp_ts: Vec::new(),
            start_rtp: config.start_rtp,
            rbuf: Vec::new(),
            demux: TcpDemux::new(),
            fe_rtime: 0,
            standby_since: None,
            adapter_timeout_ms: config.adapter_timeout_ms,
        }
    }

    /// `satipc_open_rtsp_socket`: connects and installs the RTSP socket.
    pub fn open_rtsp_socket(&mut self, is_init: bool) -> io::Result<()> {
        self.client.last_connect = self.client.tick();
        let stream = connect(&self.client.host, self.client.port)?;
        stream.set_nonblocking(true)?;
        let _ = stream.set_nodelay(true);
        self.rtsp = Some(stream);
        self.rbuf.clear();
        self.demux.reset();
        self.client.rtsp_socket_closed = false;
        self.client.rtsp_connected = true;
        self.client.fe_timeout_ms = 25_000;
        self.fe_rtime = self.client.tick();
        self.client.data_open = self.client.transport == Transport::Tcp || self.rtp.is_some();
        if self.client.data_open && !is_init {
            self.client.post_init();
        }
        Ok(())
    }

    /// `satipc_setup_rtp_udp_sockets`: binds RTP and RTCP on consecutive ports.
    pub fn setup_rtp_udp_sockets(&mut self) -> io::Result<()> {
        if self.client.transport != Transport::Udp {
            return Ok(());
        }
        let port = u32::from(self.start_rtp) + 1000 + 2 * self.client.id as u32;
        let port = u16::try_from(port).map_err(|_| io::Error::other("RTP port out of range"))?;
        let rtp = UdpSocket::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)))?;
        let rtcp = UdpSocket::bind(SocketAddr::from((
            Ipv4Addr::UNSPECIFIED,
            port.wrapping_add(1),
        )))?;
        rtp.set_nonblocking(true)?;
        rtcp.set_nonblocking(true)?;
        self.client.listen_udp = port;
        self.rtp = Some(rtp);
        self.rtcp = Some(rtcp);
        Ok(())
    }

    /// `satipc_drop_rtsp_socket`: releases an RTSP socket after a failed open.
    pub fn drop_rtsp_socket(&mut self) {
        self.rtsp = None;
        self.rbuf.clear();
        self.demux.reset();
        self.client.rtsp_connected = false;
        if self.client.transport == Transport::Tcp {
            self.client.data_open = false;
        }
        self.client.rtsp_socket_closed = true;
    }

    /// `satipc_open_device`: 0 on success, else the C++ return code.
    pub fn open_device(&mut self) -> i32 {
        let now = self.client.tick();
        if self.client.last_connect > 0 && now - self.client.last_connect < OPEN_BACKOFF_MS {
            return 3;
        }
        self.client.last_connect = now;
        if self.open_rtsp_socket(true).is_err() {
            return 2;
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

    /// `satip_close_device` plus the socket deletions.
    pub fn close_device(&mut self) {
        self.client.close_device();
        self.run_ops();
        self.rtsp = None;
        self.rtp = None;
        self.rtcp = None;
        self.rbuf.clear();
        self.demux.reset();
        self.tcp_ts.clear();
        self.client.rtsp_connected = false;
        self.client.data_open = false;
        self.client.ops.clear();
        self.device_open = false;
        self.standby_since = None;
    }

    /// Executes the client's queued ops in order.
    pub fn run_ops(&mut self) {
        while let Some(op) = self.client.ops.pop_front() {
            match op {
                Op::Send(data) => {
                    let failed = match self.rtsp.as_mut() {
                        Some(stream) => write_all_nonblocking(stream, &data).is_err(),
                        None => false,
                    };
                    if failed {
                        self.rtsp_eof();
                    }
                }
                Op::CloseData => {
                    // UDP keeps its RTP sockets so the restart can reuse the ports.
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
                    if self.open_rtsp_socket(false).is_err() {
                        self.client.rtsp_socket_closed = true;
                        self.client.ops.retain(|op| !matches!(op, Op::Send(_)));
                    }
                }
            }
        }
    }

    /// `satipc_timeout` with its socket work done.
    pub fn timeout(&mut self) -> i32 {
        let rv = self.client.timeout();
        self.fe_rtime = self.client.tick();
        self.run_ops();
        rv
    }

    // The RTSP connection ended or failed.
    fn rtsp_eof(&mut self) {
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
        }
    }

    fn handle_rtsp_bytes(&mut self, bytes: &[u8]) {
        match self.client.transport {
            Transport::Udp => {
                self.rbuf.extend_from_slice(bytes);
                if self.client.reply(&self.rbuf) != ReplyOutcome::Incomplete {
                    self.rbuf.clear();
                }
            }
            Transport::Tcp => {
                self.demux.push(bytes);
                self.drain_demux();
            }
        }
    }

    fn drain_demux(&mut self) {
        while self.tcp_ts.len() + 65_536 <= TCP_TS_MAX {
            let Some(frame) = self.demux.next_frame() else {
                break;
            };
            match frame {
                Frame::Rtsp(message) => {
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
    }

    /// One I/O pass: sockets, timers and queued ops; `true` when data arrived.
    pub fn poll(&mut self) -> bool {
        if !self.device_open {
            return false;
        }
        let mut work = false;
        let mut buf = [0u8; 16 * 1024];
        let room =
            self.client.transport == Transport::Udp || self.tcp_ts.len() + 65_536 <= TCP_TS_MAX;
        if room && let Some(stream) = self.rtsp.as_mut() {
            match stream.read(&mut buf) {
                Ok(0) => self.rtsp_eof(),
                Ok(read) => {
                    work = true;
                    if self.client.transport == Transport::Udp {
                        self.fe_rtime = self.client.tick();
                    }
                    self.handle_rtsp_bytes(&buf[..read]);
                }
                Err(error) if is_transient(&error) => {}
                Err(_) => self.rtsp_eof(),
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
                for _ in 0..MAX_RTP_MSG {
                    if buf.remaining_bytes() < MAX_UDP_PACKET_SIZE {
                        break;
                    }
                    let Ok(read) = rtp.recv(&mut datagram) else {
                        break;
                    };
                    total += process_rtp_datagram(&mut self.client, &datagram[..read], buf);
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
        }
    }

    /// Tunes, opening the device on first use (`init_hw` + `ad->tune`).
    pub fn tune(&mut self, params: &TuneParams) -> io::Result<()> {
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
        let mut tp = params.clone();
        tp.pids.clear();
        self.client.tp = tp;
        self.client.sid_cnt = 1;
        self.standby_since = None;
        self.client.tune();
        self.run_ops();
        Ok(())
    }

    /// Applies the adapter PID set and commits.
    pub fn set_pids(&mut self, pids: &PidSet) {
        if !self.device_open {
            return;
        }
        self.client.mark_pids(pids);
        self.client.update_pids();
        self.run_ops();
    }

    /// `close_adapter_for_stream` for the last stream: standby, then clear `tp`.
    pub fn standby(&mut self) {
        if !self.device_open {
            return;
        }
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

fn connect(host: &str, port: u16) -> io::Result<TcpStream> {
    let mut last = io::Error::new(io::ErrorKind::NotFound, format!("cannot resolve {host}"));
    for address in (host, port).to_socket_addrs()? {
        match TcpStream::connect_timeout(&address, CONNECT_TIMEOUT) {
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

fn write_all_nonblocking(stream: &mut TcpStream, mut data: &[u8]) -> io::Result<()> {
    let mut spins = 0;
    while !data.is_empty() {
        match stream.write(data) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(written) => data = &data[written..],
            Err(error) if is_transient(&error) && spins < 1000 => {
                spins += 1;
                thread::sleep(Duration::from_millis(1));
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// SAT>IP client tuner: the [`Tuner`] face of a [`Driver`].
pub struct SatipcTuner {
    driver: Arc<Mutex<Driver>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl SatipcTuner {
    /// Starts the I/O thread; the server is contacted on the first tune.
    pub fn new(config: &SatipcConfig) -> io::Result<Self> {
        let driver = Arc::new(Mutex::new(Driver::new(config)));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let driver = Arc::clone(&driver);
            let stop = Arc::clone(&stop);
            thread::Builder::new()
                .name(format!("satipc{}", config.adapter_id))
                .spawn(move || io_loop(&driver, &stop))?
        };
        Ok(Self {
            driver,
            stop,
            thread: Some(thread),
        })
    }

    /// Shared driver state, for status pages and tests.
    pub fn driver(&self) -> MutexGuard<'_, Driver> {
        lock(&self.driver)
    }
}

fn lock(driver: &Mutex<Driver>) -> MutexGuard<'_, Driver> {
    driver
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn io_loop(driver: &Mutex<Driver>, stop: &AtomicBool) {
    while !stop.load(Ordering::SeqCst) {
        let work = lock(driver).poll();
        if !work {
            thread::sleep(IO_IDLE);
        }
    }
}

impl Tuner for SatipcTuner {
    fn tune(&mut self, params: &TuneParams) -> Result<()> {
        lock(&self.driver).tune(params).map_err(TunerError::Io)
    }

    fn set_pids(&mut self, pids: &PidSet) -> Result<()> {
        lock(&self.driver).set_pids(pids);
        Ok(())
    }

    fn status(&self) -> SignalStatus {
        let driver = lock(&self.driver);
        let client = &driver.client;
        SignalStatus {
            locked: client.status,
            strength: u16::try_from(client.strength.clamp(0, 255)).unwrap_or(0),
            snr: u16::try_from(client.snr.clamp(0, 255)).unwrap_or(0),
        }
    }

    fn read(&mut self, buf: &mut TsBuf) -> Result<usize> {
        Ok(lock(&self.driver).read_media(buf))
    }

    fn standby(&mut self) {
        lock(&self.driver).standby();
    }
}

impl Drop for SatipcTuner {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let mut driver = lock(&self.driver);
        if driver.device_open {
            driver.close_device();
        }
    }
}
