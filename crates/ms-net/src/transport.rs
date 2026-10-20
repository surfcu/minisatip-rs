//! TCP/UDP address selection helpers, testable without kernel sockets.

use std::fmt;
use std::net::IpAddr;

/// Address family used for a socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpFamily {
    /// `AF_INET`.
    V4,
    /// `AF_INET6`.
    V6,
}

/// Low-level socket operations, faked in tests and backed by the OS elsewhere.
pub trait SocketOps {
    /// Creates a socket of `family`.
    fn socket(&mut self, family: IpFamily) -> Option<u32>;
    /// Applies `SO_REUSEADDR`.
    fn set_reuseaddr(&mut self, sock: u32) -> bool;
    /// Binds `sock` to `addr:port`.
    fn bind(&mut self, sock: u32, addr: &str, port: u16) -> bool;
    /// Starts listening with the C++ backlog of 10.
    fn listen(&mut self, sock: u32) -> bool;
    /// Connects `sock` to `addr:port`.
    fn connect(&mut self, sock: u32, addr: &str, port: u16) -> bool;
    /// Local address of `sock`, as reported by `getsockname`.
    fn local_host(&self, sock: u32) -> Option<String>;
    /// Local port of `sock`.
    fn local_port(&self, sock: u32) -> Option<u16>;
}

/// Why a transport helper failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportError {
    /// The host is not a literal IPv4/IPv6 address.
    Unresolved,
    /// Socket creation failed.
    Socket,
    /// A socket option could not be set.
    Options,
    /// `bind` failed.
    Bind,
    /// `listen` failed.
    Listen,
    /// `connect` failed.
    Connect,
    /// Binding the requested source address failed.
    Source,
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::Unresolved => "address could not be resolved",
            Self::Socket => "socket creation failed",
            Self::Options => "socket option failed",
            Self::Bind => "bind failed",
            Self::Listen => "listen failed",
            Self::Connect => "connect failed",
            Self::Source => "source bind failed",
        };
        f.write_str(message)
    }
}

impl std::error::Error for TransportError {}

/// Creates a listening TCP socket bound to `addr` or to ANY.
pub fn tcp_listen<O: SocketOps + ?Sized>(
    ops: &mut O,
    addr: Option<&str>,
    port: u16,
    ipv4_only: bool,
) -> Result<u32, TransportError> {
    let (family, bind_addr) = bind_address(addr, ipv4_only)?;
    let sock = ops.socket(family).ok_or(TransportError::Socket)?;
    if !ops.set_reuseaddr(sock) {
        return Err(TransportError::Options);
    }
    if !ops.bind(sock, &bind_addr, port) {
        return Err(TransportError::Bind);
    }
    if !ops.listen(sock) {
        return Err(TransportError::Listen);
    }
    Ok(sock)
}

/// Creates a UDP socket bound to `addr` or to ANY.
pub fn udp_bind<O: SocketOps + ?Sized>(
    ops: &mut O,
    addr: Option<&str>,
    port: u16,
    ipv4_only: bool,
) -> Result<u32, TransportError> {
    let (family, bind_addr) = bind_address(addr, ipv4_only)?;
    let sock = ops.socket(family).ok_or(TransportError::Socket)?;
    if !ops.set_reuseaddr(sock) {
        return Err(TransportError::Options);
    }
    if !ops.bind(sock, &bind_addr, port) {
        return Err(TransportError::Bind);
    }
    Ok(sock)
}

/// Creates a UDP socket bound to `src` and connected to `dest`.
pub fn udp_bind_connect<O: SocketOps + ?Sized>(
    ops: &mut O,
    src: Option<&str>,
    sport: u16,
    dest: &str,
    dport: u16,
) -> Result<u32, TransportError> {
    let dest_family = parse_family(dest).ok_or(TransportError::Unresolved)?;
    let sock = udp_bind(ops, src, sport, dest_family == IpFamily::V4)?;
    if !ops.connect(sock, dest, dport) {
        return Err(TransportError::Connect);
    }
    Ok(sock)
}

/// Connects a TCP socket, optionally binding it to `src` first.
pub fn tcp_connect_src<O: SocketOps + ?Sized>(
    ops: &mut O,
    dest: &str,
    port: u16,
    src: Option<&str>,
    ipv4_only: bool,
) -> Result<u32, TransportError> {
    let (family, _) = bind_address(Some(dest), ipv4_only)?;
    let sock = ops.socket(family).ok_or(TransportError::Socket)?;
    if !ops.set_reuseaddr(sock) {
        return Err(TransportError::Options);
    }
    if let Some(src) = src.filter(|value| !value.is_empty()) {
        let src_family = parse_family(src).ok_or(TransportError::Unresolved)?;
        if src_family == family && !ops.bind(sock, src, 0) {
            return Err(TransportError::Source);
        }
    }
    if !ops.connect(sock, dest, port) {
        return Err(TransportError::Connect);
    }
    Ok(sock)
}

/// Connects a TCP socket without a source address.
pub fn tcp_connect<O: SocketOps + ?Sized>(
    ops: &mut O,
    dest: &str,
    port: u16,
    ipv4_only: bool,
) -> Result<u32, TransportError> {
    tcp_connect_src(ops, dest, port, None, ipv4_only)
}

/// Gets the local address of a socket.
pub fn get_sock_shost<O: SocketOps + ?Sized>(ops: &O, sock: u32) -> Option<String> {
    ops.local_host(sock)
}

/// Gets the local port of a socket.
pub fn get_sock_sport<O: SocketOps + ?Sized>(ops: &O, sock: u32) -> Option<u16> {
    ops.local_port(sock)
}

fn parse_family(addr: &str) -> Option<IpFamily> {
    match addr.parse::<IpAddr>().ok()? {
        IpAddr::V4(_) => Some(IpFamily::V4),
        IpAddr::V6(_) => Some(IpFamily::V6),
    }
}

fn bind_address(addr: Option<&str>, ipv4_only: bool) -> Result<(IpFamily, String), TransportError> {
    match addr {
        Some(addr) => {
            let family = parse_family(addr).ok_or(TransportError::Unresolved)?;
            Ok((family, addr.to_owned()))
        }
        None if ipv4_only => Ok((IpFamily::V4, "0.0.0.0".to_owned())),
        None => Ok((IpFamily::V6, "::".to_owned())),
    }
}
