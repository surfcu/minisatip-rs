//! SRT listener tests, gated behind the `srt` feature like upstream's build.
#![cfg(feature = "srt")]

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use ms_net::StreamType;
use ms_net::handler::TransportError;
use ms_net::session::SessionManager;
use ms_net::srt::{
    AcceptDecision, LibSrtOps, MAX_UDP_PACKET_SIZE, PendingPool, SRT_INVALID_SOCK, SRT_PENDING_MAX,
    SrtError, SrtListener, SrtOps,
};

struct FakeInner {
    next_sock: i32,
    pending: Option<Arc<Mutex<PendingPool>>>,
    queue: VecDeque<i32>,
    listener: Option<i32>,
    listener_closed: bool,
    accepted: usize,
    closed: Vec<i32>,
    connected: HashMap<i32, bool>,
    sends: Vec<(i32, Vec<u8>)>,
    create_fails: bool,
    send_fails: bool,
}

impl Default for FakeInner {
    fn default() -> Self {
        Self {
            next_sock: 100,
            pending: None,
            queue: VecDeque::new(),
            listener: None,
            listener_closed: false,
            accepted: 0,
            closed: Vec::new(),
            connected: HashMap::new(),
            sends: Vec::new(),
            create_fails: false,
            send_fails: false,
        }
    }
}

#[derive(Default)]
struct FakeOps {
    inner: Mutex<FakeInner>,
    wake: Condvar,
}

impl FakeOps {
    fn alloc(inner: &mut FakeInner) -> i32 {
        let sock = inner.next_sock;
        inner.next_sock += 1;
        sock
    }

    /// Simulates a completed client handshake with `streamid`.
    fn connect(self: &Arc<Self>, streamid: &str) -> i32 {
        let sock = {
            let mut inner = self.inner.lock().expect("fake");
            let sock = Self::alloc(&mut inner);
            inner.connected.insert(sock, true);
            sock
        };
        let pending = self
            .inner
            .lock()
            .expect("fake")
            .pending
            .clone()
            .expect("callback installed by init");
        match pending
            .lock()
            .expect("pending")
            .accept(Some(streamid), sock)
        {
            AcceptDecision::Accepted { replaced } => {
                if replaced != SRT_INVALID_SOCK {
                    self.close(replaced);
                }
                self.inner.lock().expect("fake").queue.push_back(sock);
                self.wake.notify_all();
                sock
            }
            AcceptDecision::Rejected => {
                self.close(sock);
                SRT_INVALID_SOCK
            }
        }
    }

    fn accepted(&self) -> usize {
        self.inner.lock().expect("fake").accepted
    }

    fn closed_sockets(&self) -> Vec<i32> {
        self.inner.lock().expect("fake").closed.clone()
    }

    fn sends(&self) -> Vec<(i32, Vec<u8>)> {
        self.inner.lock().expect("fake").sends.clone()
    }

    fn set_connected(&self, sock: i32, connected: bool) {
        self.inner
            .lock()
            .expect("fake")
            .connected
            .insert(sock, connected);
    }

    fn fail_create(&self) {
        self.inner.lock().expect("fake").create_fails = true;
    }

    fn fail_sends(&self) {
        self.inner.lock().expect("fake").send_fails = true;
    }
}

impl SrtOps for FakeOps {
    fn create_socket(&self) -> i32 {
        let mut inner = self.inner.lock().expect("fake");
        if inner.create_fails {
            return SRT_INVALID_SOCK;
        }
        Self::alloc(&mut inner)
    }

    fn bind_acquire(&self, _sock: i32, udp_fd: i32) -> bool {
        udp_fd >= 0
    }

    fn set_accept_callback(&self, _sock: i32, pending: Arc<Mutex<PendingPool>>) -> bool {
        self.inner.lock().expect("fake").pending = Some(pending);
        true
    }

    fn listen(&self, sock: i32, _backlog: i32) -> bool {
        let mut inner = self.inner.lock().expect("fake");
        inner.listener = Some(sock);
        inner.listener_closed = false;
        true
    }

