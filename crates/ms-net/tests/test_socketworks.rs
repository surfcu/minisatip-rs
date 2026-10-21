//! Port of upstream `tests/test_socketworks.cpp`.
//!
//! Kernel sockets are replaced with test doubles that preserve the asserted
//! behaviour; no root privileges or real network devices are needed.

use ms_net::bind::{
    BindDevError, BindDevOps, BindDevQuery, BindDevStatus, BindOptions, DevLookup, INET_ADDRSTRLEN,
    get_dev_ip, resolve_bind_opts, set_socket_bind_dev, socket_bind_dev_ok, validate_bind_dev,
};
use ms_net::fifo::copy_iovec_to_fifo;
use ms_net::socket::{ReadOutcome, SocketTable, SocketType};
use ms_net::transport::{
    IpFamily, SocketOps, get_sock_shost, get_sock_sport, tcp_connect_src, tcp_listen, udp_bind,
    udp_bind_connect,
};
use ms_net::writeq::WriteQueue;
use std::io::{self, IoSlice, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

const START_RTSP_FRAME: [u8; 6] = [0x24, 0x00, 1, 2, 0x80, 0x21];

#[derive(Default)]
struct Capture {
    data: Vec<u8>,
}

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.data.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        let mut total = 0;
        for buf in bufs {
            self.data.extend_from_slice(buf);
            total += buf.len();
        }
        Ok(total)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Stand-in for the C++ `writev_test`: each call writes at most `limit` bytes.
struct PartialSink {
    data: Vec<u8>,
    limit: usize,
}

impl PartialSink {
    fn new() -> Self {
        Self {
            data: Vec::new(),
            limit: usize::MAX,
        }
    }

    fn set_limit(&mut self, limit: usize) {
        self.limit = limit;
    }
}

impl Write for PartialSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = buf.len().min(self.limit);
        self.data.extend_from_slice(&buf[..written]);
        Ok(written)
    }

    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        let mut left = self.limit;
        let mut total = 0;
        for buf in bufs {
            if left == 0 {
                break;
            }
            let written = buf.len().min(left);
            self.data.extend_from_slice(&buf[..written]);
            left -= written;
            total += written;
        }
        Ok(total)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn test_socket_enque_highprio() {
    let mut queue = WriteQueue::new(100);
    assert_eq!(queue.enqueue_high_prio(&[b"test1", b"test2"]), 0);
    assert_eq!(queue.enqueue_high_prio(&[b"test3"]), 0);
    assert_eq!(
        queue.prio_data(),
        b"test1test2test3",
        "expected != the actual result"
    );
}

#[test]
fn test_socket_enque_highprio_large() {
    let mut queue = WriteQueue::new(100);
    let body = vec![b'x'; 30000];
    assert_eq!(
        queue.enqueue_high_prio(&[b"head", &body]),
        0,
        "large reply queued"
    );
    assert_eq!(queue.prio_len(), 30004, "whole reply queued");
    assert!(
        queue.prio_data()[29999] == b'x' && queue.prio_data()[30003] == b'x',
        "reply copied completely"
    );
    let huge = vec![b'y'; 1024 * 1024];
    assert_eq!(queue.enqueue_high_prio(&[&huge]), -1, "queue is capped");
    assert_eq!(queue.prio_len(), 30004, "capped reply is dropped");
}

#[test]
fn test_socket_writev_flush_enqued() {
    let mut queue = WriteQueue::new(100);
    queue.fifo_mut().set_indices(0, 1);
    queue.fifo_mut().data_mut()[0] = b'0';

    let mut sink = Capture::default();
    queue.write(&mut sink, b"1", false);
    queue.write(&mut sink, &START_RTSP_FRAME, false);
    queue.write(&mut sink, &[0u8; 50], false);
    queue.set_flush_enqueued_data(true);
    queue.write(&mut sink, b"2", false);
    queue.flush(&mut sink);

    assert_eq!(sink.data.len(), 3, "read unexpected number of bytes");
    assert_eq!(sink.data, b"012", "unexpected result");
}

