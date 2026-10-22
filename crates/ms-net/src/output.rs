//! Stream outputs (RTP/UDP, RTP over RTSP TCP, HTTP TS), ported from `flush_stream`,
//! `send_rtcp` and `stream_timeout`.

use crate::handler::{StreamId, StreamType};
use crate::rtcp::{RtcpInfo, build_rtcp};
use crate::rtp::{Batch, Framing, RtpState, Segment, packetize_for_stream};
use crate::socket::DEFAULT_FIFO_SIZE;
#[cfg(feature = "srt")]
use crate::srt::{SrtOps, send_ts_partial};
use crate::writeq::WriteQueue;
use ms_tuner::PidSet;
use std::fmt;
use std::io::{self, Write};
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::os::fd::AsRawFd;
#[cfg(feature = "srt")]
use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// The C++ `stream.cpp` uses `#define DEFAULT_LOG LOG_STREAM` for this module.
const DEFAULT_LOG: u32 = ms_log::LOG_STREAM;

/// Period of the per-stream timer (`sockets_timeout(st_sock, 200)`).
pub const STREAM_TIMER_MS: i64 = 200;
/// Idle time after which an empty RTP packet is sent.
pub const KEEPALIVE_MS: i64 = 1000;
/// Minimum interval between RTCP reports.
pub const RTCP_INTERVAL_MS: i64 = 200;

/// Writer half of a TCP connection handed to the data plane.
pub type Sink = Box<dyn Write + Send>;

/// Per-output settings chosen by the session layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputConfig {
    /// SSRC; C++ uses the stream's session id (`sid->ssrc`).
    pub ssrc: u32,
    /// First RTP sequence number.
    pub seq: u16,
    /// Write-queue FIFO size (`--max-sbuf`).
    pub fifo_size: usize,
}

impl OutputConfig {
    /// Config with the default 10 MiB FIFO.
    pub fn new(ssrc: u32) -> Self {
        Self {
            ssrc,
            seq: 0,
            fifo_size: DEFAULT_FIFO_SIZE,
        }
    }
}

/// Local and remote addresses for an RTP/UDP output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UdpTarget {
    /// Local bind address; port is the RTP port, RTCP uses port + 1.
    pub local: SocketAddr,
    /// Client RTP address; RTCP goes to port + 1.
    pub dest: SocketAddr,
    /// Multicast TTL; `None` keeps the kernel default as C++ does.
    pub ttl: Option<u32>,
}

/// Local RTP port for stream `sid` (`opts.start_rtp + sid * 2`).
pub fn rtp_server_port(start_rtp: u16, sid: i32) -> u16 {
    (i32::from(start_rtp) + sid * 2) as u16
}

enum Transport {
    Udp {
        rtp: UdpSocket,
        rtcp: UdpSocket,
    },
    Tcp {
        sink: Sink,
        remote: Option<SocketAddr>,
    },
    #[cfg(feature = "srt")]
    Srt {
        sock: i32,
        ops: Arc<dyn SrtOps>,
        remote: Option<SocketAddr>,
    },
}

/// `get_stream_rhost`: an address, or the empty string C++ returns without one.
#[derive(Debug, Clone, Copy)]
enum RemoteHost {
    Addr(IpAddr),
    Empty,
}

impl fmt::Display for RemoteHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Addr(ip) => write!(f, "{ip}"),
            Self::Empty => Ok(()),
        }
    }
}

/// Counters and addresses exposed to the session layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputStats {
    /// Output type.
    pub stream_type: StreamType,
    /// Next RTP sequence number (`RTP-Info: seq=`).
    pub seq: u16,
    /// SSRC.
    pub ssrc: u32,
    /// Successful flushes (`sid->sp`).
    pub packets: u32,
    /// Bytes written (`sid->sb`).
    pub octets: u32,
    /// Failed flushes (`failed_writes`).
    pub failed_writes: u32,
    /// Bytes dropped by the queue or a full UDP socket.
    pub overflow: u64,
    /// Bytes waiting in the write queue.
    pub buffered: u64,
    /// Lifetime bytes copied into the FIFO (`s->buffered_bytes`;
    /// socketworks.cpp:1505-1506). `calculate_bw` folds its per-window delta
    /// into `c_buffered` (stream.cpp:1308).
    pub fifo_written: u64,
    /// Lifetime bytes dropped by the FIFO (`dropped_bytes`;
    /// socketworks.cpp:1496).
    pub fifo_dropped: u64,
    /// Remote address (`get_stream_rhost`/`get_stream_rport`).
    pub remote: Option<SocketAddr>,
    /// Set after the first successful flush (`start_streaming`).
    pub started: bool,
}

