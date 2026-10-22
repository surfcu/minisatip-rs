//! Stream outputs over real localhost sockets and in-memory sinks.

use ms_net::StreamType;
use ms_net::output::{OutputConfig, StreamOutput, UdpTarget, bind_udp_pair, rtp_server_port};
use ms_tuner::PidSet;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

fn ts_packet(pid: u16, fill: u8) -> [u8; 188] {
    let mut packet = [fill; 188];
    packet[0] = 0x47;
    packet[1] = (pid >> 8) as u8 & 0x1F;
    packet[2] = pid as u8;
    packet[3] = 0x10;
    packet
}

fn buffer(pids: &[u16]) -> Vec<u8> {
    pids.iter()
        .enumerate()
        .flat_map(|(index, pid)| ts_packet(*pid, index as u8))
        .collect()
}

fn config(ssrc: u32) -> OutputConfig {
    OutputConfig {
        ssrc,
        seq: 0,
        fifo_size: 1 << 20,
    }
}

/// Binds two UDP sockets on consecutive ports (RTP, RTCP).
fn udp_pair() -> (UdpSocket, UdpSocket) {
    for _ in 0..200 {
        let rtp = UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = rtp.local_addr().unwrap().port();
        if !port.is_multiple_of(2) || port == u16::MAX {
            continue;
        }
        if let Ok(rtcp) = UdpSocket::bind(("127.0.0.1", port + 1)) {
            for socket in [&rtp, &rtcp] {
                socket
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
            }
            return (rtp, rtcp);
        }
    }
    panic!("no free UDP port pair");
}

/// A local even port whose successor is free too, released for the output to bind.
fn free_local_pair() -> SocketAddr {
    let (rtp, rtcp) = udp_pair();
    let addr = rtp.local_addr().unwrap();
    drop((rtp, rtcp));
    addr
}

fn udp_output(dest: SocketAddr, ssrc: u32) -> StreamOutput {
    let target = UdpTarget {
        local: free_local_pair(),
        dest,
        ttl: None,
    };
    StreamOutput::udp(target, config(ssrc), 0).unwrap()
}

#[test]
fn udp_outputs_set_dscp_ef_and_the_send_buffer() {
    // stream.cpp:570-584: with `opts.output_buffer > 0`,
    // set_socket_send_buffer runs on RTP, then set_socket_dscp marks RTP
    // with IPTOS_DSCP_EF/priority 7 and RTCP with priority 6; the TOS byte
    // is `dscp & IPTOS_DSCP_MASK_VALUE` (socketworks.cpp:1618-1632).
    use std::os::fd::AsRawFd;

    let (rx_rtp, rx_rtcp) = udp_pair();
    let target = UdpTarget {
        local: free_local_pair(),
        dest: rx_rtp.local_addr().unwrap(),
        ttl: None,
    };
    let output = StreamOutput::udp_with_options(target, config(7), 0, None, 65_536).unwrap();
    let (rtp, rtcp) = output.udp_sockets().expect("UDP output");
    for socket in [rtp, rtcp] {
        assert_eq!(
            ms_net::sys::socket_tos(socket.as_raw_fd()).unwrap(),
            i32::from(0xb8u8)
        );
        // The kernel doubles SO_SNDBUF, so the read-back is at least 64 KiB.
        assert!(ms_net::sys::socket_send_buffer(socket.as_raw_fd()).unwrap() >= 65_536);
    }
    drop((rx_rtp, rx_rtcp));
}