#[test]
fn test_socket_buffering() {
    let mut buf = [0u8; 255];
    for (index, byte) in buf.iter_mut().enumerate() {
        // Account for the 9 bytes (priority data + RTSP frame) inserted at offset 50.
        *byte = if index < 50 {
            index as u8
        } else {
            (index + 9) as u8
        };
    }

    let mut queue = WriteQueue::new(100);
    queue.fifo_mut().set_indices(50, 50);
    let mut sink = PartialSink::new();
    sink.set_limit(1);

    for index in 0..10 {
        if index == 5 {
            queue.write(&mut sink, &START_RTSP_FRAME, false);
        }
        queue.write(&mut sink, &buf[index * 10..index * 10 + 10], false);
    }

    assert_eq!(queue.overflow(), 10, "Expected overflow");

    assert_eq!(queue.write_vectored(&mut sink, &[b"XYZ"], true), 0);
    for limit in [47, 5, 5, 50] {
        sink.set_limit(limit);
        queue.flush(&mut sink);
    }

    for index in 0..90 {
        if !(50..=58).contains(&index) {
            assert_eq!(
                sink.data[index], index as u8,
                "Invalid value found in the buffer at position {index}: {:02X}",
                sink.data[index]
            );
        }
    }
    // The C++ assertion is `ASSERT(memcmp(...))`, i.e. it only checks inequality.
    assert_ne!(
        &sink.data[50..56],
        &START_RTSP_FRAME,
        "RTSP frame not found at expected position"
    );
    assert!(
        sink.data[50..59]
            .windows(START_RTSP_FRAME.len())
            .any(|window| window == START_RTSP_FRAME),
        "RTSP frame not found in the skipped window"
    );

    let chunks: [&[u8]; 3] = [&buf[0..10], &buf[10..20], &buf[20..30]];
    queue.fifo_mut().set_indices(0, 0);
    assert_eq!(copy_iovec_to_fifo(queue.fifo_mut(), 0, &chunks), 30);
    assert_eq!(
        &queue.fifo().data()[..queue.fifo().used() as usize],
        &buf[..30],
        "copy_iovec_to_fifo failed with 0 offset"
    );

    queue.fifo_mut().set_indices(0, 0);
    assert_eq!(copy_iovec_to_fifo(queue.fifo_mut(), 19, &chunks), 11);
    assert_eq!(
        &queue.fifo().data()[..queue.fifo().used() as usize],
        &buf[19..30],
        "copy_iovec_to_fifo failed with custom offset"
    );
}

struct MapLookup;

impl DevLookup for MapLookup {
    fn dev_ip(&self, dev: &str) -> Option<String> {
        match dev {
            "lo" | "lo0" => Some("127.0.0.1".to_owned()),
            _ => None,
        }
    }
}

#[derive(Default)]
struct FakeSocket {
    local_host: String,
    local_port: u16,
}

#[derive(Default)]
struct FakeSocketOps {
    sockets: Vec<FakeSocket>,
}

impl SocketOps for FakeSocketOps {
    fn socket(&mut self, family: IpFamily) -> Option<u32> {
        let id = self.sockets.len() as u32;
        let (host, port) = match family {
            IpFamily::V4 => ("192.0.2.1", 40000),
            IpFamily::V6 => ("2001:db8::1", 40001),
        };
        self.sockets.push(FakeSocket {
            local_host: host.to_owned(),
            local_port: port + id as u16,
        });
        Some(id)
    }

    fn set_reuseaddr(&mut self, _sock: u32) -> bool {
        true
    }

    fn bind(&mut self, sock: u32, addr: &str, port: u16) -> bool {
        match self.sockets.get_mut(sock as usize) {
            Some(socket) => {
                socket.local_host = addr.to_owned();
                if port != 0 {
                    socket.local_port = port;
                }
                true
            }
            None => false,
        }
    }

    fn listen(&mut self, _sock: u32) -> bool {
        true
    }

    fn connect(&mut self, _sock: u32, _addr: &str, _port: u16) -> bool {
        true
    }

    fn local_host(&self, sock: u32) -> Option<String> {
        self.sockets
            .get(sock as usize)
            .map(|socket| socket.local_host.clone())
    }

    fn local_port(&self, sock: u32) -> Option<u16> {
        self.sockets
            .get(sock as usize)
            .map(|socket| socket.local_port)
    }
}

struct MockBindDev {
    device: Option<String>,
    status: BindDevStatus,
}

impl BindDevOps for MockBindDev {
    fn set_bind_dev(&mut self, _dev: &str) -> BindDevStatus {
        self.status
    }
}

impl BindDevQuery for MockBindDev {
    fn bound_device(&self) -> Option<String> {
        self.device.clone()
    }
}