/// One stream's output with its RTP state and write queue.
pub struct StreamOutput {
    transport: Transport,
    stream_type: StreamType,
    framing: Framing,
    /// Stream id used by the C++ log lines (`sid->sid`); set by the data plane.
    stream_id: StreamId,
    rtp: RtpState,
    queue: WriteQueue,
    udp_dropped: u64,
    wtime_ms: i64,
    rtcp_wtime_ms: i64,
    sp: u32,
    sb: u32,
    failed_writes: u32,
    started: bool,
    forced_close: bool,
}

impl StreamOutput {
    /// Binds RTP/RTCP sockets and connects them to the client (UDP `decode_transport`).
    pub fn udp(target: UdpTarget, config: OutputConfig, now_ms: i64) -> io::Result<Self> {
        Self::udp_with_options(target, config, now_ms, None, 0)
    }

    /// UDP `decode_transport` with `--bind-dev` and `--output-buffer`.
    ///
    /// `SO_BINDTODEVICE` is applied before bind by `udp_bind`, then RTP/RTCP
    /// get the send buffer and DSCP markings (stream.cpp:556-584).
    pub fn udp_with_options(
        target: UdpTarget,
        config: OutputConfig,
        now_ms: i64,
        bind_dev: Option<&str>,
        send_buffer: i32,
    ) -> io::Result<Self> {
        let rtp = match bind_connect(target.local, target.dest, target.ttl, bind_dev) {
            Ok(socket) => socket,
            Err(error) => {
                // stream.cpp:560.
                ms_log::log!(
                    "decode_transport failed: UDP connection on rtp port to {}:{} failed",
                    target.dest.ip(),
                    target.dest.port()
                );
                return Err(error);
            }
        };
        let rtcp_local = with_port(target.local, target.local.port().wrapping_add(1));
        let rtcp_dest = with_port(target.dest, target.dest.port().wrapping_add(1));
        let rtcp = match bind_connect(rtcp_local, rtcp_dest, target.ttl, bind_dev) {
            Ok(socket) => socket,
            Err(error) => {
                // stream.cpp:577.
                ms_log::log!(
                    "decode_transport failed: UDP connection on rtcp port to {}:{} failed",
                    target.dest.ip(),
                    target.dest.port().wrapping_add(1)
                );
                return Err(error);
            }
        };
        // udp_bind leaves RTP blocking with a 1.1 s timeout (set_linux_socket_timeout,
        // socketworks.cpp:374-386): a burst waits for send-buffer space instead of
        // being dropped. RTCP stays non-blocking for the `poll_rtcp` reads.
        rtp.set_nonblocking(false)?;
        rtp.set_write_timeout(Some(RTP_SOCKET_TIMEOUT))?;
        rtp.set_read_timeout(Some(RTP_SOCKET_TIMEOUT))?;
        let mut output = Self::udp_from_sockets(rtp, rtcp, config, now_ms);
        output.apply_udp_socket_options(send_buffer);
        Ok(output)
    }

    /// Applies `--output-buffer` and the DSCP markings to the RTP and RTCP
    /// sockets (`decode_transport`, stream.cpp:570-584).
    ///
    /// C++ marks RTP with priority 7 and RTCP with 6, and only sets the send
    /// buffer when `opts.output_buffer > 0`. Failures are log-only upstream.
    pub fn apply_udp_socket_options(&mut self, send_buffer: i32) {
        let Transport::Udp { rtp, rtcp } = &self.transport else {
            return;
        };
        for (socket, priority) in [(rtp, 7), (rtcp, 6)] {
            crate::sys::set_socket_send_buffer(socket.as_raw_fd(), send_buffer);
            crate::sys::set_socket_dscp(socket.as_raw_fd(), crate::sys::IPTOS_DSCP_EF, priority);
        }
    }

    /// RTP and RTCP descriptors, for socket-option diagnostics and tests.
    pub fn udp_sockets(&self) -> Option<(&UdpSocket, &UdpSocket)> {
        match &self.transport {
            Transport::Udp { rtp, rtcp } => Some((rtp, rtcp)),
            _ => None,
        }
    }

