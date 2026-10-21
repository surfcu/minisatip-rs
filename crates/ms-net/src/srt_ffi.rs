//! [`SrtOps`] backed by libsrt through the shared `ms-srt` bindings, which hold
//! the only unsafe code of the SRT port.
//!
//! Enabled by the `srt` feature, which links libsrt through `ms-srt`.

use std::sync::{Arc, Mutex};

use crate::srt::{AcceptDecision, PendingPool, SRT_INVALID_SOCK, SrtOps};

const SRT_BACKLOG: i32 = 10;

/// SRT operations backed by libsrt.
#[derive(Debug, Default, Clone, Copy)]
pub struct LibSrtOps;

impl LibSrtOps {
    /// Initializes the library (`srt_startup`).
    pub fn startup() -> bool {
        ms_srt::startup()
    }

    /// Releases the library (`srt_cleanup`) after the listener is closed.
    pub fn cleanup() -> bool {
        ms_srt::cleanup()
    }

    /// Last libsrt error as text.
    pub fn last_error() -> String {
        ms_srt::last_error()
    }
}

impl SrtOps for LibSrtOps {
    fn create_socket(&self) -> i32 {
        ms_srt::create_socket()
    }

    fn bind_acquire(&self, sock: i32, udp_fd: i32) -> bool {
        ms_srt::bind_acquire(sock, udp_fd)
    }

    fn set_accept_callback(&self, sock: i32, pending: Arc<Mutex<PendingPool>>) -> bool {
        ms_srt::set_listen_callback(
            sock,
            Arc::new(move |streamid, ns| pre_accept(&pending, streamid, ns)),
        )
    }

    fn listen(&self, sock: i32, backlog: i32) -> bool {
        ms_srt::listen(sock, if backlog > 0 { backlog } else { SRT_BACKLOG })
    }

    fn accept(&self, sock: i32) -> i32 {
        ms_srt::accept(sock)
    }

    fn close(&self, sock: i32) {
        // Closing an already-closed handle is a documented no-op error that we
        // intentionally ignore.
        ms_srt::close(sock);
    }

    fn is_connected(&self, sock: i32) -> bool {
        ms_srt::is_connected(sock)
    }

    fn send(&self, sock: i32, data: &[u8]) -> i32 {
        ms_srt::send(sock, data)
    }
}