#[test]
fn udp_rtp_socket_blocks_with_the_cpp_timeout() {
    // udp_bind never sets O_NONBLOCK and set_linux_socket_timeout gives
    // SO_SNDTIMEO/SO_RCVTIMEO of 1.1 s (socketworks.cpp:250-386), so a send
    // burst waits for buffer space. RTCP is polled, so it stays non-blocking.
    let (rx_rtp, rx_rtcp) = udp_pair();
    let target = UdpTarget {
        local: free_local_pair(),
        dest: rx_rtp.local_addr().unwrap(),
        ttl: None,
    };
    let output = StreamOutput::udp_with_options(target, config(7), 0, None, 0).unwrap();
    let (rtp, rtcp) = output.udp_sockets().expect("UDP output");
    use std::os::fd::AsRawFd;
    assert!(
        !ms_net::sys::socket_nonblocking(rtp.as_raw_fd()).unwrap(),
        "RTP sends must wait for buffer space"
    );
    assert!(ms_net::sys::socket_nonblocking(rtcp.as_raw_fd()).unwrap());
    let timeout = Some(std::time::Duration::from_millis(1100));
    assert_eq!(rtp.write_timeout().unwrap(), timeout);
    assert_eq!(rtp.read_timeout().unwrap(), timeout);
    let mut buf = [0u8; 16];
    assert_eq!(
        rtcp.recv(&mut buf).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock,
        "RTCP reads must not block"
    );
    drop((rx_rtp, rx_rtcp));
}

fn recv(socket: &UdpSocket) -> Vec<u8> {
    let mut buf = [0u8; 2048];
    let len = socket.recv(&mut buf).expect("datagram");
    buf[..len].to_vec()
}

#[test]
fn rtp_server_port_numbering() {
    assert_eq!(rtp_server_port(5500, 0), 5500);
    assert_eq!(rtp_server_port(5500, 3), 5506);
}

#[test]
fn udp_send_refused_forces_close() {
    // socketworks.cpp:1334-1343: ECONNREFUSED on an RTP socket closes the stream.
    let (rx_rtp, rx_rtcp) = udp_pair();
    let dest = rx_rtp.local_addr().unwrap();
    drop((rx_rtp, rx_rtcp));
    let mut output = udp_output(dest, 0x1234);
    let buf = buffer(&[0x100]);
    let pids = PidSet::from_pids([0x100]);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !output.forced_close() && Instant::now() < deadline {
        output.process(&buf, &pids, 1000, 5_000_000).unwrap();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        output.forced_close(),
        "refused RTP destination is not closed"
    );
    assert!(output.stats().failed_writes > 0);
}

#[test]
fn udp_send_to_localhost() {
    let (rx_rtp, _rx_rtcp) = udp_pair();
    let mut output = udp_output(rx_rtp.local_addr().unwrap(), 0xCAFE_F00D);
    let buf = buffer(&[0x100; 9]);
    let pids = PidSet::from_pids([0x100]);
    output.process(&buf, &pids, 1000, 5_000_000).unwrap();

    let first = recv(&rx_rtp);
    let second = recv(&rx_rtp);
    assert_eq!(first.len(), 12 + 7 * 188);
    assert_eq!(second.len(), 12 + 2 * 188);
    assert_eq!(&first[0..2], &[0x80, 0x21]);
    assert_eq!(&first[2..4], &[0, 0]);
    assert_eq!(&second[2..4], &[0, 1]);
    assert_eq!(&first[8..12], &0xCAFE_F00Du32.to_be_bytes());
    assert_eq!(u32::from_be_bytes(first[4..8].try_into().unwrap()), 450_000);
    assert_eq!(&first[12..], &buf[..7 * 188]);
    assert_eq!(&second[12..], &buf[7 * 188..]);

    let stats = output.stats();
    assert_eq!(stats.stream_type, StreamType::RtspUdp);
    assert_eq!(stats.packets, 1, "sp counts flushes, not RTP packets");
    assert_eq!(stats.octets as usize, first.len() + second.len());
    assert_eq!(stats.seq, 2);
    assert!(stats.started);
}

#[test]
fn udp_without_matching_pids_sends_bare_header() {
    let (rx_rtp, _rx_rtcp) = udp_pair();
    let mut output = udp_output(rx_rtp.local_addr().unwrap(), 1);
    let buf = buffer(&[0x200; 3]);
    output
        .process(&buf, &PidSet::from_pids([0x100]), 10, 0)
        .unwrap();
    assert_eq!(recv(&rx_rtp).len(), 12);
}