    /// Wraps already connected RTP and RTCP sockets.
    pub fn udp_from_sockets(
        rtp: UdpSocket,
        rtcp: UdpSocket,
        config: OutputConfig,
        now_ms: i64,
    ) -> Self {
        Self::new(
            Transport::Udp { rtp, rtcp },
            StreamType::RtspUdp,
            Framing::RtpUdp,
            config,
            now_ms,
        )
    }

    /// RTP interleaved on the RTSP connection (`RTP/AVP/TCP`).
    pub fn rtsp_tcp(
        sink: Sink,
        remote: Option<SocketAddr>,
        config: OutputConfig,
        now_ms: i64,
    ) -> Self {
        Self::new(
            Transport::Tcp { sink, remote },
            StreamType::RtspTcp,
            Framing::RtpTcp,
            config,
            now_ms,
        )
    }

    /// Raw TS on an HTTP connection whose response header was already sent.
    pub fn http(sink: Sink, remote: Option<SocketAddr>, config: OutputConfig, now_ms: i64) -> Self {
        Self::new(
            Transport::Tcp { sink, remote },
            StreamType::Http,
            Framing::Raw,
            config,
            now_ms,
        )
    }

    /// Raw TS over an accepted SRT connection (`STREAM_RTSP_SRT`).
    #[cfg(feature = "srt")]
    pub fn srt(
        sock: i32,
        ops: Arc<dyn SrtOps>,
        remote: Option<SocketAddr>,
        config: OutputConfig,
        now_ms: i64,
    ) -> Self {
        Self::new(
            Transport::Srt { sock, ops, remote },
            StreamType::RtspSrt,
            Framing::Raw,
            config,
            now_ms,
        )
    }

    fn new(
        transport: Transport,
        stream_type: StreamType,
        framing: Framing,
        config: OutputConfig,
        now_ms: i64,
    ) -> Self {
        Self {
            transport,
            stream_type,
            framing,
            stream_id: -1,
            rtp: RtpState::new(config.ssrc, config.seq),
            queue: WriteQueue::new(config.fifo_size),
            udp_dropped: 0,
            wtime_ms: now_ms,
            rtcp_wtime_ms: now_ms,
            sp: 0,
            sb: 0,
            failed_writes: 0,
            started: false,
            forced_close: false,
        }
    }

    /// Output type.
    pub fn stream_type(&self) -> StreamType {
        self.stream_type
    }

    /// Records the stream index used by the C++ log lines (`sid->sid`).
    pub fn set_stream_id(&mut self, stream_id: StreamId) {
        self.stream_id = stream_id;
    }

    /// Remote destination, as `get_stream_rhost`/`get_stream_rport` report it.
    pub fn remote_addr(&self) -> Option<SocketAddr> {
        match &self.transport {
            Transport::Udp { rtp, .. } => rtp.peer_addr().ok(),
            Transport::Tcp { remote, .. } => *remote,
            #[cfg(feature = "srt")]
            Transport::Srt { remote, .. } => *remote,
        }
    }

    /// `sid->rsock`: the RTP socket handle where one exists.
    fn rtp_handle(&self) -> i32 {
        match &self.transport {
            Transport::Udp { rtp, .. } => rtp.as_raw_fd(),
            // The TCP sink is type-erased (`Box<dyn Write>`), so no fd.
            Transport::Tcp { .. } => -1,
            #[cfg(feature = "srt")]
            Transport::Srt { sock, .. } => *sock,
        }
    }

    /// Logs the sockets C++ `close_stream` force-closes (stream.cpp:376,385).
    ///
    /// The C++ `rsock_id`/`rtcp_sock` socket-table indices have no Rust
    /// counterpart and print -1.
    pub fn log_closing_sockets(&self) {
        if let Transport::Udp { rtp, rtcp } = &self.transport {
            ms_log::log!("Closing RTP sock {} handle {}", -1, rtp.as_raw_fd());
            ms_log::log!("Closing RTCP sock {} handle {}", -1, rtcp.as_raw_fd());
        }
    }

    /// `get_stream_rhost`/`get_stream_rport` for the log lines below.
    fn remote_parts(&self) -> (RemoteHost, u16) {
        match self.remote_addr() {
            Some(addr) => (RemoteHost::Addr(addr.ip()), addr.port()),
            None => (RemoteHost::Empty, 0),
        }
    }

    /// Follows the RTP session id (`set_session_id` assigns `sid->ssrc`;
    /// stream.cpp:1409-1416).
    pub fn set_ssrc(&mut self, ssrc: u32) {
        self.rtp.ssrc = ssrc;
    }