/// The pre-accept callback (`srt_accept_poll`, srt.cpp:100): queues `ns` under
/// its stream id and closes the socket it replaces.
fn pre_accept(pending: &Mutex<PendingPool>, streamid: Option<&str>, ns: i32) -> bool {
    let mut pool = pending.lock().expect("pending pool");
    match pool.accept(streamid, ns) {
        AcceptDecision::Accepted { replaced } => {
            if replaced != SRT_INVALID_SOCK {
                ms_srt::close(replaced);
            }
            true
        }
        AcceptDecision::Rejected => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::{OutputConfig, StreamOutput};
    use crate::rtp::DVB_FRAME;
    use crate::srt::{SRT_INVALID_SOCK, SrtListener};
    use ms_srt::SockOpt;
    use ms_tuner::PidSet;
    use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
    use std::os::fd::IntoRawFd;
    use std::thread;
    use std::time::Duration;

    /// libsrt startup/cleanup are process-global; tests using it run serially.
    static SRT_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn free_udp_fd() -> (i32, u16) {
        let socket = UdpSocket::bind(("127.0.0.1", 0)).expect("free udp port");
        let port = socket.local_addr().expect("local addr").port();
        (socket.into_raw_fd(), port)
    }

    fn client_connect(port: u16, streamid: &str) -> i32 {
        let sock = ms_srt::create_socket();
        assert_ne!(sock, SRT_INVALID_SOCK, "client socket create");
        assert!(ms_srt::set_streamid(sock, streamid), "set streamid");
        assert!(
            ms_srt::set_flag(sock, SockOpt::ConnTimeo, 1500),
            "set connect timeout"
        );
        assert!(
            ms_srt::connect(sock, SocketAddr::from((Ipv4Addr::LOCALHOST, port))),
            "client connect must succeed"
        );
        assert!(
            ms_srt::set_flag(sock, SockOpt::RcvTimeo, 2000),
            "set receive timeout"
        );
        sock
    }

    // Ported from tests/test_srt.cpp:35
    #[test]
    fn test_srt_listener_drain_and_close() {
        let _serial = SRT_TEST_LOCK.lock().expect("srt test lock");
        assert!(LibSrtOps::startup());
        let (udp_fd, port) = free_udp_fd();
        assert!(port > 0, "need a free UDP port");
        let mut listener = SrtListener::new(Arc::new(LibSrtOps));
        listener.init(udp_fd).expect("listener must init");
        assert!(listener.is_init(), "listener must report init");

        // Idle with no clients: a drain that exits on empty would be gone.
        thread::sleep(Duration::from_millis(200));

        // Backlog is 10 and the drain pops continuously: 12 sequential
        // connects must all succeed (a dead drain stalls past 10).
        for i in 0..12 {
            let sid = format!("test-{i}");
            let client = client_connect(port, &sid);
            let mut srv = SRT_INVALID_SOCK;
            for _ in 0..100 {
                srv = listener.take(&sid);
                if srv != SRT_INVALID_SOCK {
                    break;
                }
                thread::sleep(Duration::from_millis(20));
            }
            assert_ne!(srv, SRT_INVALID_SOCK, "server must queue the accept");
            ms_srt::close(srv);
            ms_srt::close(client);
        }

        listener.close();
        assert!(!listener.is_init(), "listener must report closed");
        // A second cycle proves close left no wedged drain behind.
        let (udp_fd, _port) = free_udp_fd();
        listener.init(udp_fd).expect("reinit must work after close");
        listener.close();
        assert!(LibSrtOps::cleanup());
    }

    /// End-to-end SRT output: one flush_stream batch crosses a real libsrt
    /// connection and is received by the client (stream.cpp:795-834).
    #[test]
    fn srt_output_sends_ts_over_a_real_connection() {
        let _serial = SRT_TEST_LOCK.lock().expect("srt test lock");
        assert!(LibSrtOps::startup());
        let (udp_fd, port) = free_udp_fd();
        let mut listener = SrtListener::new(Arc::new(LibSrtOps));
        listener.init(udp_fd).expect("listener must init");
        let client = client_connect(port, "output-test");
        let mut server = SRT_INVALID_SOCK;
        for _ in 0..100 {
            server = listener.take("output-test");
            if server != SRT_INVALID_SOCK {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        assert_ne!(
            server, SRT_INVALID_SOCK,
            "server must accept the connection"
        );
        assert!(
            listener.ops().is_connected(server),
            "server socket must reach SRTS_CONNECTED"
        );

        let mut buf = Vec::with_capacity(16 * DVB_FRAME);
        for index in 0..16u8 {
            let mut packet = [0xffu8; DVB_FRAME];
            packet[0] = 0x47;
            packet[1] = 0;
            packet[2] = 0;
            packet[3] = 0x10 | (index & 0x0f);
            buf.extend_from_slice(&packet);
        }
        let mut pids = PidSet::new();
        pids.insert(0);
        let ops: Arc<dyn SrtOps> = listener.ops().clone();
        let mut output = StreamOutput::srt(server, ops, None, OutputConfig::new(1), 0);
        output.process(&buf, &pids, 0, 0).expect("SRT send");
        assert!(!output.forced_close());
        assert_eq!(output.stats().octets, buf.len() as u32);

        let mut received = Vec::with_capacity(buf.len());
        let mut chunk = [0u8; 1500];
        while received.len() < buf.len() {
            let read = ms_srt::recv(client, &mut chunk);
            assert!(
                read > 0,
                "client receive failed: {}",
                LibSrtOps::last_error()
            );
            received.extend_from_slice(&chunk[..read as usize]);
        }
        assert_eq!(received, buf);

        let stats = output.stats();
        assert_eq!(stats.packets, 1);
        assert_eq!(stats.octets, buf.len() as u32);

        ms_srt::close(client);
        ms_srt::close(server);
        drop(output);
        listener.close();
        assert!(LibSrtOps::cleanup());
    }
}
