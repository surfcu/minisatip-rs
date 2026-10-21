//! Linux socket helpers used by the satipc driver.
//!
//! These are the parts of `socketworks.cpp` that `std::net` cannot express:
//! `tcp_connect_src` (source-address bind), `udp_bind` (address bind and
//! `SO_REUSEADDR`/`SO_REUSEPORT`) and `set_socket_receive_buffer`
//! (`SO_RCVBUFFORCE` with a `SO_RCVBUF` fallback). Unsafe is confined here.
#![allow(unsafe_code)]

use std::io;
use std::mem::size_of;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::os::fd::{FromRawFd, RawFd};
use std::time::Duration;

/// `tcp_connect_src`: connects to `target`, binding `source` first.
///
/// A source of the wrong address family is ignored (socketworks.cpp:427-445),
/// while a source that cannot be resolved fails the connection. Without a
/// usable source this is a plain `connect_timeout`.
pub fn tcp_connect_src(
    target: SocketAddr,
    source: Option<&str>,
    timeout: Duration,
) -> io::Result<TcpStream> {
    let source = match source.filter(|source| !source.is_empty()) {
        Some(source) => resolve_source(source, target.is_ipv6())?,
        None => None,
    };
    let Some(source) = source else {
        return TcpStream::connect_timeout(&target, timeout);
    };

    // SAFETY: socket(2) is called with valid constants; a negative result is checked.
    let fd = unsafe {
        libc::socket(
            family(target),
            libc::SOCK_STREAM | libc::SOCK_CLOEXEC,
            libc::IPPROTO_TCP,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fd is a fresh descriptor owned by this function; from_raw_fd takes
    // ownership so every error path below closes it.
    let stream = unsafe { TcpStream::from_raw_fd(fd) };
    stream.set_nonblocking(true)?;

    let (address, length) = sockaddr(source);
    // SAFETY: address points to a sockaddr of the size passed.
    if unsafe { libc::bind(fd, (&raw const address).cast(), length) } < 0 {
        return Err(io::Error::last_os_error());
    }

    let (address, length) = sockaddr(target);
    // SAFETY: address points to a sockaddr of the size passed.
    let result = unsafe { libc::connect(fd, (&raw const address).cast(), length) };
    if result < 0 {
        let error = io::Error::last_os_error();
        if !matches!(
            error.raw_os_error(),
            Some(libc::EINPROGRESS) | Some(libc::EINTR)
        ) {
            return Err(error);
        }
        wait_writable(fd, timeout)?;
        let pending = socket_error(fd)?;
        if pending != 0 {
            return Err(io::Error::from_raw_os_error(pending));
        }
    }
    Ok(stream)
}

/// `udp_bind`: binds `ip` (or the wildcard) and port `port`.
///
/// The C++ helper always sets `SO_REUSEADDR` and, when available,
/// `SO_REUSEPORT` (socketworks.cpp:293-304).
pub fn udp_bind(ip: Option<IpAddr>, port: u16) -> io::Result<UdpSocket> {
    let address = SocketAddr::new(ip.unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED)), port);
    // SAFETY: socket(2) is called with valid constants; a negative result is checked.
    let fd = unsafe {
        libc::socket(
            family(address),
            libc::SOCK_DGRAM | libc::SOCK_CLOEXEC,
            libc::IPPROTO_UDP,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fd is a fresh descriptor owned by this function; from_raw_fd takes
    // ownership so every error path below closes it.
    let socket = unsafe { UdpSocket::from_raw_fd(fd) };
    let enable: libc::c_int = 1;
    // SAFETY: enable is a valid int of the given size for these options.
    if unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_REUSEADDR,
            (&raw const enable).cast(),
            size_of::<libc::c_int>() as libc::socklen_t,
        )
    } < 0
    {
        return Err(io::Error::last_os_error());
    }
    // SO_REUSEPORT is best effort, like the C++ helper.
    // SAFETY: as above.
    unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_REUSEPORT,
            (&raw const enable).cast(),
            size_of::<libc::c_int>() as libc::socklen_t,
        );
    }
    let (address, length) = sockaddr(address);
    // SAFETY: address points to a sockaddr of the size passed.
    if unsafe { libc::bind(fd, (&raw const address).cast(), length) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(socket)
}

/// `set_socket_receive_buffer`: `SO_RCVBUFFORCE`, then `SO_RCVBUF`.
///
/// Failures are ignored, matching the C++ helper's log-and-continue.
pub fn set_receive_buffer(fd: RawFd, length: i32) {
    if length <= 0 {
        return;
    }
    // SAFETY: length is a valid int of the size passed for SO_RCVBUF*.
    let forced = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_RCVBUFFORCE,
            (&raw const length).cast(),
            size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if forced != 0 {
        // SAFETY: as above.
        unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_RCVBUF,
                (&raw const length).cast(),
                size_of::<libc::c_int>() as libc::socklen_t,
            );
        }
    }
}

fn resolve_source(source: &str, ipv6: bool) -> io::Result<Option<SocketAddr>> {
    let addresses: Vec<SocketAddr> = (source, 0).to_socket_addrs()?.collect();
    if addresses.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("cannot resolve source {source}"),
        ));
    }
    Ok(addresses
        .into_iter()
        .find(|address| address.is_ipv6() == ipv6))
}