    /// Sets `wtime`/`rtcp_wtime`. C++ initialises both in `streams_add`
    /// (stream.cpp:630-636), before PLAY creates the output; the data plane
    /// passes the SETUP instant in its own clock.
    pub fn set_clocks(&mut self, now_ms: i64) {
        self.wtime_ms = now_ms;
        self.rtcp_wtime_ms = now_ms;
    }

    /// Current counters.
    pub fn stats(&self) -> OutputStats {
        let remote = match &self.transport {
            Transport::Udp { rtp, .. } => rtp.peer_addr().ok(),
            Transport::Tcp { remote, .. } => *remote,
            #[cfg(feature = "srt")]
            Transport::Srt { remote, .. } => *remote,
        };
        OutputStats {
            stream_type: self.stream_type,
            seq: self.rtp.seq,
            ssrc: self.rtp.ssrc,
            packets: self.sp,
            octets: self.sb,
            failed_writes: self.failed_writes,
            overflow: self.queue.overflow() + self.udp_dropped,
            buffered: self.queue.fifo().used(),
            fifo_written: self.queue.buffered_bytes(),
            fifo_dropped: self.queue.overflow(),
            remote,
            started: self.started,
        }
    }

    /// Reconnects the UDP sockets to a new client port, keeping RTP state.
    pub fn set_udp_destination(&mut self, dest: SocketAddr) -> io::Result<()> {
        match &self.transport {
            Transport::Udp { rtp, rtcp } => {
                rtp.connect(dest)?;
                rtcp.connect(with_port(dest, dest.port().wrapping_add(1)))
            }
            Transport::Tcp { .. } => Err(io::Error::from(io::ErrorKind::Unsupported)),
            #[cfg(feature = "srt")]
            Transport::Srt { .. } => Err(io::Error::from(io::ErrorKind::Unsupported)),
        }
    }

    /// Whether a failed SRT send requested a forced close (`sid->timeout = 1`).
    pub fn forced_close(&self) -> bool {
        self.forced_close
    }

    /// Sends the stream's packets from one adapter buffer (`process_packets_for_stream`).
    pub fn process(
        &mut self,
        buf: &[u8],
        pids: &PidSet,
        ctime_ms: i64,
        mono_us: i64,
    ) -> io::Result<()> {
        let batches = packetize_for_stream(
            &mut self.rtp,
            self.framing,
            buf,
            |pid| pids.contains(pid),
            mono_us,
            self.stream_id,
        );
        if let Some(last) = batches.last() {
            // stream.cpp:1144 (`LOGM` under `#define DEFAULT_LOG LOG_DMX`).
            let pos = last.segments.len();
            let rtp_header = last
                .segments
                .iter()
                .rposition(|segment| matches!(segment, Segment::Header { .. }))
                .unwrap_or(0);
            let total_len: usize = last
                .segments
                .iter()
                .filter_map(|segment| match segment {
                    Segment::Data { len, .. } => Some(*len),
                    Segment::Header { .. } => None,
                })
                .sum();
            let rtp_pos: usize = last
                .segments
                .iter()
                .filter_map(|segment| match segment {
                    Segment::Header { len, .. } => Some(*len),
                    Segment::Data { .. } => None,
                })
                .sum();
            ms_log::logl!(
                ms_log::LOG_DMX,
                "Processing done sid {} at pos {}, rtp header {}, rtp_pos {}, total_len {}, rlen {}",
                self.stream_id,
                pos,
                rtp_header,
                rtp_pos,
                total_len,
                buf.len()
            );
        }
        for batch in &batches {
            self.flush_stream(batch, buf, ctime_ms)?;
        }
        Ok(())
    }

