//! Linux FFI for interface lookup and `SO_BINDTODEVICE`; unsafe is confined here.
#![allow(unsafe_code)]

use crate::bind::{BindDevOps, BindDevQuery, BindDevStatus, DevLookup, IF_NAMESIZE};
use std::io;
use std::net::{Ipv4Addr, SocketAddr, TcpListener, UdpSocket};
use std::os::fd::{FromRawFd, RawFd};

const SO_BINDTODEVICE: libc::c_int = 25;
/// `IPTOS_DSCP_MASK_VALUE` (socketworks.h:61).
const IPTOS_DSCP_MASK_VALUE: libc::c_int = 0xfc;
/// `IPTOS_DSCP_EF` (socketworks.h:60).
pub const IPTOS_DSCP_EF: libc::c_int = 0xb8;

/// Creates a UDP socket with `SO_REUSEADDR` (fatal on failure) and a
/// best-effort `SO_REUSEPORT`, then binds it (`udp_bind`,
/// socketworks.cpp:250-322). `bind_dev` applies `SO_BINDTODEVICE` first.
pub fn udp_bind_reuse(addr: SocketAddr, bind_dev: Option<&str>) -> io::Result<UdpSocket> {
    // SAFETY: socket(2) is called with valid constants and the result checked.
    let fd = unsafe { libc::socket(family_of(addr), libc::SOCK_DGRAM, libc::IPPROTO_UDP) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let result = (|| {
        // C++ closes the socket when set_socket_bind_dev returns -1
        // (socketworks.cpp:266-269).
        if let Some(dev) = bind_dev
            && !set_socket_bind_dev(fd, dev)
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("cannot bind UDP socket to {dev}"),
            ));
        }
        set_socket_int(fd, libc::SO_REUSEADDR, 1)?;
        // C++ only logs a failure here (socketworks.cpp:299-304).
        let _ = set_socket_int(fd, libc::SO_REUSEPORT, 1);
        // SAFETY: the storage is filled by `sockaddr_storage_of` and passed
        // with its matching length.
        let (storage, length) = sockaddr_storage_of(addr);
        let bound =
            unsafe { libc::bind(fd, (&raw const storage).cast::<libc::sockaddr>(), length) };
        if bound < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    })();
    if let Err(error) = result {
        // SAFETY: fd is owned by this function until from_raw_fd.
        unsafe { libc::close(fd) };
        return Err(error);
    }
    // SAFETY: the descriptor was created above and is transferred to the socket.
    Ok(unsafe { UdpSocket::from_raw_fd(fd) })
}

/// Creates a listening TCP socket (`tcp_listen`, socketworks.cpp:469-505).
///
/// `SO_BINDTODEVICE` is applied before `bind`, and a hard failure closes the
/// socket as C++ does; `SO_REUSEADDR` is fatal and the backlog is 10.
pub fn tcp_listen(addr: SocketAddr, bind_dev: Option<&str>) -> io::Result<TcpListener> {
    // SAFETY: socket(2) is called with valid constants and the result checked.
    let fd = unsafe { libc::socket(family_of(addr), libc::SOCK_STREAM, libc::IPPROTO_IP) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let result = (|| {
        if let Some(dev) = bind_dev
            && !set_socket_bind_dev(fd, dev)
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("cannot bind TCP socket to {dev}"),
            ));
        }
        set_socket_int(fd, libc::SO_REUSEADDR, 1)?;
        // SAFETY: the storage is filled by `sockaddr_storage_of` and passed
        // with its matching length.
        let (storage, length) = sockaddr_storage_of(addr);
        let bound =
            unsafe { libc::bind(fd, (&raw const storage).cast::<libc::sockaddr>(), length) };
        if bound < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a successfully bound stream socket accepts a listen call.
        if unsafe { libc::listen(fd, 10) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    })();
    if let Err(error) = result {
        // SAFETY: fd is owned by this function until from_raw_fd.
        unsafe { libc::close(fd) };
        return Err(error);
    }
    // SAFETY: the descriptor was created above and is transferred to the listener.
    Ok(unsafe { TcpListener::from_raw_fd(fd) })
}

/// Applies `SO_BINDTODEVICE` to an existing socket (`set_socket_bind_dev`).
///
/// Returns `false` on a hard error; `EPERM`/`EACCES` degrades to address
/// binding and is reported as success, like C++ (socketworks.cpp:180-205).
pub fn set_socket_bind_dev(fd: RawFd, dev: &str) -> bool {
    !matches!(RawBindDev::new(fd).set_bind_dev(dev), BindDevStatus::Failed)
}

