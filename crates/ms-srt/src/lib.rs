#![cfg(feature = "srt")]
#![allow(unsafe_code)]
#![deny(clippy::undocumented_unsafe_blocks)]
//! Hand-written libsrt bindings shared by the SRT output (`ms-net`) and the
//! SAT>IP client SRT transport (`ms-tuner`). This is the only unsafe code for
//! SRT: callers get safe wrappers over the calls `srt.cpp` and `satipc.cpp` use.
//!
//! Declarations mirror `srt/srt.h`; the crate links `srt` and compiles only when
//! the `srt` feature is enabled (see build.rs).

use std::collections::HashMap;
use std::ffi::{CStr, c_char, c_int, c_void};
use std::mem::size_of;
use std::net::SocketAddr;
use std::ptr;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};

/// SRT socket handle (`SRTSOCKET`).
pub type SrtSocket = i32;

/// Invalid SRT socket value (`SRT_INVALID_SOCK`).
pub const SRT_INVALID_SOCK: SrtSocket = -1;
/// `SRT_EASYNCRCV`: no data is ready on a non-blocking receive.
pub const SRT_EASYNCRCV: i32 = 6002;

const SRT_ERROR: c_int = -1;
const SRTS_CONNECTED: c_int = 5;
const SRTO_STREAMID: c_int = 46;

/// Integer socket options (`SRT_SOCKOPT`) used by minisatip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SockOpt {
    /// `SRTO_RCVSYN`: blocking receive.
    RcvSyn,
    /// `SRTO_RCVTIMEO`: receive timeout in ms.
    RcvTimeo,
    /// `SRTO_LATENCY`: receiver and peer latency in ms.
    Latency,
    /// `SRTO_CONNTIMEO`: connect timeout in ms.
    ConnTimeo,
}

impl SockOpt {
    fn code(self) -> c_int {
        match self {
            Self::RcvSyn => 2,
            Self::RcvTimeo => 14,
            Self::Latency => 23,
            Self::ConnTimeo => 36,
        }
    }
}

/// Pre-accept handler: receives the stream id and the new socket, and returns
/// whether the connection is accepted.
pub type PreAccept = dyn Fn(Option<&str>, SrtSocket) -> bool + Send + Sync;

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
    fn srt_connect(sock: SrtSocket, addr: *const c_void, len: c_int) -> c_int;
    fn srt_close(sock: SrtSocket) -> c_int;
    fn srt_getsockstate(sock: SrtSocket) -> c_int;
    fn srt_setsockflag(sock: SrtSocket, opt: c_int, value: *const c_void, len: c_int) -> c_int;
    fn srt_send(sock: SrtSocket, data: *const c_char, len: c_int) -> c_int;
    fn srt_recv(sock: SrtSocket, data: *mut c_char, len: c_int) -> c_int;
    fn srt_getlasterror(errno_loc: *mut c_int) -> c_int;
    fn srt_getlasterror_str() -> *const c_char;
}

/// Pre-accept handlers keyed by listener socket. The C callback receives the
/// listener id as an integer, so no Rust pointer crosses the FFI boundary.
static LISTEN_CALLBACKS: LazyLock<Mutex<HashMap<SrtSocket, Arc<PreAccept>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn listen_callbacks() -> MutexGuard<'static, HashMap<SrtSocket, Arc<PreAccept>>> {
    LISTEN_CALLBACKS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Initializes the library (`srt_startup`).
pub fn startup() -> bool {
    // SAFETY: srt_startup has no preconditions and returns -1 on failure.
    unsafe { srt_startup() != SRT_ERROR }
}

/// Releases the library (`srt_cleanup`).
pub fn cleanup() -> bool {
    // SAFETY: srt_cleanup has no preconditions; callers ensure no socket uses
    // the library afterwards.
    unsafe { srt_cleanup() != SRT_ERROR }
}

/// Last libsrt error as text (`srt_getlasterror_str`).
pub fn last_error() -> String {
    // SAFETY: srt_getlasterror_str has no preconditions.
    let message = unsafe { srt_getlasterror_str() };
    if message.is_null() {
        return String::new();
    }
    // SAFETY: a non-null result is a NUL-terminated string owned by libsrt
    // that stays valid until the next SRT call on this thread.
    unsafe { CStr::from_ptr(message) }
        .to_string_lossy()
        .into_owned()
}

/// Last libsrt error code (`srt_getlasterror(NULL)`).
pub fn last_error_code() -> i32 {
    // SAFETY: a null errno location is allowed.
    unsafe { srt_getlasterror(ptr::null_mut()) }
}

