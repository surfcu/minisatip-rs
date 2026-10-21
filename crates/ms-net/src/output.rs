//! Stream outputs (RTP/UDP, RTP over RTSP TCP, HTTP TS), ported from `flush_stream`,
//! `send_rtcp` and `stream_timeout`.

use crate::handler::StreamType;
use crate::rtcp::{RtcpInfo, build_rtcp};
use crate::rtp::{Batch, Framing, RtpState, packetize};
use crate::socket::DEFAULT_FIFO_SIZE;
use crate::writeq::WriteQueue;
use ms_tuner::PidSet;
use std::io::{self, Write};
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::time::SystemTime;

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
    /// Bytes dropped by the queue or a full UDP socket.
    pub overflow: u64,
    /// Bytes waiting in the write queue.
    pub buffered: u64,
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
    rtp: RtpState,
    queue: WriteQueue,
    udp_dropped: u64,
    wtime_ms: i64,
    rtcp_wtime_ms: i64,
    sp: u32,
    sb: u32,
    started: bool,
}

impl StreamOutput {
    /// Binds RTP/RTCP sockets and connects them to the client (UDP `decode_transport`).
    pub fn udp(target: UdpTarget, config: OutputConfig, now_ms: i64) -> io::Result<Self> {
        let rtp = bind_connect(target.local, target.dest, target.ttl)?;
        let rtcp_local = with_port(target.local, target.local.port().wrapping_add(1));
        let rtcp_dest = with_port(target.dest, target.dest.port().wrapping_add(1));
        let rtcp = bind_connect(rtcp_local, rtcp_dest, target.ttl)?;
        Ok(Self::udp_from_sockets(rtp, rtcp, config, now_ms))
    }

    /// Wraps already connected, non-blocking RTP and RTCP sockets.
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
            rtp: RtpState::new(config.ssrc, config.seq),
            queue: WriteQueue::new(config.fifo_size),
            udp_dropped: 0,
            wtime_ms: now_ms,
            rtcp_wtime_ms: now_ms,
            sp: 0,
            sb: 0,
            started: false,
        }
    }

    /// Output type.
    pub fn stream_type(&self) -> StreamType {
        self.stream_type
    }

    /// Current counters.
    pub fn stats(&self) -> OutputStats {
        let remote = match &self.transport {
            Transport::Udp { rtp, .. } => rtp.peer_addr().ok(),
            Transport::Tcp { remote, .. } => *remote,
        };
        OutputStats {
            stream_type: self.stream_type,
            seq: self.rtp.seq,
            ssrc: self.rtp.ssrc,
            packets: self.sp,
            octets: self.sb,
            overflow: self.queue.overflow() + self.udp_dropped,
            buffered: self.queue.fifo().used(),
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
        }
    }

    /// Sends the stream's packets from one adapter buffer (`process_packets_for_stream`).
    pub fn process(
        &mut self,
        buf: &[u8],
        pids: &PidSet,
        ctime_ms: i64,
        mono_us: i64,
    ) -> io::Result<()> {
        let batches = packetize(
            &mut self.rtp,
            self.framing,
            buf,
            |pid| pids.contains(pid),
            mono_us,
        );
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
                for datagram in batch.datagrams(buf) {
                    match rtp.send(&datagram) {
                        Ok(count) => sent += count as i32,
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            self.udp_dropped += datagram.len() as u64;
                        }
                        Err(_) => {}
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
        };
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
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    self.udp_dropped += packet.len() as u64;
                }
                Err(_) => {}
            },
            Transport::Tcp { sink, .. } => {
                if self.queue.write(sink.as_mut(), &packet, false) < 0 {
                    return Err(io::Error::from(io::ErrorKind::BrokenPipe));
                }
            }
        }
        self.rtcp_wtime_ms = ctime_ms;
        Ok(())
    }

    /// Queues an RTSP reply on the high-priority lane (`sockets_writev_prio(.., 1)`).
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
        }
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

fn with_port(addr: SocketAddr, port: u16) -> SocketAddr {
    SocketAddr::new(addr.ip(), port)
}

fn bind_connect(local: SocketAddr, dest: SocketAddr, ttl: Option<u32>) -> io::Result<UdpSocket> {
    let socket = UdpSocket::bind(local)?;
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