/// Sets the send buffer to `len`, preferring `SO_SNDBUFFORCE`
/// (socketworks.cpp:1139-1156). Failures are ignored like C++'s log-only path.
pub fn set_socket_send_buffer(fd: RawFd, len: libc::c_int) {
    if len <= 0 {
        return;
    }
    // SAFETY: `len` is a valid c_int of the given size.
    let forced = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_SNDBUFFORCE,
            (&raw const len).cast(),
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if forced != 0 {
        // SAFETY: same as above; the return value is only checked for fallback.
        unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_SNDBUF,
                (&raw const len).cast(),
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            );
        }
    }
}

/// Marks a socket with DSCP `dscp` and `SO_PRIORITY` `priority`
/// (`set_socket_dscp`, socketworks.cpp:1618-1632).
pub fn set_socket_dscp(fd: RawFd, dscp: libc::c_int, priority: libc::c_int) {
    let tos = dscp & IPTOS_DSCP_MASK_VALUE;
    // SAFETY: `tos` is a valid c_int of the given size.
    unsafe {
        libc::setsockopt(
            fd,
            libc::IPPROTO_IP,
            libc::IP_TOS,
            (&raw const tos).cast(),
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        );
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PRIORITY,
            (&raw const priority).cast(),
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        );
    }
}

/// Reads the DSCP/TOS byte of a socket (`getsockopt(IP_TOS)`).
pub fn socket_tos(fd: RawFd) -> io::Result<libc::c_int> {
    socket_int(fd, libc::IPPROTO_IP, libc::IP_TOS)
}

/// Reads `SO_SNDBUF` (used by `set_socket_send_buffer`'s log path).
pub fn socket_send_buffer(fd: RawFd) -> io::Result<libc::c_int> {
    socket_int(fd, libc::SOL_SOCKET, libc::SO_SNDBUF)
}

fn socket_int(fd: RawFd, level: libc::c_int, option: libc::c_int) -> io::Result<libc::c_int> {
    let mut value: libc::c_int = 0;
    let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
    // SAFETY: getsockopt writes a c_int into `value` and at most `len` bytes.
    let result = unsafe { libc::getsockopt(fd, level, option, (&raw mut value).cast(), &mut len) };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(value)
}