/// Creates a socket (`srt_create_socket`); [`SRT_INVALID_SOCK`] on failure.
pub fn create_socket() -> SrtSocket {
    // SAFETY: srt_create_socket has no preconditions.
    unsafe { srt_create_socket() }
}

/// Binds `sock` to the bound UDP descriptor `udp_fd`, which libsrt then owns
/// and closes with the socket (`srt_bind_acquire`).
pub fn bind_acquire(sock: SrtSocket, udp_fd: i32) -> bool {
    // SAFETY: libsrt validates both handles and reports invalid ones as errors.
    unsafe { srt_bind_acquire(sock, udp_fd) != SRT_ERROR }
}

/// Installs `handler` as the pre-accept callback of listener `sock`
/// (`srt_listen_callback`). The handler is dropped when `sock` is closed with
/// [`close`].
pub fn set_listen_callback(sock: SrtSocket, handler: Arc<PreAccept>) -> bool {
    listen_callbacks().insert(sock, handler);
    let opaque = ptr::without_provenance_mut::<c_void>(sock as usize);
    // SAFETY: the trampoline matches ListenCallback and uses `opaque` only as
    // an integer key, never as a pointer.
    let installed =
        unsafe { srt_listen_callback(sock, Some(listen_trampoline), opaque) != SRT_ERROR };
    if !installed {
        listen_callbacks().remove(&sock);
    }
    installed
}

/// Starts listening with `backlog` (`srt_listen`).
pub fn listen(sock: SrtSocket, backlog: i32) -> bool {
    // SAFETY: libsrt validates the socket and backlog.
    unsafe { srt_listen(sock, backlog) != SRT_ERROR }
}

/// Blocking accept (`srt_accept`); [`SRT_INVALID_SOCK`] once the listener closes.
pub fn accept(sock: SrtSocket) -> SrtSocket {
    // SAFETY: a null address/length pair is allowed by srt_accept.
    unsafe { srt_accept(sock, ptr::null_mut(), ptr::null_mut()) }
}

/// Connects `sock` to `addr` (`srt_connect`); blocks unless the socket is
/// non-blocking.
pub fn connect(sock: SrtSocket, addr: SocketAddr) -> bool {
    let rc = match addr {
        SocketAddr::V4(v4) => {
            let sin = libc::sockaddr_in {
                sin_family: libc::AF_INET as libc::sa_family_t,
                sin_port: v4.port().to_be(),
                sin_addr: libc::in_addr {
                    s_addr: u32::from_ne_bytes(v4.ip().octets()),
                },
                sin_zero: [0; 8],
            };
            // SAFETY: sin is a valid sockaddr_in of the given size for the call.
            unsafe {
                srt_connect(
                    sock,
                    (&raw const sin).cast(),
                    size_of::<libc::sockaddr_in>() as c_int,
                )
            }
        }
        SocketAddr::V6(v6) => {
            let sin6 = libc::sockaddr_in6 {
                sin6_family: libc::AF_INET6 as libc::sa_family_t,
                sin6_port: v6.port().to_be(),
                sin6_flowinfo: v6.flowinfo(),
                sin6_addr: libc::in6_addr {
                    s6_addr: v6.ip().octets(),
                },
                sin6_scope_id: v6.scope_id(),
            };
            // SAFETY: sin6 is a valid sockaddr_in6 of the given size for the call.
            unsafe {
                srt_connect(
                    sock,
                    (&raw const sin6).cast(),
                    size_of::<libc::sockaddr_in6>() as c_int,
                )
            }
        }
    };
    rc != SRT_ERROR
}

/// Closes a socket (`srt_close`) and drops its pre-accept handler; closing the
/// listener also wakes a blocking accept.
pub fn close(sock: SrtSocket) {
    if sock == SRT_INVALID_SOCK {
        return;
    }
    listen_callbacks().remove(&sock);
    // SAFETY: closing a stale handle is a reported error that is ignored.
    unsafe {
        srt_close(sock);
    }
}

/// Whether `sock` is in `SRTS_CONNECTED` (`srt_getsockstate`).
pub fn is_connected(sock: SrtSocket) -> bool {
    if sock == SRT_INVALID_SOCK {
        return false;
    }
    // SAFETY: srt_getsockstate only reads the socket state.
    unsafe { srt_getsockstate(sock) == SRTS_CONNECTED }
}