#[test]
fn udp_keepalive_and_rtcp_timer() {
    let (rx_rtp, rx_rtcp) = udp_pair();
    let mut output = udp_output(rx_rtp.local_addr().unwrap(), 0x0102_0304);
    let desc = "ver=1.0;src=1;tuner=1,0,0,0,0,,,,,,,;pids=none";

    // Under 200 ms since creation: neither RTCP nor keepalive.
    output
        .on_timer(150, 150_000, SystemTime::now(), || desc.to_owned())
        .unwrap();
    rx_rtcp.set_nonblocking(true).unwrap();
    assert!(rx_rtcp.recv(&mut [0u8; 64]).is_err());
    rx_rtcp.set_nonblocking(false).unwrap();

    output
        .on_timer(200, 200_000, SystemTime::now(), || desc.to_owned())
        .unwrap();
    let report = recv(&rx_rtcp);
    assert_eq!(report.len(), 116);
    assert_eq!(&report[0..2], &[0x80, 0xC8]);
    assert_eq!(&report[60..64], b"SES1");
    assert_eq!(&report[68..68 + desc.len()], desc.as_bytes());
    assert_eq!(&report[16..20], &[0, 0, 0, 0], "wtime from creation");

    // More than 1 s without data: a bare RTP header keeps the stream alive.
    output
        .on_timer(1001, 1_001_000, SystemTime::now(), || desc.to_owned())
        .unwrap();
    let keepalive = recv(&rx_rtp);
    assert_eq!(keepalive.len(), 12);
    assert_eq!(&keepalive[2..4], &[0, 0]);
    let report = recv(&rx_rtcp);
    assert_eq!(
        &report[16..20],
        &1001u32.to_be_bytes(),
        "keepalive updates wtime"
    );
    assert_eq!(&report[20..24], &[0, 0, 0, 1]);
}

// Ported from tests/test_stream.cpp:397
#[test]
fn udp_client_port_change_updates_rport() {
    let (rx_a, _rtcp_a) = udp_pair();
    let (rx_b, _rtcp_b) = udp_pair();
    let mut output = udp_output(rx_a.local_addr().unwrap(), 1);
    assert_eq!(output.stats().remote, Some(rx_a.local_addr().unwrap()));
    // decode_transport binds the new sockets on the same server port while
    // the old ones are still open (stream.cpp:556-584).
    let local = output.udp_sockets().unwrap().0.local_addr().unwrap();
    let target = UdpTarget {
        local,
        dest: rx_b.local_addr().unwrap(),
        ttl: None,
    };
    let (rtp, rtcp) = bind_udp_pair(&target, None).unwrap();
    output.replace_udp_sockets(rtp, rtcp, 0).unwrap();
    assert_eq!(
        output.stats().remote.map(|addr| addr.port()),
        Some(rx_b.local_addr().unwrap().port()),
        "client port not updated"
    );
    let buf = buffer(&[0x100]);
    output
        .process(&buf, &PidSet::from_pids([0x100]), 0, 0)
        .unwrap();
    assert_eq!(recv(&rx_b).len(), 12 + 188);
    assert_eq!(output.stats().seq, 1, "RTP state survives the port change");
}

#[test]
fn ssrc_follows_set_session_id() {
    // stream.cpp:1409-1416: `set_session_id` assigns `sid->ssrc`; every
    // subsequent RTP/RTCP packet carries it.
    let (rx_rtp, rx_rtcp) = udp_pair();
    let mut output = udp_output(rx_rtp.local_addr().unwrap(), 1);
    output.set_ssrc(0xDEAD_BEEF);
    assert_eq!(output.stats().ssrc, 0xDEAD_BEEF);
    output.send_rtcp(0, SystemTime::now(), "x").unwrap();
    let report = recv(&rx_rtcp);
    assert_eq!(&report[4..8], &0xDEAD_BEEFu32.to_be_bytes());
}

#[test]
fn rtcp_clocks_start_at_setup() {
    // streams_add sets `sid->wtime`/`sid->rtcp_wtime` (stream.cpp:634); the
    // data plane rewinds the clocks to that instant when PLAY starts.
    let (rx_rtp, rx_rtcp) = udp_pair();
    let target = UdpTarget {
        local: free_local_pair(),
        dest: rx_rtp.local_addr().unwrap(),
        ttl: None,
    };
    // The output object was built at PLAY (tick 10000) but SETUP was at 5000.
    let mut output = StreamOutput::udp(target, config(1), 10_000).unwrap();
    output.set_clocks(5_000);
    output.send_rtcp(6_000, SystemTime::now(), "x").unwrap();
    let report = recv(&rx_rtcp);
    // The sender-report word carries `wtime - rtcp_wtime` semantics from the
    // SETUP instant, not from PLAY (compare udp_keepalive_and_rtcp_timer).
    assert_eq!(&report[16..20], &5_000u32.to_be_bytes());
}