/// Joins `group` on `interface` (`IP_ADD_MEMBERSHIP`).
pub fn join_multicast_v4(fd: RawFd, group: Ipv4Addr, interface: Ipv4Addr) -> io::Result<()> {
    let request = libc::ip_mreq {
        imr_multiaddr: in_addr(group),
        imr_interface: in_addr(interface),
    };
    // SAFETY: `request` is a valid ip_mreq of exactly the given size.
    let result = unsafe {
        libc::setsockopt(
            fd,
            libc::IPPROTO_IP,
            libc::IP_ADD_MEMBERSHIP,
            (&raw const request).cast(),
            std::mem::size_of::<libc::ip_mreq>() as libc::socklen_t,
        )
    };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Selects the outgoing multicast interface (`IP_MULTICAST_IF`).
pub fn set_multicast_if(fd: RawFd, interface: Ipv4Addr) -> io::Result<()> {
    let address = in_addr(interface);
    // SAFETY: the in_addr is valid and owned by this call.
    let result = unsafe {
        libc::setsockopt(
            fd,
            libc::IPPROTO_IP,
            libc::IP_MULTICAST_IF,
            (&raw const address).cast(),
            std::mem::size_of::<libc::in_addr>() as libc::socklen_t,
        )
    };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn family_of(addr: SocketAddr) -> libc::c_int {
    match addr {
        SocketAddr::V4(_) => libc::AF_INET,
        SocketAddr::V6(_) => libc::AF_INET6,
    }
}

fn in_addr(address: Ipv4Addr) -> libc::in_addr {
    libc::in_addr {
        s_addr: u32::from_ne_bytes(address.octets()),
    }
}

fn set_socket_int(fd: RawFd, option: libc::c_int, value: libc::c_int) -> io::Result<()> {
    // SAFETY: `value` is a valid c_int of the given size.
    let result = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            option,
            (&raw const value).cast(),
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn sockaddr_storage_of(addr: SocketAddr) -> (libc::sockaddr_storage, libc::socklen_t) {
    // SAFETY: sockaddr_storage is a plain C union that is fully zeroed.
    let mut storage: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
    let length = match addr {
        SocketAddr::V4(v4) => {
            let sin = libc::sockaddr_in {
                sin_family: libc::AF_INET as libc::sa_family_t,
                sin_port: v4.port().to_be(),
                sin_addr: in_addr(*v4.ip()),
                sin_zero: [0; 8],
            };
            // SAFETY: sockaddr_in fits in sockaddr_storage.
            unsafe {
                std::ptr::write((&raw mut storage).cast::<libc::sockaddr_in>(), sin);
            }
            std::mem::size_of::<libc::sockaddr_in>()
        }
        SocketAddr::V6(v6) => {
            let mut sin6: libc::sockaddr_in6 = unsafe { std::mem::zeroed() };
            sin6.sin6_family = libc::AF_INET6 as libc::sa_family_t;
            sin6.sin6_port = v6.port().to_be();
            sin6.sin6_addr = libc::in6_addr {
                s6_addr: v6.ip().octets(),
            };
            sin6.sin6_flowinfo = v6.flowinfo();
            sin6.sin6_scope_id = v6.scope_id();
            // SAFETY: sockaddr_in6 fits in sockaddr_storage.
            unsafe {
                std::ptr::write((&raw mut storage).cast::<libc::sockaddr_in6>(), sin6);
            }
            std::mem::size_of::<libc::sockaddr_in6>()
        }
    };
    (storage, length as libc::socklen_t)
}

/// Real interface lookup backed by `SIOCGIFADDR`.
pub struct SystemDevices;

impl DevLookup for SystemDevices {
    fn dev_ip(&self, dev: &str) -> Option<String> {
        dev_ip(dev).map(|address| address.to_string())
    }
}

/// Returns the IPv4 address of a local interface.
pub fn dev_ip(dev: &str) -> Option<Ipv4Addr> {
    if dev.is_empty() || dev.len() >= IF_NAMESIZE {
        return None;
    }
    // SAFETY: socket(2) is called with valid constants and the result is checked.
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0) };
    if fd < 0 {
        return None;
    }
    // SAFETY: ifreq is a plain C struct that is fully zero-initialised here.
    let mut request: libc::ifreq = unsafe { std::mem::zeroed() };
    for (slot, byte) in request.ifr_name.iter_mut().zip(dev.as_bytes()) {
        *slot = *byte as libc::c_char;
    }
    // `libc::Ioctl` is the request type: c_ulong on glibc, c_int on musl.
    // SAFETY: SIOCGIFADDR writes an address into the zero-initialised ifreq.
    let result = unsafe { libc::ioctl(fd, libc::SIOCGIFADDR as libc::Ioctl, &mut request) };
    // SAFETY: fd was returned by socket(2) and is owned by this function.
    unsafe { libc::close(fd) };
    if result != 0 {
        return None;
    }
    // SAFETY: a successful SIOCGIFADDR leaves a sockaddr_in in the request union.
    let sin: libc::sockaddr_in = unsafe {
        std::ptr::read_unaligned(
            (&raw const request.ifr_ifru.ifru_addr).cast::<libc::sockaddr_in>(),
        )
    };
    if sin.sin_family != libc::AF_INET as libc::sa_family_t {
        return None;
    }
    Some(Ipv4Addr::from(sin.sin_addr.s_addr.to_ne_bytes()))
}

/// Borrowed descriptor used to apply or query `SO_BINDTODEVICE`.
pub struct RawBindDev {
    fd: RawFd,
}

impl RawBindDev {
    /// Wraps a descriptor; ownership stays with the caller.
    pub fn new(fd: RawFd) -> Self {
        Self { fd }
    }
}

impl BindDevOps for RawBindDev {
    fn set_bind_dev(&mut self, dev: &str) -> BindDevStatus {
        if dev.len() >= IF_NAMESIZE {
            return BindDevStatus::Failed;
        }
        let mut name = Vec::with_capacity(dev.len() + 1);
        name.extend_from_slice(dev.as_bytes());
        name.push(0);
        // SAFETY: setsockopt receives a NUL-terminated device name of the given length.
        let result = unsafe {
            libc::setsockopt(
                self.fd,
                libc::SOL_SOCKET,
                SO_BINDTODEVICE,
                name.as_ptr().cast(),
                name.len() as libc::socklen_t,
            )
        };
        if result == 0 {
            return BindDevStatus::Applied;
        }
        let errno = io::Error::last_os_error().raw_os_error().unwrap_or(0);
        if errno == libc::EPERM || errno == libc::EACCES {
            BindDevStatus::Degraded
        } else {
            BindDevStatus::Failed
        }
    }
}

impl BindDevQuery for RawBindDev {
    fn bound_device(&self) -> Option<String> {
        let mut buffer = [0u8; IF_NAMESIZE];
        let mut len = buffer.len() as libc::socklen_t;
        // SAFETY: getsockopt writes at most `len` bytes into the buffer and updates len.
        let result = unsafe {
            libc::getsockopt(
                self.fd,
                libc::SOL_SOCKET,
                SO_BINDTODEVICE,
                buffer.as_mut_ptr().cast(),
                &mut len,
            )
        };
        if result < 0 {
            return None;
        }
        let end = buffer
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(buffer.len());
        String::from_utf8(buffer[..end].to_vec()).ok()
    }
}