    /// Writes one batch and updates `wtime`, `sp` and `sb` (`flush_stream`).
    pub fn flush_stream(&mut self, batch: &Batch, buf: &[u8], ctime_ms: i64) -> io::Result<i32> {
        let rv = match &mut self.transport {
            Transport::Udp { rtp, .. } => {
                let mut sent = 0i32;
                let mut timed_out = false;
                for datagram in batch.datagrams(buf) {
                    // One sendmmsg per batch in C++ (socketworks.cpp:1280): after a
                    // send timeout the rest of the batch is dropped, not retried.
                    if timed_out {
                        self.udp_dropped += datagram.len() as u64;
                        continue;
                    }
                    match rtp.send(&datagram) {
                        Ok(count) => sent += count as i32,
                        Err(error) => {
                            self.failed_writes = self.failed_writes.wrapping_add(1);
                            if error.kind() == io::ErrorKind::WouldBlock {
                                self.udp_dropped += datagram.len() as u64;
                                timed_out = true;
                            } else if matches!(
                                error.kind(),
                                io::ErrorKind::ConnectionRefused | io::ErrorKind::BrokenPipe
                            ) {
                                // socketworks.cpp:1334-1343: close the RTP stream.
                                self.forced_close = true;
                            }
                        }
                    }
                }
                sent
            }
            Transport::Tcp { sink, .. } => {
                let chunks = batch.chunks(buf);
                let rv = self.queue.write_vectored(sink.as_mut(), &chunks, false);
                if rv < 0 {
                    return Err(io::Error::from(io::ErrorKind::BrokenPipe));
                }
                rv
            }
            #[cfg(feature = "srt")]
            Transport::Srt { sock, ops, .. } => {
                // stream.cpp:800-823: each iovec is sent in chunks of at most
                // MAX_UDP_PACKET_SIZE bytes; a failure forces the stream closed.
                let mut sent = 0usize;
                for chunk in batch.chunks(buf) {
                    let (count, failed) = send_ts_partial(ops.as_ref(), *sock, chunk);
                    sent += count;
                    if failed {
                        // stream.cpp:817. `SrtOps` exposes no
                        // `srt_getlasterror_str`; the stable text stands in.
                        ms_log::log!(
                            "SRT send failed for sid {}: {}",
                            self.stream_id,
                            crate::srt::SrtError::Send
                        );
                        self.forced_close = true;
                        // stream.cpp:824-829: a partial flush is not a failed write.
                        if sent == 0 {
                            self.failed_writes = self.failed_writes.wrapping_add(1);
                        }
                        break;
                    }
                }
                sent as i32
            }
        };
        let (host, port) = self.remote_parts();
        if rv > 0 && !self.started {
            // stream.cpp:838.
            ms_log::log!(
                "Start streaming for stream sid {}, len {} to handle {} => {}:{}",
                self.stream_id,
                rv,
                self.rtp_handle(),
                host,
                port
            );
        }
        // stream.cpp:845 (`DEBUGM`; `sock_id` is the socket-table index).
        ms_log::debugm!(
            "flush_stream: sent {} bytes for sid {}, handle {}, sock_id {}, seq {} => {}:{}",
            rv,
            self.stream_id,
            self.rtp_handle(),
            -1,
            self.rtp.seq,
            host,
            port
        );
        if rv > 0 {
            self.started = true;
            self.sp = self.sp.wrapping_add(1);
            self.sb = self.sb.wrapping_add(rv as u32);
        }
        self.wtime_ms = ctime_ms;
        Ok(rv)
    }

    /// The 200 ms stream timer: keepalive RTP after 1 s idle, then RTCP (`stream_timeout`).
    pub fn on_timer<F: FnOnce() -> String>(
        &mut self,
        ctime_ms: i64,
        mono_us: i64,
        wall: SystemTime,
        describe: F,
    ) -> io::Result<()> {
        if self.framing == Framing::Raw {
            return Ok(());
        }
        if ctime_ms - self.wtime_ms > KEEPALIVE_MS {
            let (host, port) = self.remote_parts();
            // stream.cpp:1357 (`stream_timeout`).
            ms_log::log!(
                "no data sent for more than 1s sid: {} for {}:{}",
                self.stream_id,
                host,
                port
            );
            let mut batch = Batch::default();
            let start = batch.headers.len();
            let len = self
                .rtp
                .write_header(&mut batch.headers, self.framing, 0, mono_us);
            batch
                .segments
                .push(crate::rtp::Segment::Header { start, len });
            self.flush_stream(&batch, &[], ctime_ms)?;
        }
        if ctime_ms - self.rtcp_wtime_ms >= RTCP_INTERVAL_MS {
            self.send_rtcp(ctime_ms, wall, &describe())?;
        }
        Ok(())
    }

