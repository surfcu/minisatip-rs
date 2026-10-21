//! Linux FFI for interface lookup and `SO_BINDTODEVICE`; unsafe is confined here.
#![allow(unsafe_code)]

use crate::bind::{BindDevOps, BindDevQuery, BindDevStatus, DevLookup, IF_NAMESIZE};
use std::io;
use std::net::Ipv4Addr;
use std::os::fd::RawFd;
use std::time::{SystemTime, UNIX_EPOCH};

const SO_BINDTODEVICE: libc::c_int = 25;

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

/// `localtime_r()` fields as `asctime` prints them:
/// `(wday, mon, mday, hour, min, sec, year)` (minisatip.cpp:1155-1158).
pub fn local_time_fields(time: SystemTime) -> Option<(i32, i32, i32, i32, i32, i32, i64)> {
    let seconds = time.duration_since(UNIX_EPOCH).ok()?.as_secs();
    let stamp = libc::time_t::try_from(seconds).ok()?;
    // SAFETY: localtime_r writes into `tm` and returns it, or NULL on failure.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let result = unsafe { libc::localtime_r(&stamp, &mut tm) };
    if result.is_null() {
        return None;
    }
    Some((
        tm.tm_wday,
        tm.tm_mon,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec,
        i64::from(tm.tm_year) + 1900,
    ))
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn local_time_fields_match_localtime_r() {
        let time = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let fields = local_time_fields(time).expect("localtime_r succeeds");
        let stamp = 1_700_000_000 as libc::time_t;
        // SAFETY: localtime_r writes into caller-owned `tm` storage.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        let result = unsafe { libc::localtime_r(&stamp, &mut tm) };
        assert!(!result.is_null());
        assert_eq!(
            fields,
            (
                tm.tm_wday,
                tm.tm_mon,
                tm.tm_mday,
                tm.tm_hour,
                tm.tm_min,
                tm.tm_sec,
                i64::from(tm.tm_year) + 1900
            )
        );
    }
}