    fn accept(&self, sock: i32) -> i32 {
        let mut inner = self.inner.lock().expect("fake");
        loop {
            if inner.listener_closed {
                return SRT_INVALID_SOCK;
            }
            if let Some(accepted) = inner.queue.pop_front() {
                inner.accepted += 1;
                return accepted;
            }
            assert_eq!(some_listener(&inner), Some(sock));
            inner = self.wake.wait(inner).expect("fake");
        }
    }

    fn close(&self, sock: i32) {
        if sock == SRT_INVALID_SOCK {
            return;
        }
        let mut inner = self.inner.lock().expect("fake");
        inner.closed.push(sock);
        if inner.listener == Some(sock) {
            inner.listener_closed = true;
            self.wake.notify_all();
        }
    }

    fn is_connected(&self, sock: i32) -> bool {
        self.inner
            .lock()
            .expect("fake")
            .connected
            .get(&sock)
            .copied()
            .unwrap_or(false)
    }

    fn send(&self, sock: i32, data: &[u8]) -> i32 {
        let mut inner = self.inner.lock().expect("fake");
        inner.sends.push((sock, data.to_vec()));
        if inner.send_fails {
            return -1;
        }
        data.len() as i32
    }
}

fn some_listener(inner: &FakeInner) -> Option<i32> {
    inner.listener
}

fn wait_for<F: Fn() -> bool>(predicate: F) -> bool {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if predicate() {
            return true;
        }
        thread::sleep(Duration::from_millis(1));
    }
    predicate()
}