/// Sets an integer socket option (`srt_setsockflag`).
pub fn set_flag(sock: SrtSocket, opt: SockOpt, value: i32) -> bool {
    let value: c_int = value;
    // SAFETY: the option value points at a live c_int of the given size.
    unsafe {
        srt_setsockflag(
            sock,
            opt.code(),
            (&raw const value).cast(),
            size_of::<c_int>() as c_int,
        ) != SRT_ERROR
    }
}

/// Sets `SRTO_STREAMID`.
pub fn set_streamid(sock: SrtSocket, streamid: &str) -> bool {
    let Ok(len) = c_int::try_from(streamid.len()) else {
        return false;
    };
    // SAFETY: the string bytes are valid for `len` bytes; libsrt copies them.
    unsafe { srt_setsockflag(sock, SRTO_STREAMID, streamid.as_ptr().cast(), len) != SRT_ERROR }
}

/// Sends bytes (`srt_send`); the byte count, or a negative value on failure.
pub fn send(sock: SrtSocket, data: &[u8]) -> i32 {
    let Ok(len) = c_int::try_from(data.len()) else {
        return SRT_ERROR;
    };
    // SAFETY: data is valid for `len` bytes and srt_send only reads it.
    unsafe { srt_send(sock, data.as_ptr().cast(), len) }
}

/// Receives one message into `buf` (`srt_recv`); the byte count, or a negative
/// value on failure.
pub fn recv(sock: SrtSocket, buf: &mut [u8]) -> i32 {
    let Ok(len) = c_int::try_from(buf.len()) else {
        return SRT_ERROR;
    };
    // SAFETY: buf is writable for `len` bytes and srt_recv writes at most that.
    unsafe { srt_recv(sock, buf.as_mut_ptr().cast(), len) }
}

unsafe extern "C" fn listen_trampoline(
    opaque: *mut c_void,
    ns: SrtSocket,
    _hsversion: c_int,
    _peeraddr: *const c_void,
    streamid: *const c_char,
) -> c_int {
    let listener = opaque.addr() as SrtSocket;
    let Some(handler) = listen_callbacks().get(&listener).cloned() else {
        return -1;
    };
    let streamid = if streamid.is_null() {
        None
    } else {
        // SAFETY: libsrt passes a NUL-terminated stream id for the handshake.
        unsafe { CStr::from_ptr(streamid) }.to_str().ok()
    };
    if handler(streamid, ns) { 0 } else { -1 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, UdpSocket};
    use std::os::fd::IntoRawFd;

    /// libsrt startup/cleanup are process-global; tests run serially.
    static SRT_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn closed_udp_port() -> u16 {
        let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("udp bind");
        socket.local_addr().expect("local addr").port()
    }

    #[test]
    fn socket_lifecycle_and_failed_connect() {
        let _serial = SRT_TEST_LOCK.lock().expect("srt test lock");
        assert!(startup());
        let sock = create_socket();
        assert_ne!(sock, SRT_INVALID_SOCK, "srt_create_socket");
        assert!(!is_connected(sock), "fresh socket must not be connected");
        assert!(set_streamid(sock, "12345"));
        assert!(set_flag(sock, SockOpt::Latency, 1000));
        assert!(set_flag(sock, SockOpt::ConnTimeo, 300));
        let udp = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("udp bind");
        assert!(bind_acquire(sock, udp.into_raw_fd()));
        let port = closed_udp_port();
        assert!(
            !connect(sock, SocketAddr::from((Ipv4Addr::LOCALHOST, port))),
            "connect to a closed port must fail"
        );
        assert!(!last_error().is_empty());
        close(sock);
        assert!(!is_connected(sock), "closed socket must read down");
        assert!(!is_connected(SRT_INVALID_SOCK));
        assert!(cleanup());
    }

    #[test]
    fn listen_callback_is_dropped_on_close() {
        let _serial = SRT_TEST_LOCK.lock().expect("srt test lock");
        assert!(startup());
        let sock = create_socket();
        assert_ne!(sock, SRT_INVALID_SOCK, "srt_create_socket");
        let udp = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("udp bind");
        assert!(bind_acquire(sock, udp.into_raw_fd()));
        let handler: Arc<PreAccept> = Arc::new(|_streamid, _sock| true);
        assert!(set_listen_callback(sock, handler.clone()));
        assert_eq!(Arc::strong_count(&handler), 2, "registry holds the handler");
        close(sock);
        assert_eq!(Arc::strong_count(&handler), 1, "close drops the handler");
        assert!(cleanup());
    }
}