    /// Sends one RTCP compound packet (`send_rtcp`).
    pub fn send_rtcp(
        &mut self,
        ctime_ms: i64,
        wall: SystemTime,
        description: &str,
    ) -> io::Result<()> {
        let info = RtcpInfo {
            ssrc: self.rtp.ssrc,
            wall,
            ctime_ms,
            wtime_ms: self.wtime_ms,
            packets: self.sp,
            octets: self.sb,
        };
        let interleaved = self.framing == Framing::RtpTcp;
        let packet = build_rtcp(&info, description, interleaved);
        match &mut self.transport {
            Transport::Udp { rtcp, .. } => match rtcp.send(&packet) {
                Ok(_) => {}
                Err(error) => {
                    self.failed_writes = self.failed_writes.wrapping_add(1);
                    if error.kind() == io::ErrorKind::WouldBlock {
                        self.udp_dropped += packet.len() as u64;
                    }
                    // socketworks.cpp:1334-1335: RTCP sockets are not force-closed.
                }
            },
            Transport::Tcp { sink, .. } => {
                if self.queue.write(sink.as_mut(), &packet, false) < 0 {
                    return Err(io::Error::from(io::ErrorKind::BrokenPipe));
                }
            }
            #[cfg(feature = "srt")]
            Transport::Srt { .. } => {}
        }
        self.rtcp_wtime_ms = ctime_ms;
        let (host, port) = self.remote_parts();
        // stream.cpp:785 (`DEBUGM`; `handle` is `sid->rsock`).
        ms_log::debugm!(
            "send_rtcp: sent {} bytes for sid {}, handle {} seq {} => {}:{}",
            packet.len(),
            self.stream_id,
            self.rtp_handle(),
            i32::from(self.rtp.seq) - 1,
            host,
            port
        );
        Ok(())
    }
    pub fn write_priority(&mut self, data: &[u8]) -> io::Result<i32> {
        match &mut self.transport {
            Transport::Tcp { sink, .. } => {
                let rv = self.queue.write(sink.as_mut(), data, true);
                if rv < 0 {
                    return Err(io::Error::from(io::ErrorKind::BrokenPipe));
                }
                Ok(rv)
            }
            Transport::Udp { .. } => Err(io::Error::from(io::ErrorKind::Unsupported)),
            #[cfg(feature = "srt")]
            Transport::Srt { .. } => Err(io::Error::from(io::ErrorKind::Unsupported)),
        }
    }

    /// Requests that queued data be dropped up to the next RTSP header
    /// (`sockets_set_flush_enqued_data`, stream.cpp:374).
    pub fn set_flush_enqueued_data(&mut self) {
        self.queue.set_flush_enqueued_data(true);
    }

    /// Flushes queued TCP data (`flush_socket`); UDP never queues.
    pub fn flush_queue(&mut self) -> io::Result<i32> {
        match &mut self.transport {
            Transport::Tcp { sink, .. } => {
                let rv = self.queue.flush(sink.as_mut());
                if rv < 0 {
                    return Err(io::Error::from(io::ErrorKind::BrokenPipe));
                }
                Ok(rv)
            }
            Transport::Udp { .. } => Ok(0),
            #[cfg(feature = "srt")]
            Transport::Srt { .. } => Ok(0),
        }
    }

    /// Whether the queue holds data waiting to be flushed.
    pub fn has_pending(&self) -> bool {
        self.queue.fifo().used() > 0 || self.queue.prio_len() > 0
    }

    /// Reads pending RTCP receiver reports; `true` when one arrived (`rtcp_confirm`).
    pub fn poll_rtcp(&mut self) -> bool {
        let Transport::Udp { rtcp, .. } = &self.transport else {
            return false;
        };
        let mut buf = [0u8; 1500];
        let mut seen = false;
        while rtcp.recv(&mut buf).is_ok() {
            seen = true;
        }
        seen
    }
}

/// `SO_SNDTIMEO`/`SO_RCVTIMEO` of `set_linux_socket_timeout` (socketworks.cpp:374-386).
const RTP_SOCKET_TIMEOUT: Duration = Duration::from_millis(1100);

fn with_port(addr: SocketAddr, port: u16) -> SocketAddr {
    SocketAddr::new(addr.ip(), port)
}

fn bind_connect(
    local: SocketAddr,
    dest: SocketAddr,
    ttl: Option<u32>,
    bind_dev: Option<&str>,
) -> io::Result<UdpSocket> {
    // udp_bind sets SO_REUSEADDR/SO_REUSEPORT and SO_BINDTODEVICE before bind
    // (socketworks.cpp:250-322).
    let socket = crate::sys::udp_bind_reuse(local, bind_dev)?;
    socket.connect(dest)?;
    socket.set_nonblocking(true)?;
    if let Some(ttl) = ttl {
        match dest.ip() {
            IpAddr::V4(ip) if ip.is_multicast() => socket.set_multicast_ttl_v4(ttl)?,
            _ => {}
        }
    }
    Ok(socket)
}
