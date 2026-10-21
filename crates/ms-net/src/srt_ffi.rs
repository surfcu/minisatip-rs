#![allow(unsafe_code)]
//! Hand-written libsrt FFI. This is the only unsafe code in the SRT port.
//!
//! Declarations mirror `srt/srt.h`; the crate links `srt` only when the `srt`
//! feature is enabled (see build.rs).

use std::ffi::{CStr, c_char, c_int, c_void};
use std::ptr;
use std::sync::{Arc, Mutex};

use crate::srt::{AcceptDecision, PendingPool, SRT_INVALID_SOCK, SrtOps};

const SRT_ERROR: c_int = -1;
const SRTS_CONNECTED: c_int = 5;
const SRT_BACKLOG: c_int = 10;

type SrtSocket = c_int;

type ListenCallback = unsafe extern "C" fn(
    opaque: *mut c_void,
    ns: SrtSocket,
    hsversion: c_int,
    peeraddr: *const c_void,
    streamid: *const c_char,
) -> c_int;

unsafe extern "C" {
    fn srt_startup() -> c_int;
    fn srt_cleanup() -> c_int;
    fn srt_create_socket() -> SrtSocket;
    fn srt_bind_acquire(sock: SrtSocket, udp_sock: c_int) -> c_int;
    fn srt_listen_callback(
        sock: SrtSocket,
        callback: Option<ListenCallback>,
        opaque: *mut c_void,
    ) -> c_int;
    fn srt_listen(sock: SrtSocket, backlog: c_int) -> c_int;
    fn srt_accept(sock: SrtSocket, addr: *mut c_void, len: *mut c_int) -> SrtSocket;
    fn srt_close(sock: SrtSocket) -> c_int;
    fn srt_getsockstate(sock: SrtSocket) -> c_int;
    fn srt_send(sock: SrtSocket, data: *const c_char, len: c_int) -> c_int;
    fn srt_getlasterror_str() -> *const c_char;
}

/// SRT operations backed by libsrt.
#[derive(Debug, Default, Clone, Copy)]
pub struct LibSrtOps;

impl LibSrtOps {
    /// Initializes the library (`srt_startup`).
    pub fn startup() -> bool {
        // SAFETY: srt_startup has no preconditions and returns 0 on success.
        unsafe { srt_startup() != SRT_ERROR }
    }

    /// Releases the library (`srt_cleanup`) after the listener is closed.
    pub fn cleanup() -> bool {
        // SAFETY: srt_cleanup has no preconditions; callers ensure no listener
        // uses the library afterwards.
        unsafe { srt_cleanup() != SRT_ERROR }
    }

    /// Last libsrt error as text.
    pub fn last_error() -> String {
        // SAFETY: srt_getlasterror_str returns a NUL-terminated static string.
        unsafe {
            let message = srt_getlasterror_str();
            if message.is_null() {
                return String::new();
            }
            CStr::from_ptr(message).to_string_lossy().into_owned()
        }
    }
}

impl SrtOps for LibSrtOps {
    fn create_socket(&self) -> i32 {
        // SAFETY: srt_create_socket has no preconditions.
        unsafe { srt_create_socket() }
    }

    fn bind_acquire(&self, sock: i32, udp_fd: i32) -> bool {
        // SAFETY: sock is a live SRT socket and udp_fd a bound UDP descriptor.
        unsafe { srt_bind_acquire(sock, udp_fd) != SRT_ERROR }
    }

    fn set_accept_callback(&self, sock: i32, pending: Arc<Mutex<PendingPool>>) -> bool {
        let opaque = Arc::as_ptr(&pending) as *mut c_void;
        // SAFETY: the listener owns `pending` for as long as callbacks can run;
        // the trampoline only dereferences this pointer while that Arc is live.
        unsafe { srt_listen_callback(sock, Some(callback), opaque) != SRT_ERROR }
    }

    fn listen(&self, sock: i32, backlog: i32) -> bool {
        // SAFETY: sock is a bound SRT socket; backlog is validated by libsrt.
        unsafe { srt_listen(sock, if backlog > 0 { backlog } else { SRT_BACKLOG }) != SRT_ERROR }
    }

    fn accept(&self, sock: i32) -> i32 {
        // SAFETY: a null address/length pair is allowed by srt_accept.
        unsafe { srt_accept(sock, ptr::null_mut(), ptr::null_mut()) }
    }

    fn close(&self, sock: i32) {
        if sock == SRT_INVALID_SOCK {
            return;
        }
        // SAFETY: closes an SRT socket; closing an already-closed handle is a
        // documented no-op error that we intentionally ignore.
        unsafe {
            srt_close(sock);
        }
    }

    fn is_connected(&self, sock: i32) -> bool {
        if sock == SRT_INVALID_SOCK {
            return false;
        }
        // SAFETY: srt_getsockstate only reads the socket state.
        unsafe { srt_getsockstate(sock) == SRTS_CONNECTED }
    }