#[test]
fn test_bind_dev() {
    let lookup = MapLookup;
    assert!(
        get_dev_ip(&lookup, None, INET_ADDRSTRLEN).is_err(),
        "NULL dev must fail"
    );
    assert!(
        get_dev_ip(&lookup, Some(""), INET_ADDRSTRLEN).is_err(),
        "empty dev must fail"
    );
    // A zero length stands in for the C++ NULL buffer.
    assert!(
        get_dev_ip(&lookup, Some("lo"), 0).is_err(),
        "NULL buf must fail"
    );
    assert!(
        get_dev_ip(&lookup, Some("lo"), 4).is_err(),
        "short buf must fail"
    );
    assert!(validate_bind_dev(Some("eth0")), "eth0 must validate");
    assert!(!validate_bind_dev(None), "NULL must not validate");
    assert!(!validate_bind_dev(Some("")), "empty must not validate");
    assert!(
        validate_bind_dev(Some("123456789012345")),
        "15 chars must pass"
    );
    assert!(
        !validate_bind_dev(Some("1234567890123456")),
        "16 chars must fail"
    );
    assert!(!validate_bind_dev(Some("a/b")), "slash must not validate");
    assert!(
        !validate_bind_dev(Some("a\nb")),
        "newline must not validate"
    );

    let dev = if get_dev_ip(&lookup, Some("lo"), INET_ADDRSTRLEN).is_ok() {
        "lo"
    } else if get_dev_ip(&lookup, Some("lo0"), INET_ADDRSTRLEN).is_ok() {
        "lo0"
    } else {
        eprintln!("no loopback device, skipping bind-dev test");
        return;
    };
    let ip = get_dev_ip(&lookup, Some(dev), INET_ADDRSTRLEN).unwrap();

    // getlocalip() is not part of this crate, so its baseline assertion is not ported.

    let mut opts = BindOptions::new(Some("127.0.0.2"), None, Some(dev));
    assert_eq!(
        resolve_bind_opts(&mut opts, &lookup),
        Err(BindDevError::Conflict),
        "bind + bind-dev must conflict"
    );
    let mut opts = BindOptions::new(None, Some("127.0.0.2"), Some(dev));
    assert_eq!(
        resolve_bind_opts(&mut opts, &lookup),
        Err(BindDevError::Conflict),
        "bind-http + bind-dev must conflict"
    );
    let mut opts = BindOptions::new(None, None, Some("no-such-dev-xyz"));
    assert!(
        resolve_bind_opts(&mut opts, &lookup).is_err(),
        "unknown dev must fail"
    );

    let mut opts = BindOptions::new(None, None, Some(dev));
    assert!(
        resolve_bind_opts(&mut opts, &lookup).is_ok(),
        "loopback dev must resolve"
    );
    assert_eq!(
        opts.bind.as_deref(),
        Some(ip.as_str()),
        "bind must hold dev IP"
    );
    assert_eq!(
        opts.bind_http.as_deref(),
        Some(ip.as_str()),
        "bind-http must hold dev IP"
    );
    assert_eq!(
        opts.bind_dev_ip(),
        Some(ip.as_str()),
        "source IP must be dev IP"
    );
    assert!(
        resolve_bind_opts(&mut opts, &lookup).is_ok(),
        "second resolve must be idempotent"
    );
    assert_eq!(
        opts.bind.as_deref(),
        Some(ip.as_str()),
        "bind must keep dev IP"
    );

    opts.bind = Some(String::new());
    opts.bind_http = Some(String::new());
    opts.bind_dev = Some(String::new());
    assert!(
        resolve_bind_opts(&mut opts, &lookup).is_ok(),
        "empty strings must resolve"
    );
    assert!(
        opts.bind.is_none()
            && opts.bind_http.is_none()
            && opts.bind_dev.is_none()
            && opts.bind_dev_ip().is_none(),
        "empty strings must normalize to NULL"
    );
    opts.bind_dev = Some(dev.to_owned());
    assert!(
        resolve_bind_opts(&mut opts, &lookup).is_ok(),
        "loopback dev must resolve again"
    );

    let mut ops = FakeSocketOps::default();
    let fd = tcp_listen(&mut ops, opts.bind.as_deref(), 0, true).unwrap();
    let tcp_host = get_sock_shost(&ops, fd).unwrap();
    let afd = tcp_listen(&mut ops, None, 0, true).unwrap();
    let any_host = get_sock_shost(&ops, afd).unwrap();
    let ufd = udp_bind(&mut ops, opts.bind.as_deref(), 0, true).unwrap();
    let udp_host = get_sock_shost(&ops, ufd).unwrap();
    let port = get_sock_sport(&ops, fd).unwrap();
    let cfd = tcp_connect_src(&mut ops, &ip, port, opts.bind_dev_ip(), true).unwrap();
    let conn_host = get_sock_shost(&ops, cfd).unwrap();

    assert_eq!(tcp_host, ip, "tcp_listen must bind dev IP");
    assert_eq!(
        any_host, "0.0.0.0",
        "NULL must bind ANY without substitution"
    );
    assert_eq!(udp_host, ip, "udp_bind must bind dev IP");
    assert_eq!(conn_host, ip, "connect must source dev IP");

    let mut mock = MockBindDev {
        device: Some(dev.to_owned()),
        status: BindDevStatus::Applied,
    };
    let soft = set_socket_bind_dev(&mut mock, Some(dev));
    assert!(
        soft == BindDevStatus::Applied || soft == BindDevStatus::Degraded,
        "valid dev must apply or degrade"
    );
    if soft == BindDevStatus::Applied {
        assert!(
            socket_bind_dev_ok(&mock, Some(dev)),
            "device restriction not applied"
        );
    }

    let mfd = udp_bind(&mut ops, Some("239.255.255.250"), 0, true);
    assert!(mfd.is_ok(), "multicast bind with dev IP must succeed");

    let usock = udp_bind_connect(&mut ops, Some(&ip), 0, "127.0.0.1", 9).unwrap();
    let usrc = get_sock_shost(&ops, usock).unwrap();
    assert_eq!(usrc, ip, "udp_bind_connect must bind src IP");

    let v6fd = tcp_listen(&mut ops, None, 0, false).unwrap();
    let v6port = get_sock_sport(&ops, v6fd).unwrap();
    let v6skip = tcp_connect_src(&mut ops, "::1", v6port, Some("127.0.0.1"), false);
    let v4fd = tcp_listen(&mut ops, Some("127.0.0.1"), 0, true).unwrap();
    let v4port = get_sock_sport(&ops, v4fd).unwrap();
    let v4skip = tcp_connect_src(&mut ops, "127.0.0.1", v4port, Some("::1"), true);
    let badsrc = tcp_connect_src(
        &mut ops,
        "127.0.0.1",
        v4port,
        Some("no-such-src-xyz.invalid"),
        true,
    );
    assert!(v6skip.is_ok(), "IPv4 source on IPv6 dest must be skipped");
    assert!(v4skip.is_ok(), "IPv6 source on IPv4 dest must be skipped");
    assert!(badsrc.is_err(), "garbage source must fail");

    let mut opts = BindOptions::new(Some("127.0.0.2"), None, None);
    assert!(
        resolve_bind_opts(&mut opts, &lookup).is_ok(),
        "bind alone must resolve"
    );
    assert_eq!(
        opts.bind_http.as_deref(),
        Some("127.0.0.2"),
        "bind-http must default to bind"
    );
    assert!(
        opts.bind_dev_ip().is_none(),
        "no source IP without bind-dev"
    );
    let efd = tcp_listen(&mut ops, opts.bind.as_deref(), 0, true).unwrap();
    assert_eq!(
        get_sock_shost(&ops, efd).as_deref(),
        Some("127.0.0.2"),
        "bind must be used as-is"
    );

    let mut opts = BindOptions::new(None, None, None);
    assert!(
        resolve_bind_opts(&mut opts, &lookup).is_ok(),
        "empty opts must resolve"
    );
    assert!(
        opts.bind.is_none() && opts.bind_http.is_none() && opts.bind_dev_ip().is_none(),
        "empty opts must stay NULL"
    );
}