fn family(address: SocketAddr) -> libc::c_int {
    if address.is_ipv6() {
        libc::AF_INET6
    } else {
        libc::AF_INET
    }
}

fn sockaddr(address: SocketAddr) -> (libc::sockaddr_storage, libc::socklen_t) {
    // SAFETY: sockaddr_storage is a plain C struct; zero is a valid value.
    let mut storage: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
    match address {
        SocketAddr::V4(address) => {
            let sin = libc::sockaddr_in {
                sin_family: libc::AF_INET as libc::sa_family_t,
                sin_port: address.port().to_be(),
                sin_addr: libc::in_addr {
                    s_addr: u32::from_ne_bytes(address.ip().octets()),
                },
                sin_zero: [0; 8],
            };
            // SAFETY: storage is large and aligned enough for sockaddr_in.
            unsafe {
                std::ptr::write((&raw mut storage).cast::<libc::sockaddr_in>(), sin);
            }
            (storage, size_of::<libc::sockaddr_in>() as libc::socklen_t)
        }
        SocketAddr::V6(address) => {
            let sin6 = libc::sockaddr_in6 {
                sin6_family: libc::AF_INET6 as libc::sa_family_t,
                sin6_port: address.port().to_be(),
                sin6_flowinfo: address.flowinfo(),
                sin6_addr: libc::in6_addr {
                    s6_addr: address.ip().octets(),
                },
                sin6_scope_id: address.scope_id(),
            };
            // SAFETY: storage is large and aligned enough for sockaddr_in6.
            unsafe {
                std::ptr::write((&raw mut storage).cast::<libc::sockaddr_in6>(), sin6);
            }
            (storage, size_of::<libc::sockaddr_in6>() as libc::socklen_t)
        }
    }
}

fn wait_writable(fd: RawFd, timeout: Duration) -> io::Result<()> {
    let millis = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
    let mut descriptor = libc::pollfd {
        fd,
        events: libc::POLLOUT,
        revents: 0,
    };
    loop {
        // SAFETY: descriptor points to one valid pollfd.
        let ready = unsafe { libc::poll(&mut descriptor, 1, millis) };
        if ready > 0 {
            return Ok(());
        }
        if ready == 0 {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "connect timed out"));
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

fn socket_error(fd: RawFd) -> io::Result<i32> {
    let mut error: libc::c_int = 0;
    let mut length = size_of::<libc::c_int>() as libc::socklen_t;
    // SAFETY: error and length are valid for SO_ERROR.
    let result = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_ERROR,
            (&raw mut error).cast(),
            &mut length,
        )
    };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::os::fd::AsRawFd;

    #[test]
    fn source_address_is_bound() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("listen");
        let target = listener.local_addr().expect("addr");
        let stream =
            tcp_connect_src(target, Some("127.0.0.2"), Duration::from_secs(2)).expect("connect");
        let (peer, _) = listener.accept().expect("accept");
        assert_eq!(
            peer.peer_addr().expect("peer").ip(),
            Ipv4Addr::new(127, 0, 0, 2)
        );
        drop(stream);
    }

    #[test]
    fn wrong_family_source_is_ignored() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("listen");
        let target = listener.local_addr().expect("addr");
        let stream = tcp_connect_src(target, Some("::1"), Duration::from_secs(2)).expect("connect");
        let (peer, _) = listener.accept().expect("accept");
        assert_eq!(peer.peer_addr().expect("peer").ip(), Ipv4Addr::LOCALHOST);
        drop(stream);
    }

    #[test]
    fn unparseable_source_fails() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("listen");
        let target = listener.local_addr().expect("addr");
        assert!(tcp_connect_src(target, Some("not an address"), Duration::from_secs(2)).is_err());
    }

    #[test]
    fn udp_bind_uses_requested_address() {
        let socket = udp_bind(Some(Ipv4Addr::new(127, 0, 0, 2).into()), 0).expect("bind");
        assert_eq!(
            socket.local_addr().expect("addr").ip(),
            Ipv4Addr::new(127, 0, 0, 2)
        );
    }

    #[test]
    fn receive_buffer_is_applied() {
        let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
        let mut before: libc::c_int = 0;
        let mut length = size_of::<libc::c_int>() as libc::socklen_t;
        // SAFETY: before/length are valid for SO_RCVBUF.
        let ok = unsafe {
            libc::getsockopt(
                socket.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_RCVBUF,
                (&raw mut before).cast(),
                &mut length,
            )
        };
        assert_eq!(ok, 0);
        set_receive_buffer(socket.as_raw_fd(), 1024);
        let mut after: libc::c_int = 0;
        let mut length = size_of::<libc::c_int>() as libc::socklen_t;
        // SAFETY: after/length are valid for SO_RCVBUF.
        let ok = unsafe {
            libc::getsockopt(
                socket.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_RCVBUF,
                (&raw mut after).cast(),
                &mut length,
            )
        };
        assert_eq!(ok, 0);
        assert!(before > after, "default {before} must shrink to {after}");
        assert!(after >= 1024, "requested size must be kept");
    }
}