    fn send(&self, sock: i32, data: &[u8]) -> i32 {
        let Ok(len) = c_int::try_from(data.len()) else {
            return SRT_ERROR;
        };
        // SAFETY: data is valid for `len` bytes and srt_send only reads it.
        unsafe { srt_send(sock, data.as_ptr().cast(), len) }
    }
}

unsafe extern "C" fn callback(
    opaque: *mut c_void,
    ns: SrtSocket,
    _hsversion: c_int,
    _peeraddr: *const c_void,
    streamid: *const c_char,
) -> c_int {
    if opaque.is_null() {
        return -1;
    }
    // SAFETY: opaque is the `Mutex<PendingPool>` pointer registered with the
    // listener; the backing Arc lives in the owning SrtListener.
    let pending = unsafe { &*(opaque as *const Mutex<PendingPool>) };
    let streamid = if streamid.is_null() {
        None
    } else {
        // SAFETY: libsrt passes a NUL-terminated stream id for the handshake.
        unsafe { CStr::from_ptr(streamid) }.to_str().ok()
    };
    let mut pool = pending.lock().expect("pending pool");
    match pool.accept(streamid, ns) {
        AcceptDecision::Accepted { replaced } => {
            if replaced != SRT_INVALID_SOCK {
                // SAFETY: replaced was accepted earlier by this listener.
                unsafe {
                    srt_close(replaced);
                }
            }
            0
        }
        AcceptDecision::Rejected => -1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::srt::{SRT_INVALID_SOCK, SrtListener};
    use std::ffi::CString;
    use std::mem::size_of;
    use std::net::UdpSocket;
    use std::os::fd::IntoRawFd;
    use std::thread;
    use std::time::Duration;

    const SRTO_CONNTIMEO: c_int = 36;
    const SRTO_STREAMID: c_int = 46;
    const AF_INET: u16 = 2;

    unsafe extern "C" {
        fn srt_setsockflag(
            sock: SrtSocket,
            option: c_int,
            value: *const c_void,
            length: c_int,
        ) -> c_int;
        fn srt_connect(sock: SrtSocket, addr: *const c_void, length: c_int) -> c_int;
    }

    #[repr(C)]
    struct SockAddrIn {
        family: u16,
        port: u16,
        addr: u32,
        zero: [u8; 8],
    }

    fn free_udp_fd() -> (i32, u16) {
        let socket = UdpSocket::bind(("127.0.0.1", 0)).expect("free udp port");
        let port = socket.local_addr().expect("local addr").port();
        (socket.into_raw_fd(), port)
    }

    fn client_connect(port: u16, streamid: &str) -> i32 {
        // SAFETY: srt_create_socket has no preconditions.
        let sock = unsafe { srt_create_socket() };
        assert_ne!(sock, SRT_INVALID_SOCK, "client socket create");
        let streamid = CString::new(streamid).expect("streamid");
        // SAFETY: the string pointer is valid for its length.
        let rc = unsafe {
            srt_setsockflag(
                sock,
                SRTO_STREAMID,
                streamid.as_ptr().cast(),
                streamid.as_bytes().len() as c_int,
            )
        };
        assert_ne!(rc, SRT_ERROR, "set streamid");
        let timeout: c_int = 1500;
        // SAFETY: the option value points at a live c_int of the given size.
        let rc = unsafe {
            srt_setsockflag(
                sock,
                SRTO_CONNTIMEO,
                (&timeout as *const c_int).cast(),
                size_of::<c_int>() as c_int,
            )
        };
        assert_ne!(rc, SRT_ERROR, "set connect timeout");
        let addr = SockAddrIn {
            family: AF_INET,
            port: port.to_be(),
            addr: u32::from_ne_bytes([127, 0, 0, 1]),
            zero: [0; 8],
        };
        // SAFETY: addr is a valid sockaddr_in for the duration of the call.
        let rc = unsafe {
            srt_connect(
                sock,
                (&addr as *const SockAddrIn).cast(),
                size_of::<SockAddrIn>() as c_int,
            )
        };
        assert_ne!(rc, SRT_ERROR, "client connect must succeed");
        sock
    }

    // Ported from tests/test_srt.cpp:35
    #[test]
    fn test_srt_listener_drain_and_close() {
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
            // SAFETY: both handles are live SRT sockets owned by this test.
            unsafe {
                srt_close(srv);
                srt_close(client);
            }
        }

        listener.close();
        assert!(!listener.is_init(), "listener must report closed");
        // A second cycle proves close left no wedged drain behind.
        let (udp_fd, _port) = free_udp_fd();
        listener.init(udp_fd).expect("reinit must work after close");
        listener.close();
        assert!(LibSrtOps::cleanup());
    }
}