#[test]
fn udp_rtcp_receiver_report_is_seen() {
    let (rx_rtp, rx_rtcp) = udp_pair();
    let mut output = udp_output(rx_rtp.local_addr().unwrap(), 1);
    assert!(!output.poll_rtcp());
    output.send_rtcp(0, SystemTime::now(), "x").unwrap();
    let mut report = [0u8; 2048];
    let (_, server) = rx_rtcp.recv_from(&mut report).unwrap();
    rx_rtcp
        .send_to(&[0x80, 0xC9, 0, 1, 0, 0, 0, 1], server)
        .unwrap();
    let mut seen = false;
    for _ in 0..200 {
        if output.poll_rtcp() {
            seen = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(seen, "receiver report not seen");
}

#[derive(Clone, Default)]
struct SharedSink {
    data: Arc<Mutex<Vec<u8>>>,
    blocked: Arc<Mutex<bool>>,
}

impl SharedSink {
    fn bytes(&self) -> Vec<u8> {
        self.data.lock().unwrap().clone()
    }

    fn set_blocked(&self, blocked: bool) {
        *self.blocked.lock().unwrap() = blocked;
    }
}

impl Write for SharedSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if *self.blocked.lock().unwrap() {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        self.data.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn write_vectored(&mut self, bufs: &[io::IoSlice<'_>]) -> io::Result<usize> {
        if *self.blocked.lock().unwrap() {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let mut data = self.data.lock().unwrap();
        let before = data.len();
        for buf in bufs {
            data.extend_from_slice(buf);
        }
        Ok(data.len() - before)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn rtsp_tcp_interleaved_framing() {
    let sink = SharedSink::default();
    let mut output = StreamOutput::rtsp_tcp(Box::new(sink.clone()), None, config(9), 0);
    let buf = buffer(&[0x100; 43]);
    output
        .process(&buf, &PidSet::from_pids([0x100]), 5, 0)
        .unwrap();
    let bytes = sink.bytes();
    assert_eq!(bytes.len(), 16 + 42 * 188 + 16 + 188);
    assert_eq!(&bytes[0..4], &[b'$', 0, 0x1E, 0xE4]);
    assert_eq!(&bytes[4..6], &[0x80, 0x21]);
    assert_eq!(&bytes[16..16 + 42 * 188], &buf[..42 * 188]);
    let second = 16 + 42 * 188;
    assert_eq!(&bytes[second..second + 4], &[b'$', 0, 0, 200]);
    assert_eq!(&bytes[second + 16..], &buf[42 * 188..]);

    output
        .on_timer(200, 200_000, SystemTime::now(), || "abc".to_owned())
        .unwrap();
    let bytes = sink.bytes();
    let rtcp = &bytes[second + 16 + 188..];
    assert_eq!(&rtcp[0..4], &[b'$', 1, 0, 72]);
    assert_eq!(&rtcp[4..6], &[0x80, 0xC8]);
    assert_eq!(rtcp.len(), 4 + 72);
}

#[test]
fn rtsp_tcp_buffers_and_prioritises_replies() {
    let sink = SharedSink::default();
    sink.set_blocked(true);
    let mut output = StreamOutput::rtsp_tcp(Box::new(sink.clone()), None, config(9), 0);
    let buf = buffer(&[0x100; 2]);
    let pids = PidSet::from_pids([0x100]);
    output.process(&buf, &pids, 5, 0).unwrap();
    output.process(&buf, &pids, 6, 0).unwrap();
    let stats = output.stats();
    assert_eq!(
        stats.packets, 0,
        "fully queued writes return 0 like sockets_writev"
    );
    assert_eq!(stats.buffered as usize, 2 * (16 + 2 * 188));
    // socketworks.cpp:1505-1506: `buffered_bytes` accumulates every byte
    // copied into the FIFO, not the current queue occupancy.
    assert_eq!(stats.fifo_written as usize, 2 * (16 + 2 * 188));
    assert_eq!(stats.fifo_dropped, 0);
    assert!(output.has_pending());

    let reply = b"RTSP/1.0 200 OK\r\nCSeq: 3\r\n\r\n";
    assert_eq!(output.write_priority(reply).unwrap(), 0);
    sink.set_blocked(false);
    // The queue starts on an interleaved frame boundary, so the reply jumps ahead.
    output.flush_queue().unwrap();
    assert_eq!(sink.bytes(), reply);
    while output.has_pending() {
        output.flush_queue().unwrap();
    }
    let bytes = sink.bytes();
    assert_eq!(bytes.len(), reply.len() + 2 * (16 + 2 * 188));
    assert_eq!(&bytes[reply.len()..reply.len() + 2], &[b'$', 0]);
    assert_eq!(&bytes[reply.len() + 16 + 2 * 188..][..2], &[b'$', 0]);
    let stats = output.stats();
    assert_eq!(stats.buffered, 0, "queue drained");
    assert_eq!(
        stats.fifo_written as usize,
        2 * (16 + 2 * 188),
        "the lifetime copy counter is not a queue gauge"
    );
}

#[test]
fn rtsp_tcp_reply_waits_for_partial_frame() {
    struct Limited {
        data: Arc<Mutex<Vec<u8>>>,
        budget: Arc<Mutex<usize>>,
    }
    impl Write for Limited {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let mut budget = self.budget.lock().unwrap();
            if *budget == 0 {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            let count = buf.len().min(*budget);
            *budget -= count;
            self.data.lock().unwrap().extend_from_slice(&buf[..count]);
            Ok(count)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let data = Arc::new(Mutex::new(Vec::new()));
    let budget = Arc::new(Mutex::new(100));
    let sink = Limited {
        data: data.clone(),
        budget: budget.clone(),
    };
    let mut output = StreamOutput::rtsp_tcp(Box::new(sink), None, config(9), 0);
    let buf = buffer(&[0x100; 2]);
    let pids = PidSet::from_pids([0x100]);
    output.process(&buf, &pids, 5, 0).unwrap();
    output.process(&buf, &pids, 6, 0).unwrap();
    assert_eq!(output.stats().packets, 1, "the partial direct write counts");
    let reply = b"RTSP/1.0 200 OK\r\nCSeq: 4\r\n\r\n";
    output.write_priority(reply).unwrap();
    *budget.lock().unwrap() = usize::MAX;
    while output.has_pending() {
        output.flush_queue().unwrap();
    }
    let frame = 16 + 2 * 188;
    let bytes = data.lock().unwrap().clone();
    assert_eq!(bytes.len(), 2 * frame + reply.len());
    assert_eq!(
        &bytes[frame..frame + reply.len()],
        reply,
        "reply after the first frame"
    );
    assert_eq!(bytes[frame + reply.len()], b'$');
}

#[test]
fn http_output_streams_raw_ts() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let mut client = TcpStream::connect(addr).unwrap();
    let (server, peer) = listener.accept().unwrap();
    server.set_nonblocking(true).unwrap();
    let mut output = StreamOutput::http(Box::new(server), Some(peer), config(1), 0);
    assert_eq!(output.stream_type(), StreamType::Http);

    let buf = buffer(&[0x100, 0x200, 0x101, 0x100]);
    let pids = PidSet::from_pids([0x100, 0x101]);
    output.process(&buf, &pids, 0, 0).unwrap();
    output
        .on_timer(5000, 5_000_000, SystemTime::now(), || "x".to_owned())
        .unwrap();
    drop(output);

    let mut received = Vec::new();
    client.read_to_end(&mut received).unwrap();
    let expected = [&buf[..188], &buf[376..]].concat();
    assert_eq!(received, expected, "no RTP headers, no keepalive, no RTCP");
}

#[test]
fn http_output_reports_broken_connection() {
    struct Broken;
    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut output = StreamOutput::http(Box::new(Broken), None, config(1), 0);
    let buf = buffer(&[0x100]);
    assert!(
        output
            .process(&buf, &PidSet::from_pids([0x100]), 0, 0)
            .is_err()
    );
}
