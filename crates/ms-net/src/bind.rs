//! `--bind-dev` validation, resolution and socket-restriction checks.

use std::fmt;

/// Size of a Linux interface name including the terminating NUL (`IFNAMSIZ`).
pub const IF_NAMESIZE: usize = 16;
/// Size of a dotted-quad IPv4 string including the terminating NUL.
pub const INET_ADDRSTRLEN: usize = 16;

/// Why a device name or bind option could not be resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindDevError {
    /// No device name was supplied.
    MissingDevice,
    /// The device name was empty.
    EmptyDevice,
    /// The name does not fit an interface name buffer.
    DeviceTooLong,
    /// The name contains a separator or whitespace.
    InvalidDevice,
    /// The output buffer is too small for a dotted-quad address.
    BufferTooSmall,
    /// The device has no IPv4 address or no kernel lookup was available.
    NoAddress,
    /// A device and an explicit address were requested together.
    Conflict,
}

impl fmt::Display for BindDevError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::MissingDevice => "missing device",
            Self::EmptyDevice => "empty device",
            Self::DeviceTooLong => "device name too long",
            Self::InvalidDevice => "invalid device name",
            Self::BufferTooSmall => "address buffer too small",
            Self::NoAddress => "device has no IPv4 address",
            Self::Conflict => "--bind-dev cannot be combined with --bind or --bind-http",
        };
        f.write_str(message)
    }
}

impl std::error::Error for BindDevError {}

/// Source of interface addresses; the real implementation lives in `sys`.
pub trait DevLookup {
    /// Returns the IPv4 address of `dev`, if the interface exists.
    fn dev_ip(&self, dev: &str) -> Option<String>;
}

/// Checks `--bind-dev` syntax: non-empty, fits `IFNAMSIZ`, no separators.
pub fn validate_bind_dev(dev: Option<&str>) -> bool {
    let Some(dev) = dev else {
        return false;
    };
    !dev.is_empty()
        && dev.len() < IF_NAMESIZE
        && !dev
            .chars()
            .any(|ch| matches!(ch, '/' | ' ' | '\t' | '\r' | '\n'))
}

/// Resolves a device address through `lookup`.
pub fn get_dev_ip<L: DevLookup + ?Sized>(
    lookup: &L,
    dev: Option<&str>,
    buf_len: usize,
) -> Result<String, BindDevError> {
    let dev = dev.ok_or(BindDevError::MissingDevice)?;
    if dev.is_empty() {
        return Err(BindDevError::EmptyDevice);
    }
    if buf_len < INET_ADDRSTRLEN {
        return Err(BindDevError::BufferTooSmall);
    }
    if dev.len() >= IF_NAMESIZE {
        return Err(BindDevError::DeviceTooLong);
    }
    lookup.dev_ip(dev).ok_or(BindDevError::NoAddress)
}

/// Bind-related options plus the IP derived from `--bind-dev`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BindOptions {
    /// Address for listening sockets.
    pub bind: Option<String>,
    /// Address for HTTP sockets.
    pub bind_http: Option<String>,
    /// Restrict sockets to this interface.
    pub bind_dev: Option<String>,
    derived_dev_ip: Option<String>,
}

impl BindOptions {
    /// Creates options, normalising empty strings to `None`.
    pub fn new(bind: Option<&str>, bind_http: Option<&str>, bind_dev: Option<&str>) -> Self {
        Self {
            bind: normalize(bind),
            bind_http: normalize(bind_http),
            bind_dev: normalize(bind_dev),
            derived_dev_ip: None,
        }
    }

    /// IP derived from `--bind-dev`, used as source address for joins.
    pub fn bind_dev_ip(&self) -> Option<&str> {
        match self.bind_dev.as_deref() {
            Some(dev) if !dev.is_empty() => self.bind.as_deref(),
            _ => None,
        }
    }
}

/// Resolves `--bind-dev` into `bind`/`bind_http`, rejecting address mixes.
pub fn resolve_bind_opts<L: DevLookup + ?Sized>(
    opts: &mut BindOptions,
    lookup: &L,
) -> Result<(), BindDevError> {
    opts.bind = normalize(opts.bind.as_deref());
    opts.bind_http = normalize(opts.bind_http.as_deref());
    opts.bind_dev = normalize(opts.bind_dev.as_deref());

    let derived = opts.derived_dev_ip.as_deref();
    let has_bind = opts.bind.is_some() && opts.bind.as_deref() != derived;
    let has_http = opts.bind_http.is_some() && opts.bind_http.as_deref() != derived;
    let has_dev = opts.bind_dev.is_some();

    if has_dev && (has_bind || has_http) {
        return Err(BindDevError::Conflict);
    }
    if has_dev {
        let dev = opts.bind_dev.as_deref().unwrap_or_default();
        let ip = get_dev_ip(lookup, Some(dev), INET_ADDRSTRLEN)?;
        opts.derived_dev_ip = Some(ip.clone());
        opts.bind = Some(ip.clone());
        opts.bind_http = Some(ip);
    } else if has_bind && !has_http {
        opts.bind_http = opts.bind.clone();
    }
    Ok(())
}

/// Result of applying `SO_BINDTODEVICE` to a socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindDevStatus {
    /// No device restriction was requested.
    Unset,
    /// The kernel accepted the restriction.
    Applied,
    /// The restriction was unavailable (permissions or platform).
    Degraded,
    /// Applying the restriction failed.
    Failed,
}

impl BindDevStatus {
    /// C++ `set_socket_bind_dev` return code: 0 applied/unset, 1 degraded, -1 error.
    pub fn code(self) -> i32 {
        match self {
            Self::Unset | Self::Applied => 0,
            Self::Degraded => 1,
            Self::Failed => -1,
        }
    }
}

/// Applies a device restriction; implemented for real sockets in `sys`.
pub trait BindDevOps {
    /// Restricts `self` to `dev`.
    fn set_bind_dev(&mut self, dev: &str) -> BindDevStatus;
}

/// Applies `--bind-dev` to a socket, rejecting over-long names first.
pub fn set_socket_bind_dev<O: BindDevOps + ?Sized>(
    sock: &mut O,
    bind_dev: Option<&str>,
) -> BindDevStatus {
    match bind_dev {
        None | Some("") => BindDevStatus::Unset,
        Some(dev) if dev.len() >= IF_NAMESIZE => BindDevStatus::Failed,
        Some(dev) => sock.set_bind_dev(dev),
    }
}

/// Reports the device a socket is restricted to; implemented in `sys`.
pub trait BindDevQuery {
    /// Returns the bound device name, or `None` when the query failed.
    fn bound_device(&self) -> Option<String>;
}

/// Compares a socket's restriction with the configured device.
pub fn socket_bind_dev_ok<Q: BindDevQuery + ?Sized>(sock: &Q, bind_dev: Option<&str>) -> bool {
    match bind_dev {
        None | Some("") => true,
        Some(dev) => sock
            .bound_device()
            .is_some_and(|bound| c_str_eq(dev.as_bytes(), bound.as_bytes())),
    }
}

fn c_str_eq(left: &[u8], right: &[u8]) -> bool {
    let left = left.split(|byte| *byte == 0).next().unwrap_or(&[]);
    let right = right.split(|byte| *byte == 0).next().unwrap_or(&[]);
    left == right
}

fn normalize(value: Option<&str>) -> Option<String> {
    value.filter(|text| !text.is_empty()).map(str::to_owned)
}