// Fake-transport variant of tests/test_srt.cpp:35; the libsrt variant lives in
// src/srt_ffi.rs.
#[test]
fn test_srt_listener_drain_and_close() {
    let ops = Arc::new(FakeOps::default());
    let mut listener = SrtListener::new(ops.clone());
    listener.init(10).expect("listener must init");
    assert!(listener.is_init(), "listener must report init");
    assert_eq!(listener.udp_fd(), 10);

    // Idle with no clients: a drain that exits on empty would be gone.
    thread::sleep(Duration::from_millis(200));

    // The drain pops continuously: 12 sequential connects must all be
    // accepted instead of stalling on the backlog.
    for i in 0..12 {
        let sid = format!("test-{i}");
        let client = ops.connect(&sid);
        assert_ne!(client, SRT_INVALID_SOCK, "client socket create");
        let mut srv = SRT_INVALID_SOCK;
        for _ in 0..100 {
            srv = listener.take(&sid);
            if srv != SRT_INVALID_SOCK {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        assert_ne!(srv, SRT_INVALID_SOCK, "server must queue the accept");
        assert!(
            wait_for(|| ops.accepted() > i),
            "drain thread must pop the accepted connection"
        );
        ops.close(srv);
        ops.close(client);
    }

    listener.close();
    assert!(!listener.is_init(), "listener must report closed");
    // A second cycle proves close left no wedged drain behind.
    listener.init(11).expect("reinit must work after close");
    listener.close();
}

#[test]
fn pending_pool_rejects_empty_and_overflow() {
    let mut pool = PendingPool::new();
    assert_eq!(pool.accept(None, 1), AcceptDecision::Rejected);
    assert_eq!(pool.accept(Some(""), 1), AcceptDecision::Rejected);
    assert!(pool.is_empty());

    for index in 0..SRT_PENDING_MAX {
        assert_eq!(
            pool.accept(Some(&format!("s{index}")), index as i32 + 10),
            AcceptDecision::Accepted {
                replaced: SRT_INVALID_SOCK
            }
        );
    }
    assert_eq!(pool.len(), SRT_PENDING_MAX);
    assert_eq!(pool.accept(Some("overflow"), 999), AcceptDecision::Rejected);
    assert_eq!(pool.len(), SRT_PENDING_MAX);
}

#[test]
fn pending_pool_replaces_and_takes() {
    let mut pool = PendingPool::new();
    pool.accept(Some("a"), 1);
    assert_eq!(
        pool.accept(Some("a"), 2),
        AcceptDecision::Accepted { replaced: 1 }
    );
    assert_eq!(pool.len(), 1);
    assert_eq!(pool.take("a"), 2);
    assert_eq!(pool.take("a"), SRT_INVALID_SOCK);
    assert_eq!(pool.put("b", 3), SRT_INVALID_SOCK);
    assert_eq!(pool.put("b", 4), 3);
    assert_eq!(pool.drain(), vec![4]);
    assert!(pool.is_empty());
}

#[test]
fn return_socket_closes_disconnected_and_queues_connected() {
    let ops = Arc::new(FakeOps::default());
    let listener = SrtListener::new(ops.clone());
    listener.return_socket(SRT_INVALID_SOCK, "a");
    assert!(ops.closed_sockets().is_empty());

    listener.return_socket(7, "a");
    assert_eq!(ops.closed_sockets(), vec![7]);
    assert!(listener.pending().lock().expect("pending").is_empty());

    ops.set_connected(8, true);
    listener.return_socket(8, "a");
    assert_eq!(listener.pending().lock().expect("pending").take("a"), 8);

    ops.set_connected(9, true);
    listener.return_socket(8, "a");
    listener.return_socket(9, "a");
    assert_eq!(listener.pending().lock().expect("pending").take("a"), 9);
    assert_eq!(ops.closed_sockets(), vec![7, 8]);
}

#[test]
fn send_ts_uses_cpp_chunk_size() {
    let ops = Arc::new(FakeOps::default());
    let listener = SrtListener::new(ops.clone());
    let data: Vec<u8> = (0..3000).map(|value| value as u8).collect();
    assert_eq!(listener.send_ts(5, &data), Ok(3000));
    let sends = ops.sends();
    assert_eq!(sends.len(), 3);
    assert_eq!(sends[0].1.len(), MAX_UDP_PACKET_SIZE);
    assert_eq!(sends[1].1.len(), MAX_UDP_PACKET_SIZE);
    assert_eq!(sends[2].1.len(), 3000 - 2 * MAX_UDP_PACKET_SIZE);
    assert!(sends.iter().all(|(sock, _)| *sock == 5));

    ops.fail_sends();
    assert_eq!(listener.send_ts(5, &[0; 10]), Err(SrtError::Send));
}

#[test]
fn init_reports_library_failures() {
    let ops = Arc::new(FakeOps::default());
    ops.fail_create();
    let mut listener = SrtListener::new(ops);
    assert_eq!(listener.init(10), Err(SrtError::CreateSocket));
    assert!(!listener.is_init());
}

#[test]
fn srt_setup_takes_the_pending_socket() {
    let mut manager = SessionManager::new();
    let listener = SrtListener::new(Arc::new(LibSrtOps));
    listener.pending().lock().expect("pending").put("sid-1", 7);
    manager.set_srt_listener(listener);
    let id = manager.add_stream().expect("stream slot");
    assert_eq!(
        manager.decode_transport(id, "RTP/AVP/SRT;srt_streamid=sid-1"),
        Ok(())
    );
    let stream = manager.stream(id).expect("stream");
    assert_eq!(stream.stream_type, StreamType::RtspSrt);
    assert_eq!(stream.srt_sock, 7);
    assert_eq!(stream.srt_streamid, "sid-1");
    assert_eq!(
        manager.srt_listener().expect("listener").take("sid-1"),
        SRT_INVALID_SOCK
    );
}

#[test]
fn srt_setup_requires_a_listener_and_pending_socket() {
    let mut manager = SessionManager::new();
    manager.set_srt_listener(SrtListener::new(Arc::new(LibSrtOps)));
    let id = manager.add_stream().expect("stream slot");
    assert_eq!(
        manager.decode_transport(id, "RTP/AVP/SRT;srt_streamid=missing"),
        Err(TransportError)
    );
    assert_eq!(
        manager.decode_transport(id, "RTP/AVP/SRT"),
        Err(TransportError)
    );

    let mut manager = SessionManager::new();
    let id = manager.add_stream().expect("stream slot");
    assert_eq!(
        manager.decode_transport(id, "RTP/AVP/SRT;srt_streamid=x"),
        Err(TransportError)
    );
}