struct MockQuery(Option<&'static str>);

impl BindDevQuery for MockQuery {
    fn bound_device(&self) -> Option<String> {
        self.0.map(str::to_owned)
    }
}

#[test]
fn test_socket_bind_dev_ok_compare() {
    // C++ devbuf is "eth0" followed by unrelated bytes past the NUL.
    let configured = "eth0\0XY";
    let mut mock = MockQuery(Some("eth0"));
    assert!(
        socket_bind_dev_ok(&mock, Some(configured)),
        "bound device must match exactly"
    );
    mock.0 = Some("wlan9");
    assert!(
        !socket_bind_dev_ok(&mock, Some(configured)),
        "other device must not match"
    );
    assert!(socket_bind_dev_ok(&mock, None), "unset bind-dev must pass");
}

#[test]
fn test_eof_does_not_dispatch_action() {
    // The C++ test uses a stale EWOULDBLOCK errno; io::Result models EOF as Ok(0).
    let calls = Arc::new(AtomicUsize::new(0));
    let mut table = SocketTable::new();
    let id = table.add(SocketType::Http, -1).expect("sockets_add failed");
    let counter = Arc::clone(&calls);
    let socket = table.get_mut(id).expect("socket must exist");
    socket.set_read(|_buf| Ok(0));
    socket.set_action(move |_| {
        counter.fetch_add(1, Ordering::SeqCst);
        0
    });

    assert_eq!(table.on_readable(id, 0), ReadOutcome::Closed);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "action must not run on EOF"
    );
    assert!(table.get(id).is_none(), "EOF socket must be closed");
}
